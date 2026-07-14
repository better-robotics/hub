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

**The credential IS the role** (same rule as the whole system): the server
connects as whatever identity you give it, and Mosquitto's ACL does the
scoping. A student runs it with their robot's own code and their AI can only
touch that rover; the professor identity gets the fleet plus the management
tools. No role logic lives in this server. It is the first real MQTT *client*
in this repo (hubd is deliberately not one).

## Tools it exposes

Fleet (any identity — reads are anonymous-public, writes ACL-scoped):

| tool | topic | what it does |
|------|-------|--------------|
| `fleet()` | `robots/+/sys` | every board online (keyed by board id, with its name) + freshness |
| `drive(robot_id, left_motor, right_motor, duration_ms=400)` | `robots/<id>/pwm` | signed PWM per side (±255, sign = direction); auto-expires after `duration_ms` |
| `stop(robot_id)` | `robots/<id>/pwm` | zero PWM, immediate halt |
| `blink(board)` | `robots/<name>/cmd/identify` | flash a board's LED ~6 s — find the physical rover |
| `read_imu(robot_id, timeout_s=2)` | `robots/<id>/imu` | latest accel/gyro sample, freshness-gated (channel lands with next-gen electronics) |
| `set_led(robot_id, on, red, green, blue)` | `robots/<id>/led` | RGB set via MQTT5 request/reply* |

Wire primitives (the pedagogy layer — and how any future channel is usable
before a dedicated tool exists):

| tool | what it does |
|------|--------------|
| `publish(topic, payload)` | raw JSON publish; the broker ACL scopes what lands |
| `watch(topic_pattern, duration_s=5, max_messages=50)` | subscribe with wildcards, collect live messages — see exactly what's on the wire while your code runs |

In-chat pairing (no credential configured — the server starts anonymous and
read-only):

| tool | backend | what it does |
|------|---------|--------------|
| `request_access(name, wait_s=45)` | hubd `/codes/request` + `/codes/poll` | knock on the hub's access gate and wait for a browser click. An existing name's owner approves from **its own signed-in dashboard** (an Approve banner appears; match the pairing code this tool returns). A new name is approved by the professor. On approval the session reconnects with the delivered code — scope becomes that identity's subtree. |

Professor ops (mutations authenticate with this server's own credential —
under any other identity's credential they simply come back rejected):

| tool | backend | what it does |
|------|---------|--------------|
| `codes_list()` / `codes_set(name, code="")` / `codes_del(name)` | hubd `/codes/*` | manage broker identities; empty code = hub generates one (shown once) |
| `requests_list()` / `approve_request(name)` / `deny_request(name)` | hubd `/codes/*` | the dashboard gate's access requests; approving a board claim also assigns that rover |
| `assign(board, name, code, hub_pin="")` | `cmd/config` | manual (re)assign — the repair path |
| `flip(board, direction)` | `cmd/config` | fix motor orientation: `left`, `right`, or `swap` |

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
| `HUB_USER` | `professor` | ACL identity (ignored without a `HUB_PASS`) |
| `HUB_PASS` | *(empty)* | password from your `mosquitto-passwd`; empty = connect anonymous (read-only) and pair in-chat via `request_access` |

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
