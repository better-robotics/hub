#!/usr/bin/env python3
"""hub_mcp_zenoh.py — the Zenoh port of hub_mcp.py (MQTT→Zenoh migration, M2).

Runs alongside the MQTT `hub_mcp.py` during the migration — the live bench tool
stays on MQTT until the cutover; this is the Zenoh prototype that validates the
`zenoh-migration.md` wire spec on real `eclipse-zenoh`. Same FastMCP tools, same
envelopes, same `robots/<id>/<channel>` keys — only the transport changes:

    MQTT                              Zenoh
    ----------------------------      ------------------------------------------
    subscribe robots/+/imu            declare_subscriber robots/*/imu   (+ -> *)
    publish  robots/<id>/pwm          session.put robots/<id>/pwm
    set_led req/reply (MQTT5)         session.get robots/<id>/led  (rover queryable)
    fleet/estop retained              session.put fleet/estop  (latch persistence is
                                        hub-side: Pi storage / ESP app-queryable)

Discovery: HUB_HOST set -> client mode connecting to `tcp/<host>:7447`; unset ->
peer mode with multicast scouting (local bench + the validation harness).

Run:
    pip install "mcp[cli]" eclipse-zenoh
    HUB_HOST=hub.local python hub_mcp_zenoh.py
"""
from __future__ import annotations

import json
import os
import sys
import threading
import time
import uuid

import zenoh
from mcp.server.fastmcp import FastMCP

# ---- config (env-driven) ----------------------------------------------------
HUB_HOST = os.environ.get("HUB_HOST", "")                     # unset = peer/multicast (local)
HUB_PORT = int(os.environ.get("HUB_PORT", "7447"))            # Zenoh default TCP port
HUB_USER = os.environ.get("HUB_USER", "instructor")           # e-stop authority label (app-layer)
HUB_PASS = os.environ.get("HUB_PASS", "")
MOTOR_MAX = 255

# ---- live fabric state, kept fresh by background subscriptions --------------
_imu: dict[str, dict] = {}
_sys: dict[str, dict] = {}
_watchers: list[dict] = []

_session: zenoh.Session | None = None
_subs: list = []                 # keep declared subscribers alive


def _keyexpr_from_mqtt(pattern: str) -> str:
    """MQTT wildcards -> Zenoh: `#` (rest) -> `**`, `+` (one level) -> `*`."""
    parts = ["**" if p == "#" else "*" if p == "+" else p for p in pattern.split("/")]
    return "/".join(parts)


def _key_matches(pattern_ke: str, key: str) -> bool:
    return zenoh.KeyExpr(pattern_ke).intersects(zenoh.KeyExpr(key))


def _on_sample(sample: "zenoh.Sample") -> None:
    key = str(sample.key_expr)
    try:
        body = json.loads(sample.payload.to_bytes())
    except (ValueError, UnicodeDecodeError):
        body = sample.payload.to_bytes().decode(errors="replace")

    # Feed active watch() taps (they see every subscribed key).
    for w in _watchers:
        if len(w["msgs"]) < w["cap"] and _key_matches(w["pattern"], key):
            w["msgs"].append({"topic": key, "payload": body, "t": round(time.time(), 3)})

    if not isinstance(body, dict):
        return
    parts = key.split("/")                   # robots/<id>/<channel>
    if len(parts) < 3:
        return
    robot_id, channel = parts[1], parts[2]
    body["_rx"] = time.time()
    if channel == "imu":
        _imu[robot_id] = body
    elif channel == "sys":
        # Key by BOARD, not topic id: every pool board publishes on
        # robots/unassigned/sys, so keying by topic collapses them into one
        # flapping entry. The topic id rides along as the board's assigned name.
        body["_name"] = robot_id
        _sys[body.get("board") or robot_id] = body


def _clamp(v: int) -> int:
    return max(-MOTOR_MAX, min(MOTOR_MAX, int(v)))


def _clamp8(v: int) -> int:
    return max(0, min(255, int(v)))


def _clean(d: dict) -> dict:
    return {k: v for k, v in d.items() if not k.startswith("_")}


def _put(key: str, body: dict) -> None:
    if _session is None:
        raise RuntimeError(
            f"not connected to the hub — are you on its Wi-Fi? "
            f"(HUB_HOST={HUB_HOST or '(peer/multicast)'})")
    _session.put(key, json.dumps(body).encode())


# ---- MCP tools --------------------------------------------------------------
mcp = FastMCP("hub-fleet")


@mcp.tool()
def drive(robot_id: str, left_motor: int, right_motor: int, duration_ms: int = 400) -> str:
    """Drive a rover: signed PWM per side, magnitude 0..255, sign sets direction
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
    """Immediately halt a rover (zero PWM, zero duration). Publishes robots/<id>/pwm.
    Transient and per-rover — for a room-wide halt that STAYS engaged, use estop()."""
    _put(f"robots/{robot_id}/pwm",
         {"timestamp": time.time(), "left_motor": 0, "right_motor": 0, "duration_ms": 0})
    return f"stop {robot_id}"


@mcp.tool()
def estop(engaged: bool = True, reason: str = "") -> str:
    """Fleet-wide EMERGENCY STOP latch (CONTRACT.md § Fleet e-stop). engaged=True
    halts every rover and makes them refuse drive until estop(engaged=False).
    Published on fleet/estop; the hub holds the latch (Pi storage / ESP queryable)
    and answers a rebooting rover's join-time get, so the stop survives reconnects.
    The e-stop authority is the instructor, enforced at the hub (app-layer)."""
    body: dict = {"timestamp": time.time(), "engaged": engaged, "by": HUB_USER}
    if reason:
        body["reason"] = reason
    _put("fleet/estop", body)
    return ("E-STOP ENGAGED — fleet halted and latched (clear with estop(engaged=False))"
            if engaged else "e-stop cleared — fleet released")


@mcp.tool()
def read_imu(robot_id: str, timeout_s: float = 2.0) -> dict:
    """Latest IMU sample for a rover: accel_x/y/z, gyro_x/y/z. Waits up to
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
    return {"error": f"no IMU seen for {robot_id}", "hint": "check robot_id and that the rover is publishing"}


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
    """Set a rover's RGB LED and wait for its ack. Native Zenoh RPC: a get() on the
    rover's queryable at robots/<id>/led carries the request and returns the reply
    ({status:ok} / {status:error,message}) — no reply topic, no correlation-data."""
    req = {"method": "set_led", "on": bool(on),
           "red": _clamp8(red), "green": _clamp8(green), "blue": _clamp8(blue)}
    if _session is None:
        return {"status": "error", "acked": False, "message": "not connected"}

    reply_box: dict = {}
    done = threading.Event()

    def _on_reply(reply):
        try:
            if reply.ok is not None:
                reply_box.update(json.loads(reply.ok.payload.to_bytes()))
        except Exception as e:                       # noqa: BLE001 — surface, don't crash the get
            reply_box.setdefault("_err", str(e))
        finally:
            done.set()

    _session.get(f"robots/{robot_id}/led", handler=_on_reply,
                 payload=json.dumps(req).encode(), timeout=timeout_s)
    if done.wait(timeout=timeout_s + 0.5) and reply_box and "_err" not in reply_box:
        return {"acked": True, **reply_box}
    return {"status": "sent", "acked": False,
            "note": "no reply within timeout — is the rover declaring a robots/<id>/led queryable?"}


# ---- wire primitives ---------------------------------------------------------

@mcp.tool()
def publish(topic: str, payload: dict) -> str:
    """Publish a JSON payload to any key (e.g. robots/rover3/pwm). robots/** and
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
    sub = _session.declare_subscriber(ke, _on_sample) if _session else None
    _watchers.append(tap)
    deadline = time.time() + duration_s
    while time.time() < deadline and len(tap["msgs"]) < tap["cap"]:
        time.sleep(0.05)
    _watchers.remove(tap)
    if sub is not None:
        sub.undeclare()
    out = {"messages": tap["msgs"], "count": len(tap["msgs"])}
    if not tap["msgs"]:
        out["note"] = "nothing seen — no publisher on that pattern right now"
    return out


def _board_name(board: str) -> str | None:
    rec = _sys.get(board)
    return rec.get("_name") if rec else None


@mcp.tool()
def blink(board: str) -> str:
    """Blink a board's LED for ~6 s so a human can find the physical rover.
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
    robots/<cur>/cmd/config; the rover saves to NVS and reboots under the name."""
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
    """Fix a rover driving the wrong way without rewiring: 'left', 'right', or
    'swap'. Permutes the stored motor pins in NVS; the rover reboots with the fix."""
    if direction not in ("left", "right", "swap"):
        return {"error": "direction must be left, right, or swap"}
    name = _board_name(board)
    if not name:
        return {"error": f"unknown board {board} — call fleet() to see who's online"}
    _put(f"robots/{name}/cmd/config", {"target": board, "flip": {direction: True}})
    return {"sent": f"flip {direction} -> {board}", "note": "reboots with the fix in a few seconds"}


# ---- session lifecycle ------------------------------------------------------

def _build_config() -> "zenoh.Config":
    conf = zenoh.Config()
    if HUB_HOST:
        # Client mode connecting to the hub's Zenoh endpoint (the DHCP gateway).
        conf.insert_json5("mode", '"client"')
        conf.insert_json5("connect/endpoints", json.dumps([f"tcp/{HUB_HOST}:{HUB_PORT}"]))
    # else: default peer mode + multicast scouting (local bench / validation).
    return conf


def connect() -> None:
    """Open the Zenoh session and declare the background subscriptions. Callable
    from a test harness; main() calls it before serving MCP."""
    global _session
    _session = zenoh.open(_build_config())
    for ke in ("robots/*/imu", "robots/*/sys"):
        _subs.append(_session.declare_subscriber(ke, _on_sample))


def main() -> None:
    if not HUB_PASS:
        print("[hub_mcp_zenoh] no HUB_PASS — e-stop authority is unauthenticated "
              "(fine for local/bench; the hub enforces instructor at the app layer)",
              file=sys.stderr)
    connect()
    try:
        mcp.run()
    finally:
        if _session is not None:
            _session.close()


if __name__ == "__main__":
    main()
