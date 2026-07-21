#!/usr/bin/env python3
"""hub_mcp.py — an MCP server that lets an LLM drive the classroom fleet.

This is the *hub-side* answer to "can Claude Code run on the ESP32": it can't
(the CLI is a native Linux/macOS binary; the robot is a 400 KB-SRAM MCU running
FreeRTOS). Instead Claude Code runs on the hub appliance and reaches the robots
over this fabric — an MCP tool server that speaks the same MQTT contract the
dashboard and firmware already use.

Identity lives in the topic (`robots/<id>/<channel>`), never the body — the
envelopes mirror `protocol/envelopes/*.json`:

    robots/<id>/pwm        drive       {timestamp, left_motor, right_motor, duration_ms}
    robots/<id>/imu        telemetry   {timestamp, accel_x..z, gyro_x..z}
    robots/<id>/sys        fleet       read-only presence/telemetry
    robots/<id>/led        set_led req / robots/<id>/led/reply resp (MQTT5 correlation)

A robot's name is a topic address now, not a credential — the hub's own
Wi-Fi is the security boundary, so every client (robot or browser) gets full
read+write on `robots/#` and `pair/#` with no username/password at all (see
mosquitto-acl.example.conf). HUB_USER/HUB_PASS only matter for one thing:
`estop()`, the sole action still gated behind the `instructor` credential.
Every other tool here works fine connected anonymously — empty HUB_PASS is
the documented default and just means "connect anonymous."

Run:
    pip install "mcp[cli]" paho-mqtt        # or: uv pip install ...
    HUB_HOST=hub.local HUB_PASS=secret python hub_mcp.py   # only needed for estop()
    HUB_HOST=hub.local python hub_mcp.py    # anonymous; everything but estop() works

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
HUB_USER = os.environ.get("HUB_USER", "instructor")           # only used by estop() — the one gated action
HUB_PASS = os.environ.get("HUB_PASS", "")
MOTOR_MAX = 255                                              # 8-bit PWM magnitude; sign = direction

# ---- live fabric state, kept fresh by background subscriptions --------------
# GIL makes these plain-dict swaps atomic enough for this read/write pattern.
_imu: dict[str, dict] = {}       # robot_id -> latest IMU envelope (+ _rx wall-clock)
_sys: dict[str, dict] = {}       # board id -> latest sys envelope (+ _rx wall-clock, _name)
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
        # pool rows fixed). The topic id rides along as the board's assigned identity.
        payload["_name"] = robot_id
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


def _publish(topic: str, body: dict, properties: Properties | None = None,
             *, qos: int = 0, retain: bool = False) -> None:
    if not _client.is_connected():
        raise RuntimeError(
            f"not connected to the hub at {HUB_HOST}:{HUB_PORT} — are you on its Wi-Fi? "
            "(the connection keeps retrying in the background; try again in a few seconds)")
    info = _client.publish(topic, json.dumps(body), qos=qos, retain=retain, properties=properties)
    info.wait_for_publish(timeout=2.0)


# ---- MCP tools --------------------------------------------------------------
mcp = FastMCP("hub-fleet")


@mcp.tool()
def drive(robot_id: str, left_motor: int, right_motor: int, duration_ms: int = 400) -> str:
    """Drive a robot: signed PWM per side, magnitude 0..255, sign sets direction
    (positive = forward, negative = reverse). The command auto-expires after
    duration_ms — firmware stops the motors when it lapses, so a dropped follow-up
    can't leave a robot running away. Publishes robots/<id>/pwm."""
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
    """Immediately halt a robot (zero PWM, zero duration). Publishes robots/<id>/pwm.
    Transient and per-robot — for a room-wide halt that STAYS engaged, use estop()."""
    _publish(f"robots/{robot_id}/pwm",
             {"timestamp": time.time(), "left_motor": 0, "right_motor": 0, "duration_ms": 0})
    return f"stop {robot_id}"


@mcp.tool()
def estop(engaged: bool = True, reason: str = "") -> str:
    """Fleet-wide EMERGENCY STOP latch (CONTRACT.md § Fleet e-stop). engaged=True
    halts every robot on the hub now and makes them refuse all drive commands
    until estop(engaged=False) clears it. Published RETAINED on fleet/estop, so
    a robot that reboots or reconnects mid-emergency latches anyway. Needs the
    instructor credential (the only fleet/estop write grant in the Pi ACL)."""
    body: dict = {"timestamp": time.time(), "engaged": engaged, "by": HUB_USER}
    if reason:
        body["reason"] = reason
    _publish("fleet/estop", body, qos=1, retain=True)
    return ("E-STOP ENGAGED — fleet halted and latched (clear with estop(engaged=False))"
            if engaged else "e-stop cleared — fleet released")


@mcp.tool()
def read_imu(robot_id: str, timeout_s: float = 2.0) -> dict:
    """Latest IMU sample for a robot: accel_x/y/z, gyro_x/y/z (epoch-seconds
    timestamp). Waits up to timeout_s for a sample *newer than this call* so a
    reading reflects the robot's state now, not a stale cache. Reads robots/<id>/imu."""
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
    """Every board currently on the hub, keyed by hardware board id, each with
    its assigned identity (the topic id it publishes under — `unassigned` = the pool),
    latest sys telemetry, and seconds-since-last-message. Reads robots/+/sys, open
    to anyone on the hub's Wi-Fi — the same data the dashboard's fleet cards show.
    To drive a board, target the id it's assigned (pool boards all share `unassigned`)."""
    now = time.time()
    return {
        board: {**_clean(payload), "name": payload.get("_name", "?"),
                "age_s": round(now - payload.get("_rx", now), 1)}
        for board, payload in _sys.items()
    }


@mcp.tool()
def set_led(robot_id: str, on: bool, red: int = 0, green: int = 0, blue: int = 0,
            timeout_s: float = 1.5) -> dict:
    """Set a robot's RGB LED and wait for its ack. Sends an MQTT5 request on
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
# dedicated tool exists. robots/# and pair/# are open read+write to anyone
# on the hub's Wi-Fi — the broker ACL enforces that boundary, not this server.

@mcp.tool()
def publish(topic: str, payload: dict) -> str:
    """Publish a JSON payload to any MQTT topic (e.g. robots/robot3/pwm).
    The broker ACL leaves robots/# and pair/# open to everyone on the hub's
    Wi-Fi, so this reaches any robot's subtree. Use watch() to confirm a
    message actually landed."""
    _publish(topic, payload)
    return f"published to {topic}: {json.dumps(payload)}"


@mcp.tool()
def watch(topic_pattern: str = "robots/#", duration_s: float = 5.0, max_messages: int = 50) -> dict:
    """Subscribe to a topic pattern (MQTT wildcards: + one level, # rest) and
    collect live messages for duration_s. Returns {topic, payload, t} per
    message, oldest first. robots/# is open to everyone on the hub's Wi-Fi,
    so this always works for observing the fleet — your own drive commands
    included (watch your own subtree while your code runs to see exactly
    what's on the wire)."""
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
        out["note"] = "nothing seen — no publisher on that pattern right now"
    return out


def _board_name(board: str) -> str | None:
    rec = _sys.get(board)
    return rec.get("_name") if rec else None


@mcp.tool()
def blink(board: str) -> str:
    """Blink a board's LED for ~6 s so a human can find the physical robot on
    the desk. Targets the board through its current assigned topic (works for
    pool boards too — writes are open to everyone on the hub's Wi-Fi)."""
    name = _board_name(board)
    if not name:
        return f"unknown board {board} — call fleet() to see who's online"
    _publish(f"robots/{name}/cmd/identify", {"target": board})
    return f"blink sent to {board} (via robots/{name}/cmd/identify) — watch the desk"


# ---- naming ---------------------------------------------------------------
# A name is just a topic address now — assigning one is a plain MQTT publish,
# no credential involved (same shape as flip() below).

@mcp.tool()
def assign(board: str, name: str, hub_pin: str = "") -> dict:
    """(Re)assign a board to a name — the topic id it publishes/listens under.
    Optional hub_pin locks the board to one exact hub SSID ('-' clears an
    existing pin). The robot saves the name to NVS and reboots under it."""
    cur = _board_name(board)
    if not cur:
        return {"error": f"unknown board {board} — call fleet() to see who's online"}
    cfg: dict = {"target": board, "name": name}
    if hub_pin == "-":
        cfg["hub"] = ""
    elif hub_pin:
        cfg["hub"] = hub_pin
    _publish(f"robots/{cur}/cmd/config", cfg)
    return {"sent": f"{board} ({cur}) -> {name}", "note": "reboots and reappears in a few seconds"}


@mcp.tool()
def flip(board: str, direction: str) -> dict:
    """Fix a robot driving the wrong way without rewiring: direction is one of
    'left' (reverse left motor), 'right' (reverse right motor), 'swap'
    (exchange sides). Permutes the stored motor pins in NVS; the robot reboots
    with the fix."""
    if direction not in ("left", "right", "swap"):
        return {"error": "direction must be left, right, or swap"}
    name = _board_name(board)
    if not name:
        return {"error": f"unknown board {board} — call fleet() to see who's online"}
    _publish(f"robots/{name}/cmd/config", {"target": board, "flip": {direction: True}})
    return {"sent": f"flip {direction} -> {board}", "note": "reboots with the fix in a few seconds"}


def main() -> None:
    if HUB_PASS:
        _client.username_pw_set(HUB_USER, HUB_PASS)
    else:
        # No credential = anonymous, deliberately: the broker ACL opens
        # robots/# and pair/# to everyone on the hub's Wi-Fi, so every tool
        # except estop() works out of the box with no HUB_PASS at all.
        print(f"[hub_mcp] no HUB_PASS — connecting anonymous"
              f"{f' (HUB_USER={HUB_USER!r} ignored without a pass)' if HUB_USER else ''}; "
              "estop() will fail without the instructor credential",
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
