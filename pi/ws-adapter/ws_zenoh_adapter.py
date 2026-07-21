#!/usr/bin/env python3
"""Pi-side WS-JSON adapter — the browser edge of the Zenoh hub, the Python sibling
of the ESP firmware's ws_zenoh_bridge.c. A browser can't speak native Zenoh, so
the dashboard speaks this small JSON op protocol over one WebSocket and the
adapter maps it onto a zenoh session beside zenohd:

    {op:"sub",   key}           declare a per-client key filter
    {op:"unsub", key}
    {op:"pub",   key, val}      session.put   (fleet/estop gated on auth)
    {op:"get",   key, val, id}  session.get   -> {op:"reply", id, val}
    {op:"auth",  password}      -> {op:"auth", ok}
    hub -> client: {key, val}   a delivered subscription sample

The hub owns the fleet/estop latch: an authed estop pub updates it and a
queryable answers a (re)joining robot's join-time get.

Config via env:
  ZENOH_CONNECT   tcp/<zenohd-host>:7447  (client mode; production = beside zenohd)
  ZENOH_LISTEN    tcp/127.0.0.1:7447      (peer mode with a listen endpoint; local test)
  WS_PORT         9001
  OPERATOR_PASS the one gated identity (default "change-me")
"""
import asyncio, json, os, sys
import zenoh
import websockets

WS_PORT = int(os.environ.get("WS_PORT", "9001"))
ZENOH_CONNECT = os.environ.get("ZENOH_CONNECT", "")
ZENOH_LISTEN = os.environ.get("ZENOH_LISTEN", "")
# The one gated identity — its only power is engaging/clearing fleet/estop
# (pi/CLAUDE.md § Permissions: everything under robots/** is open by design, the
# Wi-Fi perimeter is the boundary). A silent default would make that gate
# meaningless, so an unset password is a loud startup warning, never a quiet
# fallback — a deploy that skips it fails visibly, not open. (Mirrors the ESP
# hub's compile-time default + the Pi's install.sh hub-passwd placeholder: a
# placeholder to rotate, announced as one.)
_OPERATOR_PASS_ENV = os.environ.get("OPERATOR_PASS", "")
OPERATOR_PASS = _OPERATOR_PASS_ENV or "change-me"

clients = []             # each: {"ws","subs":set,"authed":bool,"client_id":str,"queue":asyncio.Queue}
estop_latched = False
# Per-owner robot isolation (hub#10) — the Python mirror of ws_zenoh_bridge.c.
# Ownership lives ONLY here (never on the wire), keyed by robot name:
#   robots[id] = {"owner": clientId or "", "claimable_until": loop-monotonic seconds}
# A slot appears only from a robot's own claimable announce, so a claim can only
# land on a robot that physically opened its BOOT-tap window.
robots = {}
loop = None
session = None


def build_config():
    conf = zenoh.Config()
    if ZENOH_CONNECT:
        conf.insert_json5("mode", '"client"')
        conf.insert_json5("connect/endpoints", json.dumps([ZENOH_CONNECT]))
    else:
        conf.insert_json5("mode", '"peer"')
        if ZENOH_LISTEN:
            conf.insert_json5("listen/endpoints", json.dumps([ZENOH_LISTEN]))
    return conf


# ---- per-owner isolation helpers (hub#10, mirroring ws_zenoh_bridge.c) --------
def note_claimable(key, valobj):
    # key = robots/<id>/claimable — a BOOT-tap announce extends the window a few
    # seconds; no cross-device clock, just "recently announced".
    parts = key.split("/")
    if len(parts) < 3:
        return
    rid = parts[1]
    open_ = not (isinstance(valobj, dict) and valobj.get("open") is False)
    r = robots.setdefault(rid, {"owner": "", "claimable_until": 0.0})
    r["claimable_until"] = (loop.time() + 4.0) if open_ else 0.0


def is_stop(valobj):
    # A zero-drive pwm is honored for everyone regardless of ownership.
    if not isinstance(valobj, dict):
        return False
    return (valobj.get("left_motor") or 0) == 0 and (valobj.get("right_motor") or 0) == 0


def ownership_ok(c, key, valobj):
    # Only drive channels (pwm, cmd/*) of a *claimed* robot are gated; the rest —
    # fleet/pair namespaces, led queries, an unclaimed robot — stay open.
    if not key.startswith("robots/"):
        return True
    parts = key.split("/", 2)
    if len(parts) < 3:
        return True
    rid, chan = parts[1], parts[2]
    if chan != "pwm" and not chan.startswith("cmd/"):
        return True
    r = robots.get(rid)
    if not r or not r["owner"]:
        return True                       # unclaimed → open
    if c["authed"]:
        return True                       # operator override
    if r["owner"] == c["client_id"]:
        return True                       # the owner
    return chan == "pwm" and is_stop(valobj)   # a stop is for everyone


def owner_state_for(r, c):
    # The owner clientId is a bearer token (presenting it proves ownership at the
    # gate), so it never leaves the adapter — each client is told only whether a
    # robot is theirs, held by another, or free, never *who* holds it. A
    # broadcast token could be copied off a socket and replayed to impersonate.
    if not r["owner"]:
        return "free"
    return "mine" if r["owner"] == c["client_id"] else "held"


def broadcast_owner(rid):
    r = robots.get(rid)
    if not r:
        return
    for c in list(clients):
        c["queue"].put_nowait(json.dumps({"op": "owner", "id": rid, "state": owner_state_for(r, c)}))


def owners_frame_for(c):
    mine = [rid for rid, r in robots.items() if r["owner"] and r["owner"] == c["client_id"]]
    held = [rid for rid, r in robots.items() if r["owner"] and r["owner"] != c["client_id"]]
    return json.dumps({"op": "owners", "mine": mine, "held": held})


# ---- zenoh sample -> matching WS clients (runs on a zenoh thread) ------------
def on_sample(sample):
    key = str(sample.key_expr)
    raw = sample.payload.to_bytes().decode(errors="replace")
    try:
        valobj = json.loads(raw)
    except ValueError:
        valobj = raw
    if key.endswith("/claimable"):
        loop.call_soon_threadsafe(note_claimable, key, valobj)
    frame = json.dumps({"key": key, "val": valobj})
    for c in list(clients):
        for pat in list(c["subs"]):
            try:
                if zenoh.KeyExpr(pat).intersects(zenoh.KeyExpr(key)):
                    loop.call_soon_threadsafe(c["queue"].put_nowait, frame)
                    break
            except Exception:
                pass


def on_estop_query(query):
    body = json.dumps({"engaged": bool(estop_latched)})
    query.reply("fleet/estop", body.encode())


# ---- per-client outbound pump -----------------------------------------------
async def client_sender(c):
    while True:
        frame = await c["queue"].get()
        try:
            await c["ws"].send(frame)
        except Exception:
            break


async def handle_op(c, text):
    global estop_latched
    try:
        msg = json.loads(text)
    except ValueError:
        return
    op, key = msg.get("op"), msg.get("key")
    if op == "sub" and key:
        c["subs"].add(key)
    elif op == "unsub" and key:
        c["subs"].discard(key)
    elif op == "pub" and key:
        val = msg.get("val")
        if key == "fleet/estop" and not c["authed"]:
            await c["ws"].send(json.dumps({"op": "error", "reason": "estop requires operator auth"}))
        elif val is not None and not ownership_ok(c, key, val):
            await c["ws"].send(json.dumps({"op": "error", "reason": "robot claimed by another student"}))
        elif val is not None:
            if key == "fleet/estop" and isinstance(val, dict):
                estop_latched = val.get("engaged") is not False   # missing/true => engaged
            session.put(key, json.dumps(val).encode())
    elif op == "get" and key:
        val, gid = msg.get("val"), msg.get("id", "")

        def on_reply(reply, gid=gid, c=c):
            try:
                if reply.ok is not None:
                    payload = reply.ok.payload.to_bytes().decode(errors="replace")
                    frame = json.dumps({"op": "reply", "id": gid, "val": json.loads(payload)})
                    loop.call_soon_threadsafe(c["queue"].put_nowait, frame)
            except Exception:
                pass

        if val is not None:
            session.get(key, handler=on_reply, payload=json.dumps(val).encode(), timeout=4.0)
        else:
            session.get(key, handler=on_reply, timeout=4.0)
    elif op == "auth":
        c["authed"] = msg.get("password") == OPERATOR_PASS
        await c["ws"].send(json.dumps({"op": "auth", "ok": c["authed"]}))
    elif op == "hello":
        # The dashboard's opaque persistent identity (localStorage) — the owner
        # key, bound once per connection so a refresh keeps its claims.
        cid = msg.get("clientId")
        if isinstance(cid, str):
            c["client_id"] = cid
        await c["ws"].send(owners_frame_for(c))
    elif op == "claim":
        # Presence-gated: only lands during a live BOOT-tap window, consumed on
        # the first claim.
        rid = msg.get("id")
        r = robots.get(rid) if isinstance(rid, str) else None
        if r and loop.time() < r["claimable_until"]:
            r["owner"] = c["client_id"]
            r["claimable_until"] = 0.0
            broadcast_owner(rid)
        else:
            await c["ws"].send(json.dumps({"op": "error", "reason": "press the robot's BOOT button first"}))
    elif op == "release":
        # The owner, or the operator (master override), may release.
        rid = msg.get("id")
        r = robots.get(rid) if isinstance(rid, str) else None
        if r and (c["authed"] or r["owner"] == c["client_id"]):
            r["owner"] = ""
            broadcast_owner(rid)


async def ws_handler(ws):
    c = {"ws": ws, "subs": set(), "authed": False, "client_id": "", "queue": asyncio.Queue()}
    clients.append(c)
    sender = asyncio.create_task(client_sender(c))
    try:
        async for text in ws:
            await handle_op(c, text)
    finally:
        if c in clients: clients.remove(c)
        sender.cancel()


async def main():
    global loop, session
    if not _OPERATOR_PASS_ENV:
        print("[ws-adapter] WARNING: OPERATOR_PASS is unset — using the well-known "
              "default 'change-me'. The e-stop operator gate is meaningless with a "
              "public default; set OPERATOR_PASS to your classroom code before any "
              "real deployment.", file=sys.stderr, flush=True)
    loop = asyncio.get_running_loop()
    session = zenoh.open(build_config())
    session.declare_subscriber("**", on_sample)
    session.declare_queryable("fleet/estop", on_estop_query)
    where = f"client -> {ZENOH_CONNECT}" if ZENOH_CONNECT else f"peer (listen {ZENOH_LISTEN or 'multicast'})"
    async with websockets.serve(ws_handler, "0.0.0.0", WS_PORT):
        print(f"WS-JSON adapter on :{WS_PORT}, zenoh {where}", flush=True)
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
