# hub_mcp — drive the fleet from an LLM

An [MCP](https://modelcontextprotocol.io) tool server that lets an LLM agent
(Claude Code, or anything that speaks MCP) operate the classroom robots over the
same MQTT contract the dashboard and firmware use.

The robots are ESP32s — they can't host an LLM, so the intelligence runs on the
**hub** and reaches the fleet across the fabric:

```
  Claude Code ──stdio──► hub_mcp.py ──MQTT :1883──► Mosquitto ──► ESP32 robots
   (on the hub)          (this tool,  as `instructor`)   (broker)   (esp-mqtt)
```

**A robot's name is a topic address, not a credential.** The hub's own Wi-Fi
is the security boundary: every MQTT client — robot or browser — gets full
read+write on `robots/#` and `pair/#` with no username/password at all.
HUB_USER/HUB_PASS matter for exactly one tool: `estop()`, the sole action
still gated behind the `instructor` credential (the only `fleet/estop` write
grant in the Pi ACL). Every other tool here works fine connected
anonymously. No role logic lives in this server. It is the first real MQTT
*client* in this repo (hubd is deliberately not one).

## Tools it exposes

Fleet (open to any client on the hub's Wi-Fi — no credential needed):

| tool | topic | what it does |
|------|-------|--------------|
| `fleet()` | `robots/+/sys` | every board online (keyed by board id, with its name) + freshness |
| `drive(robot_id, left_motor, right_motor, duration_ms=400)` | `robots/<id>/pwm` | signed PWM per side (±255, sign = direction); auto-expires after `duration_ms` |
| `stop(robot_id)` | `robots/<id>/pwm` | zero PWM, immediate halt |
| `blink(board)` | `robots/<name>/cmd/identify` | flash a board's LED ~6 s — find the physical robot |
| `read_imu(robot_id, timeout_s=2)` | `robots/<id>/imu` | latest accel/gyro sample, freshness-gated (channel lands with next-gen electronics) |
| `set_led(robot_id, on, red, green, blue)` | `robots/<id>/led` | RGB set via MQTT5 request/reply* |

Wire primitives (the pedagogy layer — and how any future channel is usable
before a dedicated tool exists):

| tool | what it does |
|------|--------------|
| `publish(topic, payload)` | raw JSON publish; `robots/#` and `pair/#` are open to everyone on the hub's Wi-Fi |
| `watch(topic_pattern, duration_s=5, max_messages=50)` | subscribe with wildcards, collect live messages — see exactly what's on the wire while your code runs |

Naming and repair (also open — no credential needed):

| tool | backend | what it does |
|------|---------|--------------|
| `assign(board, name, hub_pin="")` | `cmd/config` | (re)assign a board to a name — the topic id it publishes/listens under |
| `flip(board, direction)` | `cmd/config` | fix motor orientation: `left`, `right`, or `swap` |

Instructor-gated (the one credentialed action — needs `HUB_PASS` set to the
`instructor` password):

| tool | topic | what it does |
|------|-------|--------------|
| `estop(engaged=True, reason="")` | `fleet/estop` | fleet-wide emergency stop latch, retained |

\* The firmware-side `led/reply` isn't wired yet (hub#1); until it lands
`set_led` returns `acked: false` on timeout — the LED still changes, only the
confirmation is missing.

## Run

```sh
pip install -r requirements.txt          # or: uv pip install -r requirements.txt
HUB_HOST=hub.local python hub_mcp.py                       # anonymous — everything but estop() works
HUB_HOST=hub.local HUB_PASS=<instructor-pw> python hub_mcp.py  # adds estop()
```

Environment knobs (defaults match `../pi/mosquitto.example.conf`):

| var | default | note |
|-----|---------|------|
| `HUB_HOST` | `localhost` | broker host (`hub.local` reaches either hub) |
| `HUB_PORT` | `1883` | raw MQTT — **not** the `:9001` WebSocket port |
| `HUB_USER` | `instructor` | ACL identity (ignored without a `HUB_PASS`) |
| `HUB_PASS` | *(empty)* | the `instructor` password from your `mosquitto-passwd`; empty = connect anonymous, which is fine for every tool except `estop()` |

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

So: `pip install -r mcp-bridge/requirements.txt`, `export HUB_PASS=<instructor-pw>`,
open the repo, approve `hub-fleet`. (`${CLAUDE_PROJECT_DIR}` makes the path work
regardless of where you launched Claude; `${HUB_PASS}` stays in your shell, out
of git.)

Then, in a Claude Code session:

> *"List the fleet. Drive robot_01 forward at half speed for one second, read its
> IMU, and stop it if accel_z drops below 8."*

Claude calls `fleet()` → `drive("robot_01", 128, 128, 1000)` → `read_imu` →
`stop` — a closed loop over the fabric, with the robot's `duration_ms` expiry as
the safety floor if the session drops.

## Scope

A demo/operator bridge, not a control-loop runtime — MQTT QoS 0, one shared
`instructor` credential, no rate limiting. Per-device identity and the `set_led`
reply path are hub#1. For hard-real-time motion, close the loop on the robot;
this is for supervisory, natural-language operation.
