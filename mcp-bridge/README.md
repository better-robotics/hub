# hub_mcp — drive the fleet from an LLM

An [MCP](https://modelcontextprotocol.io) tool server that lets an LLM agent
(Claude Code, or anything that speaks MCP) operate the classroom rovers over the
same MQTT contract the dashboard and firmware use.

The rovers are ESP32s — they can't host an LLM, so the intelligence runs on the
**hub** and reaches the fleet across the fabric:

```
  Claude Code ──stdio──► hub_mcp.py ──MQTT :1883──► Mosquitto ──► ESP32 rovers
   (on the hub)          (this tool,  as `professor`)   (broker)   (esp-mqtt)
```

It connects as the `professor` ACL identity — write on `robots/+/pwm` and
`robots/+/led`, read on `robots/#` (see `../../mosquitto-acl.example.conf`). It
is the first real MQTT *client* in this repo (hubd is deliberately not one).

## Tools it exposes

| tool | topic | what it does |
|------|-------|--------------|
| `drive(robot_id, left_motor, right_motor, duration_ms=400)` | `robots/<id>/pwm` | signed PWM per side (±255, sign = direction); auto-expires after `duration_ms` |
| `stop(robot_id)` | `robots/<id>/pwm` | zero PWM, immediate halt |
| `read_imu(robot_id, timeout_s=2)` | `robots/<id>/imu` | latest accel/gyro sample, freshness-gated |
| `fleet()` | `robots/+/sys` | every rover on the hub + seconds-since-last-message |
| `set_led(robot_id, on, red, green, blue)` | `robots/<id>/led` | RGB set via MQTT5 request/reply* |

\* The firmware-side `led/reply` isn't wired yet (hub#1); until it lands
`set_led` returns `acked: false` on timeout — the LED still changes, only the
confirmation is missing.

## Run

```sh
pip install -r requirements.txt          # or: uv pip install -r requirements.txt
HUB_HOST=hub.local HUB_PASS=<professor-pw> python hub_mcp.py
```

Environment knobs (defaults match `../pi/mosquitto.example.conf`):

| var | default | note |
|-----|---------|------|
| `HUB_HOST` | `localhost` | broker host (`hub.local` reaches either hub) |
| `HUB_PORT` | `1883` | raw MQTT — **not** the `:9001` WebSocket port |
| `HUB_USER` | `professor` | ACL identity with fleet write |
| `HUB_PASS` | *(empty)* | password from your `mosquitto-passwd` |

## Register with Claude Code

This repo ships a committed **`.mcp.json`** at its root, so opening `hub/` in
Claude Code offers the `hub-fleet` server automatically (you approve it once).
It resolves this script by project-relative path and pulls the broker password
from your environment — never a committed secret:

```json
{
  "mcpServers": {
    "hub-fleet": {
      "command": "python3",
      "args": ["${CLAUDE_PROJECT_DIR}/mcp-bridge/hub_mcp.py"],
      "env": { "HUB_HOST": "${HUB_HOST:-hub.local}", "HUB_PASS": "${HUB_PASS}" }
    }
  }
}
```

So: `pip install -r mcp-bridge/requirements.txt`, `export HUB_PASS=<professor-pw>`,
open the repo, approve `hub-fleet`. (`${CLAUDE_PROJECT_DIR}` makes the path work
regardless of where you launched Claude; `${HUB_PASS}` stays in your shell, out
of git.)

Then, in a Claude Code session:

> *"List the fleet. Drive rover_01 forward at half speed for one second, read its
> IMU, and stop it if accel_z drops below 8."*

Claude calls `fleet()` → `drive("rover_01", 128, 128, 1000)` → `read_imu` →
`stop` — a closed loop over the fabric, with the rover's `duration_ms` expiry as
the safety floor if the session drops.

## Scope

A demo/operator bridge, not a control-loop runtime — MQTT QoS 0, one shared
`professor` credential, no rate limiting. Per-device identity and the `set_led`
reply path are hub#1. For hard-real-time motion, close the loop on the rover;
this is for supervisory, natural-language operation.
