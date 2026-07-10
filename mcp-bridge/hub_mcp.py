#!/usr/bin/env python3
"""hub_mcp.py — an MCP server that lets an LLM drive the classroom fleet.

This is the *hub-side* answer to "can Claude Code run on the ESP32": it can't
(the CLI is a native Linux/macOS binary; the rover is a 400 KB-SRAM MCU running
FreeRTOS). Instead Claude Code runs on the hub appliance and reaches the rovers
over this fabric — an MCP tool server that speaks the same MQTT contract the
dashboard and firmware already use.

Identity lives in the topic (`robots/<id>/<channel>`), never the body — the
envelopes mirror `protocol/envelopes/*.json`:

    robots/<id>/pwm        drive       {timestamp, left_motor, right_motor, duration_ms}
    robots/<id>/imu        telemetry   {timestamp, accel_x..z, gyro_x..z}
    robots/<id>/sys        fleet       read-only presence/telemetry
    robots/<id>/led        set_led req / robots/<id>/led/reply resp (MQTT5 correlation)

It connects to Mosquitto as whatever identity HUB_USER/HUB_PASS name — the
broker ACL scopes everything (see mosquitto-acl.example.conf). With NO
credential it connects anonymous (read-only fleet view) and `request_access()`
pairs it in-chat: knock on hubd's access gate, a human approves from a browser
(the team's own signed-in dashboard for an existing team, the professor for a
new name), and this session reconnects with the delivered code. It is the
first real MQTT *client* in this repo; hubd is deliberately not one, and
reprovision.py still stubs on hub#1.

Run:
    pip install "mcp[cli]" paho-mqtt        # or: uv pip install ...
    HUB_HOST=hub.local HUB_PASS=secret python hub_mcp.py
    HUB_HOST=hub.local python hub_mcp.py    # anonymous; pair via request_access()

Register with Claude Code (stdio):
    claude mcp add hub-fleet -- python /path/to/hub_mcp.py
See README.md for the .mcp.json form and the env knobs.
"""
from __future__ import annotations

import json
import os
import sys
import threading
import time
import uuid

import paho.mqtt.client as mqtt
from paho.mqtt.packettypes import PacketTypes
from paho.mqtt.properties import Properties
from mcp.server.fastmcp import FastMCP

# ---- config (env-driven; defaults match mosquitto.example.conf) -------------
HUB_HOST = os.environ.get("HUB_HOST", "localhost")
HUB_PORT = int(os.environ.get("HUB_PORT", "1883"))          # raw MQTT, not the :9001 WS port
HUB_USER = os.environ.get("HUB_USER", "professor")           # ACL identity; scope = this credential
HUB_PASS = os.environ.get("HUB_PASS", "")
# hubd's HTTP side (codes/requests management — Pi hub only; the ESP32 hub
# role has no /codes API and these tools will report that plainly).
HUB_HTTP = os.environ.get("HUB_HTTP", f"http://{HUB_HOST}")
MOTOR_MAX = 255                                              # 8-bit PWM magnitude; sign = direction

# ---- live fabric state, kept fresh by background subscriptions --------------
# GIL makes these plain-dict swaps atomic enough for this read/write pattern.
_imu: dict[str, dict] = {}       # robot_id -> latest IMU envelope (+ _rx wall-clock)
_sys: dict[str, dict] = {}       # board id -> latest sys envelope (+ _rx wall-clock, _team)
_replies: dict[str, dict] = {}   # correlation-id -> led/reply payload
_reply_event = threading.Event()
_watchers: list[dict] = []       # active watch() taps: {pattern, msgs, cap}

_client = mqtt.Client(
    mqtt.CallbackAPIVersion.VERSION2,
    client_id=f"hub-mcp-{uuid.uuid4().hex[:8]}",
    protocol=mqtt.MQTTv5,        # v5 needed for set_led request/reply correlation
)


def _on_connect(client, userdata, flags, reason_code, properties):
    # Subscribe on (re)connect so telemetry survives a broker bounce.
    client.subscribe("robots/+/imu")
    client.subscribe("robots/+/sys")
    client.subscribe("robots/+/led/reply")


def _on_message(client, userdata, msg):
    # Feed any active watch() taps first — they see every subscribed topic,
    # including ones the fixed channel handling below doesn't parse.
    for w in _watchers:
        if len(w["msgs"]) < w["cap"] and mqtt.topic_matches_sub(w["pattern"], msg.topic):
            try:
                body = json.loads(msg.payload)
            except (ValueError, UnicodeDecodeError):
                body = msg.payload.decode(errors="replace")
            w["msgs"].append({"topic": msg.topic, "payload": body, "t": round(time.time(), 3)})

    parts = msg.topic.split("/")            # robots/<id>/<channel>[/reply]
    if len(parts) < 3:
        return
    robot_id, channel = parts[1], parts[2]
    try:
        payload = json.loads(msg.payload)
    except (ValueError, UnicodeDecodeError):
        return
    payload["_rx"] = time.time()

    if channel == "imu":
        _imu[robot_id] = payload
    elif channel == "sys":
        # Key by BOARD, not topic id: every pool board publishes on
        # robots/unassigned/sys, so keying by topic collapses them into one
        # flapping entry (the same last-writer-wins the dashboard's per-board
        # pool rows fixed). The topic id rides along as the board's team.
        payload["_team"] = robot_id
        _sys[payload.get("board") or robot_id] = payload
    elif channel == "led" and len(parts) == 4 and parts[3] == "reply":
        cid = None
        props = msg.properties
        if props is not None and getattr(props, "CorrelationData", None):
            cid = props.CorrelationData.decode(errors="replace")
        if cid:
            _replies[cid] = payload
            _reply_event.set()


_client.on_connect = _on_connect
_client.on_message = _on_message


def _clamp(v: int) -> int:
    return max(-MOTOR_MAX, min(MOTOR_MAX, int(v)))


def _clamp8(v: int) -> int:
    return max(0, min(255, int(v)))


def _clean(d: dict) -> dict:
    # Drop internal bookkeeping (e.g. _rx wall-clock) before handing a payload
    # to the LLM — it sees the envelope fields plus whatever the tool derives.
    return {k: v for k, v in d.items() if not k.startswith("_")}


def _publish(topic: str, body: dict, properties: Properties | None = None) -> None:
    if not _client.is_connected():
        raise RuntimeError(
            f"not connected to the hub at {HUB_HOST}:{HUB_PORT} — are you on its Wi-Fi? "
            "(the connection keeps retrying in the background; try again in a few seconds)")
    info = _client.publish(topic, json.dumps(body), qos=0, properties=properties)
    info.wait_for_publish(timeout=2.0)


# ---- MCP tools --------------------------------------------------------------
mcp = FastMCP("hub-fleet")


@mcp.tool()
def drive(robot_id: str, left_motor: int, right_motor: int, duration_ms: int = 400) -> str:
    """Drive a rover: signed PWM per side, magnitude 0..255, sign sets direction
    (positive = forward, negative = reverse). The command auto-expires after
    duration_ms — firmware stops the motors when it lapses, so a dropped follow-up
    can't leave a rover running away. Publishes robots/<id>/pwm."""
    body = {
        "timestamp": time.time(),
        "left_motor": _clamp(left_motor),
        "right_motor": _clamp(right_motor),
        "duration_ms": max(0, int(duration_ms)),
    }
    _publish(f"robots/{robot_id}/pwm", body)
    return f"drive {robot_id}: L={body['left_motor']} R={body['right_motor']} for {body['duration_ms']}ms"


@mcp.tool()
def stop(robot_id: str) -> str:
    """Immediately halt a rover (zero PWM, zero duration). Publishes robots/<id>/pwm."""
    _publish(f"robots/{robot_id}/pwm",
             {"timestamp": time.time(), "left_motor": 0, "right_motor": 0, "duration_ms": 0})
    return f"stop {robot_id}"


@mcp.tool()
def read_imu(robot_id: str, timeout_s: float = 2.0) -> dict:
    """Latest IMU sample for a rover: accel_x/y/z, gyro_x/y/z (epoch-seconds
    timestamp). Waits up to timeout_s for a sample *newer than this call* so a
    reading reflects the rover's state now, not a stale cache. Reads robots/<id>/imu."""
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
    return {"error": f"no IMU seen for {robot_id}", "hint": "check robot_id and that the rover is publishing"}


@mcp.tool()
def fleet() -> dict:
    """Every board currently on the hub, keyed by hardware board id, each with
    its team (the topic identity it publishes under — `unassigned` = the pool),
    latest sys telemetry, and seconds-since-last-message. This is the anonymous
    public view (robots/+/sys) — the same data the dashboard's fleet cards show.
    To drive a board, target its TEAM id (pool boards all share `unassigned`)."""
    now = time.time()
    return {
        board: {**_clean(payload), "team": payload.get("_team", "?"),
                "age_s": round(now - payload.get("_rx", now), 1)}
        for board, payload in _sys.items()
    }


@mcp.tool()
def set_led(robot_id: str, on: bool, red: int = 0, green: int = 0, blue: int = 0,
            timeout_s: float = 1.5) -> dict:
    """Set a rover's RGB LED and wait for its ack. Sends an MQTT5 request on
    robots/<id>/led (response-topic + correlation-data) and awaits the reply on
    robots/<id>/led/reply. NOTE: the firmware-side reply is not wired yet
    (hub#1); until it lands this returns {status:'sent', acked:false} on timeout —
    the LED still changes, only the confirmation is missing."""
    cid = uuid.uuid4().hex
    props = Properties(PacketTypes.PUBLISH)
    props.ResponseTopic = f"robots/{robot_id}/led/reply"
    props.CorrelationData = cid.encode()

    _reply_event.clear()
    _replies.pop(cid, None)
    _publish(f"robots/{robot_id}/led",
             {"method": "set_led", "on": bool(on),
              "red": _clamp8(red), "green": _clamp8(green), "blue": _clamp8(blue)},
             properties=props)

    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if cid in _replies:
            return {"acked": True, **_replies.pop(cid)}
        _reply_event.wait(timeout=0.1)
    return {"status": "sent", "acked": False,
            "note": "no reply within timeout — firmware RPC reply not wired yet (hub#1)"}


# ---- wire primitives ---------------------------------------------------------
# The pedagogy layer, and the escape hatch: every future channel (range, imu,
# cmd_vel) is usable through these the day firmware ships it, before any
# dedicated tool exists. Scope is the connected credential's ACL — a team
# identity can only publish under its own subtree; the broker enforces it,
# not this server.

@mcp.tool()
def publish(topic: str, payload: dict) -> str:
    """Publish a JSON payload to any MQTT topic (e.g. robots/team3/pwm).
    Scoped by your credential's broker ACL — a team can only write its own
    robots/<team>/... subtree; out-of-scope publishes are silently dropped by
    the broker. Use watch() to confirm a message actually landed."""
    _publish(topic, payload)
    return f"published to {topic}: {json.dumps(payload)}"


@mcp.tool()
def watch(topic_pattern: str = "robots/#", duration_s: float = 5.0, max_messages: int = 50) -> dict:
    """Subscribe to a topic pattern (MQTT wildcards: + one level, # rest) and
    collect live messages for duration_s. Returns {topic, payload, t} per
    message, oldest first. robots/# is anonymously readable, so this always
    works for observing the fleet — your own drive commands included (watch
    your team's subtree while your code runs to see exactly what's on the wire)."""
    duration_s = min(max(duration_s, 0.1), 30.0)
    tap = {"pattern": topic_pattern, "msgs": [], "cap": max(1, min(int(max_messages), 200))}
    _client.subscribe(topic_pattern)
    _watchers.append(tap)
    deadline = time.time() + duration_s
    while time.time() < deadline and len(tap["msgs"]) < tap["cap"]:
        time.sleep(0.05)
    _watchers.remove(tap)
    # Only drop the extra subscription if the background channels don't need it.
    if topic_pattern not in ("robots/+/imu", "robots/+/sys", "robots/+/led/reply"):
        _client.unsubscribe(topic_pattern)
    out = {"messages": tap["msgs"], "count": len(tap["msgs"])}
    if not tap["msgs"]:
        out["note"] = ("nothing seen — no publisher on that pattern right now, "
                       "or it's outside your credential's read scope")
    return out


def _board_team(board: str) -> str | None:
    rec = _sys.get(board)
    return rec.get("_team") if rec else None


@mcp.tool()
def blink(board: str) -> str:
    """Blink a board's LED for ~6 s so a human can find the physical rover on
    the desk. Targets the board through its current team topic (works for pool
    boards too, if your credential may write there — professor always can)."""
    team = _board_team(board)
    if not team:
        return f"unknown board {board} — call fleet() to see who's online"
    _publish(f"robots/{team}/cmd/identify", {"target": board})
    return f"blink sent to {board} (via robots/{team}/cmd/identify) — watch the desk"


# ---- in-chat pairing -----------------------------------------------------------
# Device-flow-shaped auth on the hub's own gate (hubd /codes/request|poll):
# knock, show the human a short pairing code, and a browser click delivers the
# credential — no code typed into any config screen. For an existing team the
# approver is the team's own signed-in dashboard (/codes/grant re-shares the
# code it holds; nothing is minted, the rover is untouched); for a new name it
# is the professor's panel (a code is minted, exactly like a browser knock).

_pending_req: dict[str, dict] = {}   # team -> {token, pair, join}; survives across tool calls


def _reconnect_as(user: str, password: str) -> None:
    global HUB_USER, HUB_PASS
    HUB_USER, HUB_PASS = user, password
    _client.username_pw_set(user, password)
    try:
        _client.disconnect()
    except Exception:
        pass
    _client.reconnect()                  # on_connect resubscribes the channels


@mcp.tool()
def request_access(team: str, wait_s: float = 45.0) -> dict:
    """Get this session authorized from a browser instead of a config screen.
    Knocks on the hub's access gate as `team` and waits for a human click:
    an existing team approves from its own signed-in dashboard (tell the
    teammate the PAIRING CODE this returns — the approve banner shows the
    same one); a brand-new name is approved by the professor. On approval
    this session reconnects with the delivered credential and your scope
    becomes that team's subtree. If the wait times out, call this again with
    the same name — the request stays pending on the hub for ~30 minutes."""
    team = team.strip()
    req = _pending_req.get(team)
    if req is None:
        r = _hubd("/codes/request", {"name": team})
        if not r.get("ok"):
            return {"error": r.get("error", "the hub refused the request"),
                    "hint": "an unanswered earlier knock for this name may still be pending — "
                            "it can be denied from the dashboard, or expires on its own"}
        req = _pending_req[team] = {"token": r["token"], "pair": r.get("pair", ""),
                                    "join": r.get("join", False)}
    approver = (f"{team}'s signed-in dashboard shows an Approve banner"
                if req["join"] else "the professor's codes panel lists the request")
    deadline = time.time() + min(max(wait_s, 2.0), 120.0)
    while time.time() < deadline:
        r = _hubd("/codes/poll", {"token": req["token"]})
        status = r.get("status")
        if status == "approved":
            _pending_req.pop(team, None)
            try:
                _reconnect_as(r["user"], r["pass"])
            except Exception as e:
                return {"status": "approved", "error": f"reconnect failed: {e}",
                        "hint": "the credential was delivered (one-shot) — restart the server "
                                "with it set as HUB_USER/HUB_PASS"}
            return {"status": "connected", "user": r["user"],
                    "note": "scope is now this team's subtree — drive/publish will stick"}
        if status == "denied":
            _pending_req.pop(team, None)
            return {"status": "denied", "note": "the approver dismissed this request"}
        if status == "unknown":
            _pending_req.pop(team, None)
            return {"status": "expired", "note": "the request lapsed or the hub restarted — call again to re-knock"}
        if status is None:                     # HTTP error — hubd unreachable
            return {"error": r.get("error", "hub unreachable"), "pair": req["pair"]}
        time.sleep(2.0)
    return {"status": "waiting", "pair": req["pair"],
            "note": f"tell the human: approve on {approver}, and ONLY if it shows "
                    f"pairing code {req['pair']} — then call request_access('{team}') again"}


# ---- professor ops -----------------------------------------------------------
# Wrappers over hubd's HTTP /codes API plus the cmd/config publishes the
# dashboard's professor panels make. Mutations carry HUB_PASS as the professor
# code (hubd re-verifies it against the broker per request) — with a team
# credential these simply come back rejected.

def _hubd(path: str, body: dict | None = None) -> dict:
    import urllib.request, urllib.error
    try:
        req = urllib.request.Request(
            f"{HUB_HTTP}{path}",
            data=json.dumps(body).encode() if body is not None else None,
            method="POST" if body is not None else "GET",
        )
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return {"error": f"hubd {path} -> HTTP {e.code}",
                "hint": "this hub may not serve the /codes API (ESP32 hub role has none)"}
    except Exception as e:  # connection refused, timeout, bad JSON
        return {"error": f"hubd {path} unreachable: {e}"}


@mcp.tool()
def codes_list() -> dict:
    """Broker identities (team names) and whether the class still runs the
    shipped placeholder codes. Public read on the Pi hub."""
    return _hubd("/codes/list")


@mcp.tool()
def codes_set(team: str, code: str = "") -> dict:
    """Create a team or rotate its code (professor only — authenticates with
    this server's own credential). Empty code = hub generates a readable one;
    the code in the response is shown exactly once and cannot be recovered
    later, only rotated."""
    return _hubd("/codes/set", {"auth": HUB_PASS, "user": team, "pass": code})


@mcp.tool()
def codes_del(team: str) -> dict:
    """Delete a team identity (professor only). Its rover and browsers lose
    access; `professor` and the pool identity are protected."""
    return _hubd("/codes/del", {"auth": HUB_PASS, "user": team})


@mcp.tool()
def requests_list() -> dict:
    """Pending access requests from the dashboard gate: [{name, board}].
    board is set when a team claimed a specific rover ('' = name-only)."""
    return _hubd("/codes/requests")


@mcp.tool()
def approve_request(name: str) -> dict:
    """Approve a pending access request (professor only): the hub mints the
    team's code and delivers it to the requester's browser. If the request
    claimed a board, this also assigns that rover to the new team (the same
    cmd/config the dashboard publishes) — it reboots renamed. JOIN requests
    (name already has a code) can't be approved here: the team grants those
    from its own signed-in dashboard."""
    r = _hubd("/codes/approve", {"auth": HUB_PASS, "name": name})
    if r.get("ok") and r.get("board"):
        team = _board_team(r["board"]) or "unassigned"
        _publish(f"robots/{team}/cmd/config",
                 {"target": r["board"], "team": r["user"], "pass": r["pass"]})
        r["assigned"] = f"{r['board']} -> {r['user']} (reboots in a few seconds)"
    return r


@mcp.tool()
def deny_request(name: str) -> dict:
    """Dismiss a pending access request (professor only); the requester's
    browser is told."""
    return _hubd("/codes/deny", {"auth": HUB_PASS, "name": name})


@mcp.tool()
def assign(board: str, team: str, code: str, name: str = "", hub_pin: str = "") -> dict:
    """Manually (re)assign a board to a team — the repair path; new teams
    normally arrive via approve_request. `code` must be the team's current
    broker code (create it first with codes_set). Optional hub_pin locks the
    board to one exact hub SSID ('-' clears an existing pin). The rover saves
    the credential to NVS and reboots under the new identity."""
    cur = _board_team(board)
    if not cur:
        return {"error": f"unknown board {board} — call fleet() to see who's online"}
    cfg: dict = {"target": board, "team": team, "pass": code}
    if name:
        cfg["name"] = name
    if hub_pin == "-":
        cfg["hub"] = ""
    elif hub_pin:
        cfg["hub"] = hub_pin
    _publish(f"robots/{cur}/cmd/config", cfg)
    return {"sent": f"{board} ({cur}) -> {team}", "note": "reboots and reappears in a few seconds"}


@mcp.tool()
def flip(board: str, direction: str) -> dict:
    """Fix a rover driving the wrong way without rewiring: direction is one of
    'left' (reverse left motor), 'right' (reverse right motor), 'swap'
    (exchange sides). Permutes the stored motor pins in NVS; the rover reboots
    with the fix."""
    if direction not in ("left", "right", "swap"):
        return {"error": "direction must be left, right, or swap"}
    team = _board_team(board)
    if not team:
        return {"error": f"unknown board {board} — call fleet() to see who's online"}
    _publish(f"robots/{team}/cmd/config", {"target": board, "flip": {direction: True}})
    return {"sent": f"flip {direction} -> {board}", "note": "reboots with the fix in a few seconds"}


def main() -> None:
    if HUB_PASS:
        _client.username_pw_set(HUB_USER, HUB_PASS)
    else:
        # No credential = anonymous, deliberately: the broker ACL gives
        # anonymous read on robots/#, so the fleet/watch/read_imu tools work
        # out of the box and request_access() upgrades the session in-chat.
        print(f"[hub_mcp] no HUB_PASS — connecting anonymous (read-only)"
              f"{f' (HUB_USER={HUB_USER!r} ignored without a pass)' if HUB_USER else ''}; "
              "use the request_access tool to pair for drive access",
              file=sys.stderr)
    # Async connect + paho's retry loop: the server must come up (and stay up)
    # even when launched off the hub's Wi-Fi — an enabled-but-unconfigured
    # desktop extension must not crash at spawn.
    _client.connect_async(HUB_HOST, HUB_PORT, keepalive=30)
    _client.loop_start()                 # background network thread; tools stay sync
    try:
        mcp.run()                        # stdio transport — Claude Code speaks this
    finally:
        _client.loop_stop()
        _client.disconnect()


if __name__ == "__main__":
    main()
