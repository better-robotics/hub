#!/usr/bin/env python3
"""hub_mcp.py — drive the classroom fleet from an LLM (the MCP server).

The bench tool. It talks to the hub's WS-JSON adapter as the operator, so every
drive command flows through the one adapter edge that the router ACL trusts (the
MQTT bridge it replaced was retired in the Zenoh cutover).

**Transport: the WS-JSON adapter, as an operator — NOT raw Zenoh (hub#10 step 5).**
The bridge is a drive path, and on the Pi `zenohd` routes, so a raw client could
inject drive without passing the hub's per-owner gate. Rather than carve the
bridge out of the router ACL (static + deny-precedence makes "everyone on the AP
except this one client" unexpressible cleanly, and the bench runs this remotely
from a laptop, not on the Pi), the bridge speaks the same WS-JSON op protocol the
browser dashboard does and authenticates as the operator. So **all drive flows
through the one adapter edge on both tiers**: the router ACL just denies raw
non-loopback drive, and per-owner isolation + the operator override live in
exactly one place (the adapter). Same FastMCP tools, same envelopes, same
`robots/<id>/<channel>` keys — only the wire under `_put`/subscribe changes:

    op           frame                              was (raw Zenoh)
    ----------   --------------------------------   ------------------------------
    pub          {op:pub,  key, val}                session.put
    sub / unsub  {op:sub,  key} / {op:unsub, key}   declare_subscriber
    get          {op:get,  id, key, val} -> reply   session.get  (robot queryable)
    auth         {op:auth, password}     -> {ok}    (n/a — raw had no hub auth)
    hello        {op:hello, clientId}               (n/a)

Discovery: HUB_HOST names the adapter host (the hub's DHCP gateway, or a bench
IP); the adapter's WS port is the fixed 9001 the dashboard uses.

Run:
    pip install "mcp[cli]" websockets
    HUB_HOST=hub.local HUB_PASS=<operator code> python hub_mcp_zenoh.py
"""
from __future__ import annotations

import asyncio
import json
import os
import sys
import threading
import time
import uuid

import websockets
from mcp.server.fastmcp import FastMCP

# ---- config (env-driven) ----------------------------------------------------
HUB_HOST = os.environ.get("HUB_HOST", "127.0.0.1")           # the WS adapter host
WS_PORT = int(os.environ.get("HUB_WS_PORT", "9001"))         # fixed dashboard convention
HUB_USER = os.environ.get("HUB_USER", "operator")            # e-stop / override authority
HUB_PASS = os.environ.get("HUB_PASS", "")
CLIENT_ID = "mcp-" + uuid.uuid4().hex[:8]                    # opaque owner id (this bridge)
MOTOR_MAX = 255

# ---- live fabric state, kept fresh by the background WS receive loop ---------
_imu: dict[str, dict] = {}
_sys: dict[str, dict] = {}
_watchers: list[dict] = []

_loop: asyncio.AbstractEventLoop | None = None   # the background WS event loop
_ws = None                                       # current connection (owned by _loop)
_ready = threading.Event()                       # set while connected + subscribed
_authed = {"ok": False}
_replies: dict[str, dict] = {}                   # get-id -> {"val":…, "event":Event}
# Background feeds re-sent on every (re)connect — the always-on telemetry taps.
_FEEDS = ("robots/*/imu", "robots/*/sys")


def _keyexpr_from_mqtt(pattern: str) -> str:
    """MQTT wildcards -> Zenoh: `#` (rest) -> `**`, `+` (one level) -> `*`."""
    return "/".join("**" if p == "#" else "*" if p == "+" else p for p in pattern.split("/"))


def _key_matches(pattern_ke: str, key: str) -> bool:
    """Zenoh key-expr glob (`*`=one chunk, `**`=zero+ chunks). Local so the bridge
    carries no Zenoh dependency now that the wire is WS-JSON."""
    def seg(P, K):
        if not P:
            return not K
        if P[0] == "**":
            if len(P) == 1:
                return True
            return any(seg(P[1:], K[i:]) for i in range(len(K) + 1))
        if not K:
            return False
        return (P[0] == "*" or P[0] == K[0]) and seg(P[1:], K[1:])
    return seg(pattern_ke.split("/"), key.split("/"))


# ---- WS receive handling (runs on the background loop) ----------------------
def _handle(d: dict) -> None:
    op = d.get("op")
    if op == "auth":
        _authed["ok"] = bool(d.get("ok"))
    elif op == "reply":
        r = _replies.get(d.get("id"))
        if r:
            r["val"] = d.get("val")
            r["event"].set()
    elif op in ("owner", "owners", "error"):
        pass          # the bridge is the operator — ownership never gates it; errors are log-only
    elif d.get("key") is not None:
        _on_message(d["key"], d.get("val"))


def _on_message(key: str, body) -> None:
    for w in _watchers:
        if len(w["msgs"]) < w["cap"] and _key_matches(w["pattern"], key):
            w["msgs"].append({"topic": key, "payload": body, "t": round(time.time(), 3)})
    if not isinstance(body, dict):
        return
    parts = key.split("/")                       # robots/<id>/<channel>
    if len(parts) < 3:
        return
    robot_id, channel = parts[1], parts[2]
    rec = dict(body)
    rec["_rx"] = time.time()
    if channel == "imu":
        _imu[robot_id] = rec
    elif channel == "sys":
        # Key by BOARD, not topic id: every pool board publishes on
        # robots/unassigned/sys, so keying by topic collapses them into one
        # flapping entry. The topic id rides along as the board's assigned name.
        rec["_name"] = robot_id
        _sys[rec.get("board") or robot_id] = rec


async def _ws_loop() -> None:
    """Persistent WS connection with reconnect: hello + operator auth, resubscribe
    the telemetry feeds, then pump inbound frames. Owns the socket for the life of
    the process; the sync MCP tools post sends to it via run_coroutine_threadsafe."""
    global _ws
    url = f"ws://{HUB_HOST}:{WS_PORT}/"
    while True:
        try:
            async with websockets.connect(url) as ws:
                _ws = ws
                await ws.send(json.dumps({"op": "hello", "clientId": CLIENT_ID}))
                if HUB_PASS:
                    await ws.send(json.dumps({"op": "auth", "role": HUB_USER, "password": HUB_PASS}))
                for ke in _FEEDS:
                    await ws.send(json.dumps({"op": "sub", "key": ke}))
                _ready.set()
                async for raw in ws:
                    try:
                        _handle(json.loads(raw))
                    except (ValueError, TypeError):
                        pass
        except Exception:                        # noqa: BLE001 — any drop -> reconnect
            pass
        _ready.clear()
        _ws = None
        _authed["ok"] = False
        await asyncio.sleep(1.0)


def _send(frame: dict) -> None:
    if _loop is None or _ws is None:
        raise RuntimeError(
            f"not connected to the hub adapter — is it running, and are you on its "
            f"Wi-Fi? (ws://{HUB_HOST}:{WS_PORT})")
    asyncio.run_coroutine_threadsafe(_ws.send(json.dumps(frame)), _loop).result(timeout=3)


def _clamp(v: int) -> int:
    return max(-MOTOR_MAX, min(MOTOR_MAX, int(v)))


def _clamp8(v: int) -> int:
    return max(0, min(255, int(v)))


def _clean(d: dict) -> dict:
    return {k: v for k, v in d.items() if not k.startswith("_")}


def _put(key: str, body: dict) -> None:
    _send({"op": "pub", "key": key, "val": body})


# ---- MCP tools --------------------------------------------------------------
mcp = FastMCP("hub-fleet")


@mcp.tool()
def drive(robot_id: str, left_motor: int, right_motor: int, duration_ms: int = 400) -> str:
    """Drive a robot: signed PWM per side, magnitude 0..255, sign sets direction
    (positive = forward, negative = reverse). Auto-expires after duration_ms —
    firmware stops the motors when it lapses. Publishes robots/<id>/pwm."""
    body = {
        "timestamp": time.time(),
        "left_motor": _clamp(left_motor),
        "right_motor": _clamp(right_motor),
        "duration_ms": max(0, int(duration_ms)),
    }
    _put(f"robots/{robot_id}/pwm", body)
    return f"drive {robot_id}: L={body['left_motor']} R={body['right_motor']} for {body['duration_ms']}ms"


@mcp.tool()
def stop(robot_id: str) -> str:
    """Immediately halt a robot (zero PWM, zero duration). Publishes robots/<id>/pwm.
    Transient and per-robot — for a room-wide halt that STAYS engaged, use estop()."""
    _put(f"robots/{robot_id}/pwm",
         {"timestamp": time.time(), "left_motor": 0, "right_motor": 0, "duration_ms": 0})
    return f"stop {robot_id}"


@mcp.tool()
def estop(engaged: bool = True, reason: str = "") -> str:
    """Fleet-wide EMERGENCY STOP latch (CONTRACT.md § Fleet e-stop). engaged=True
    halts every robot and makes them refuse drive until estop(engaged=False).
    Published on fleet/estop; the hub holds the latch (Pi storage / ESP queryable)
    and answers a rebooting robot's join-time get, so the stop survives reconnects.
    The e-stop authority is the operator, enforced at the hub — this bridge must
    have authed (HUB_PASS) or the adapter refuses the write."""
    body: dict = {"timestamp": time.time(), "engaged": engaged, "by": HUB_USER}
    if reason:
        body["reason"] = reason
    _put("fleet/estop", body)
    return ("E-STOP ENGAGED — fleet halted and latched (clear with estop(engaged=False))"
            if engaged else "e-stop cleared — fleet released")


@mcp.tool()
def read_imu(robot_id: str, timeout_s: float = 2.0) -> dict:
    """Latest IMU sample for a robot: accel_x/y/z, gyro_x/y/z. Waits up to
    timeout_s for a sample newer than this call. Reads robots/<id>/imu."""
    start = time.time()
    deadline = start + timeout_s
    while time.time() < deadline:
        sample = _imu.get(robot_id)
        if sample and sample.get("_rx", 0) >= start:
            return _clean(sample)
        time.sleep(0.02)
    cached = _imu.get(robot_id)
    if cached:
        return {**_clean(cached), "stale": True}
    return {"error": f"no IMU seen for {robot_id}", "hint": "check robot_id and that the robot is publishing"}


@mcp.tool()
def fleet() -> dict:
    """Every board currently on the hub, keyed by hardware board id, each with its
    assigned identity, latest sys telemetry, and seconds-since-last-message.
    Reads robots/*/sys — open to anyone on the hub's Wi-Fi."""
    now = time.time()
    return {
        board: {**_clean(payload), "name": payload.get("_name", "?"),
                "age_s": round(now - payload.get("_rx", now), 1)}
        for board, payload in _sys.items()
    }


@mcp.tool()
def set_led(robot_id: str, on: bool, red: int = 0, green: int = 0, blue: int = 0,
            timeout_s: float = 1.5) -> dict:
    """Set a robot's RGB LED and wait for its ack. A get() on the robot's queryable
    at robots/<id>/led carries the request over the adapter and returns the reply
    ({status:ok} / {status:error,message})."""
    req = {"method": "set_led", "on": bool(on),
           "red": _clamp8(red), "green": _clamp8(green), "blue": _clamp8(blue)}
    gid = uuid.uuid4().hex[:8]
    ev = threading.Event()
    _replies[gid] = {"val": None, "event": ev}
    try:
        _send({"op": "get", "id": gid, "key": f"robots/{robot_id}/led", "val": req})
    except RuntimeError as e:
        _replies.pop(gid, None)
        return {"status": "error", "acked": False, "message": str(e)}
    got = ev.wait(timeout=timeout_s + 0.5)
    val = _replies.pop(gid, {}).get("val")
    if got and isinstance(val, dict):
        return {"acked": True, **val}
    return {"status": "sent", "acked": False,
            "note": "no reply within timeout — is the robot declaring a robots/<id>/led queryable?"}


# ---- wire primitives ---------------------------------------------------------

@mcp.tool()
def publish(topic: str, payload: dict) -> str:
    """Publish a JSON payload to any key (e.g. robots/robot3/pwm). robots/** and
    pair/** are open to everyone on the hub's Wi-Fi. Use watch() to confirm it landed."""
    _put(topic, payload)
    return f"published to {topic}: {json.dumps(payload)}"


@mcp.tool()
def watch(topic_pattern: str = "robots/#", duration_s: float = 5.0, max_messages: int = 50) -> dict:
    """Subscribe to a key pattern (MQTT-style wildcards accepted: + one level, #
    rest — mapped to Zenoh * / **) and collect live messages for duration_s.
    Returns {topic, payload, t} per message, oldest first."""
    duration_s = min(max(duration_s, 0.1), 30.0)
    ke = _keyexpr_from_mqtt(topic_pattern)
    tap = {"pattern": ke, "msgs": [], "cap": max(1, min(int(max_messages), 200))}
    _watchers.append(tap)
    _send({"op": "sub", "key": ke})
    try:
        deadline = time.time() + duration_s
        while time.time() < deadline and len(tap["msgs"]) < tap["cap"]:
            time.sleep(0.05)
    finally:
        _watchers.remove(tap)
        try:
            _send({"op": "unsub", "key": ke})
        except RuntimeError:
            pass
    out = {"messages": tap["msgs"], "count": len(tap["msgs"])}
    if not tap["msgs"]:
        out["note"] = "nothing seen — no publisher on that pattern right now"
    return out


def _board_name(board: str) -> str | None:
    rec = _sys.get(board)
    return rec.get("_name") if rec else None


@mcp.tool()
def blink(board: str) -> str:
    """Blink a board's LED for ~6 s so a human can find the physical robot.
    Targets the board through its assigned topic. Writes robots/<name>/cmd/identify."""
    name = _board_name(board)
    if not name:
        return f"unknown board {board} — call fleet() to see who's online"
    _put(f"robots/{name}/cmd/identify", {"target": board})
    return f"blink sent to {board} (via robots/{name}/cmd/identify) — watch the desk"


@mcp.tool()
def assign(board: str, name: str, hub_pin: str = "") -> dict:
    """(Re)assign a board to a name — the topic id it publishes/listens under.
    Optional hub_pin locks the board to one hub SSID ('-' clears). Writes
    robots/<cur>/cmd/config; the robot saves to NVS and reboots under the name."""
    cur = _board_name(board)
    if not cur:
        return {"error": f"unknown board {board} — call fleet() to see who's online"}
    cfg: dict = {"target": board, "name": name}
    if hub_pin == "-":
        cfg["hub"] = ""
    elif hub_pin:
        cfg["hub"] = hub_pin
    _put(f"robots/{cur}/cmd/config", cfg)
    return {"sent": f"{board} ({cur}) -> {name}", "note": "reboots and reappears in a few seconds"}


@mcp.tool()
def flip(board: str, direction: str) -> dict:
    """Fix a robot driving the wrong way without rewiring: 'left', 'right', or
    'swap'. Permutes the stored motor pins in NVS; the robot reboots with the fix."""
    if direction not in ("left", "right", "swap"):
        return {"error": "direction must be left, right, or swap"}
    name = _board_name(board)
    if not name:
        return {"error": f"unknown board {board} — call fleet() to see who's online"}
    _put(f"robots/{name}/cmd/config", {"target": board, "flip": {direction: True}})
    return {"sent": f"flip {direction} -> {board}", "note": "reboots with the fix in a few seconds"}


# ---- session lifecycle ------------------------------------------------------

def connect(timeout_s: float = 8.0) -> None:
    """Start the background WS loop and wait until it is connected + subscribed.
    Callable from a test harness; main() calls it before serving MCP."""
    global _loop
    _loop = asyncio.new_event_loop()

    def _run() -> None:
        asyncio.set_event_loop(_loop)
        _loop.run_until_complete(_ws_loop())

    threading.Thread(target=_run, name="hub-ws", daemon=True).start()
    _ready.wait(timeout=timeout_s)   # best-effort; tools raise a clear error if still down


def main() -> None:
    if not HUB_PASS:
        print("[hub_mcp_zenoh] no HUB_PASS — operator actions (e-stop, and driving a "
              "CLAIMED robot) will be refused by the hub; unclaimed robots still drive.",
              file=sys.stderr)
    connect()
    mcp.run()


if __name__ == "__main__":
    main()
