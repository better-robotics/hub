# hub_mcp — drive the fleet from an LLM

An [MCP](https://modelcontextprotocol.io) tool server that lets an LLM agent
(Claude Code, or anything that speaks MCP) operate the classroom robots over the
same contract the dashboard and firmware use — WS-JSON to the hub's ws-adapter,
exactly as the dashboard does.

The robots are ESP32s — they can't host an LLM, so the intelligence runs on the
**hub** and reaches the fleet across the fabric:

```
  Claude Code ──stdio──► hub_mcp.py ──WS-JSON :9001──► ws-adapter ──► zenohd ──► robots
   (on the hub)          (as `operator`)               (browser edge) (router)  (zenoh-pico)
```

**A robot's name is a key address, not a credential.** The hub's own Wi-Fi is
the security boundary: every client — robot or browser — gets full read+write on
`robots/**` and `pair/**` with no username/password at all. `HUB_PASS` matters
for one thing: authenticating as the `operator`, which the ws-adapter requires
before an `estop()` write — and before driving a *claimed* robot (per-owner
claiming, hub#10). Every other tool works fine connected anonymously; this bridge
just authes as the operator so it can also override. No role logic lives in this
server — the adapter enforces it, exactly as it does for the dashboard. And the
bridge speaks WS-JSON to the ws-adapter, **not raw Zenoh**, so all its drive
flows through the hub's one drive path (hub#10 step 5).

## Tools it exposes

Fleet (open to any client on the hub's Wi-Fi — no credential needed):

| tool | topic | what it does |
|------|-------|--------------|
| `fleet()` | `robots/+/sys` | every board online (keyed by board id, with its name) + freshness |
| `drive(robot_id, left_motor, right_motor, duration_ms=400)` | `robots/<id>/pwm` | signed PWM per side (±255, sign = direction); auto-expires after `duration_ms` |
| `stop(robot_id)` | `robots/<id>/pwm` | zero PWM, immediate halt |
| `blink(board)` | `robots/<name>/cmd/identify` | flash a board's LED ~6 s — find the physical robot |
| `read_imu(robot_id, timeout_s=2)` | `robots/<id>/imu` | latest accel/gyro sample, freshness-gated (channel lands with next-gen electronics) |
| `set_led(robot_id, on, red, green, blue)` | `robots/<id>/led` | RGB set via a Zenoh query/reply* |

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

Operator-gated (the one credentialed action — needs `HUB_PASS` set to the
`operator` password):

| tool | topic | what it does |
|------|-------|--------------|
| `estop(engaged=True, reason="")` | `fleet/estop` | fleet-wide emergency stop latch (a hub-held queryable) |

\* The firmware-side queryable reply isn't wired yet (hub#1); until it lands
`set_led` returns `acked: false` on timeout — the LED still changes, only the
confirmation is missing.

## Run

```sh
pip install "mcp[cli]" websockets        # or: pip install -r requirements.txt
HUB_HOST=hub.local python hub_mcp.py                         # anonymous — everything but estop() works
HUB_HOST=hub.local HUB_PASS=<operator-pw> python hub_mcp.py  # authes as operator — adds estop()
```

Environment knobs:

| var | default | note |
|-----|---------|------|
| `HUB_HOST` | `127.0.0.1` | the ws-adapter host (`hub.local` reaches either hub; its DHCP gateway on the hub AP) |
| `HUB_WS_PORT` | `9001` | the ws-adapter's WebSocket port (the fixed dashboard convention) |
| `HUB_USER` | `operator` | the auth role (ignored without a `HUB_PASS`) |
| `HUB_PASS` | *(empty)* | the `operator` code from `/etc/hub/operator.env`; empty = connect anonymous, which is fine for every tool except `estop()` and driving a claimed robot |

## Register with Claude Code

This repo ships a committed **`.mcp.json`** at its root, so opening `hub/` in
Claude Code offers the `hub-fleet` server automatically (you approve it once).
It resolves this script by project-relative path and pulls the operator password
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

So: `pip install -r mcp-bridge/requirements.txt`, `export HUB_PASS=<operator-pw>`,
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

A demo/operator bridge, not a control-loop runtime — best-effort delivery, one
shared `operator` credential, no rate limiting. The `set_led` reply path is
hub#1. For hard-real-time motion, close the loop on the robot; this is for
supervisory, natural-language operation.
