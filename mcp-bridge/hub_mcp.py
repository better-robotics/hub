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

It connects to Mosquitto as `professor` — the ACL identity with write on
`robots/+/pwm` and `robots/+/led` and read on `robots/#`
(see mosquitto-acl.example.conf). It is the first real MQTT *client* in this
repo; hubd is deliberately not one, and reprovision.py still stubs on hub#1.

Run:
    pip install "mcp[cli]" paho-mqtt        # or: uv pip install ...
    HUB_HOST=hub.local HUB_PASS=secret python hub_mcp.py

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
HUB_USER = os.environ.get("HUB_USER", "professor")           # ACL identity with fleet write
HUB_PASS = os.environ.get("HUB_PASS", "")
MOTOR_MAX = 255                                              # 8-bit PWM magnitude; sign = direction

# ---- live fabric state, kept fresh by background subscriptions --------------
# GIL makes these plain-dict swaps atomic enough for this read/write pattern.
_imu: dict[str, dict] = {}       # robot_id -> latest IMU envelope (+ _rx wall-clock)
_sys: dict[str, dict] = {}       # robot_id -> latest sys envelope (+ _rx wall-clock)
_replies: dict[str, dict] = {}   # correlation-id -> led/reply payload
_reply_event = threading.Event()

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
        _sys[robot_id] = payload
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
    """Every rover currently on the hub, keyed by robot_id, with its latest sys
    telemetry and seconds-since-last-message. This is the anonymous public view
    (robots/+/sys) — the same data the dashboard's live fleet card shows."""
    now = time.time()
    return {
        rid: {**_clean(payload), "age_s": round(now - payload.get("_rx", now), 1)}
        for rid, payload in _sys.items()
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


def main() -> None:
    if HUB_USER and not HUB_PASS:
        print(f"[hub_mcp] warning: HUB_USER={HUB_USER!r} but HUB_PASS is empty — "
              "the broker will reject auth (set HUB_PASS to that user's mosquitto password)",
              file=sys.stderr)
    if HUB_USER:
        _client.username_pw_set(HUB_USER, HUB_PASS)
    _client.connect(HUB_HOST, HUB_PORT, keepalive=30)
    _client.loop_start()                 # background network thread; tools stay sync
    try:
        mcp.run()                        # stdio transport — Claude Code speaks this
    finally:
        _client.loop_stop()
        _client.disconnect()


if __name__ == "__main__":
    main()
