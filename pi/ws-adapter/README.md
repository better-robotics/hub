# ws-adapter — the browser edge of the Pi Zenoh hub

The Pi-side of the MQTT→Zenoh migration's browser tier (tracked in
[`#9`](https://github.com/better-robotics/hub/issues/9); wire spec:
`../../zenoh-migration.md`). A browser can't speak native Zenoh, so the dashboard
speaks a small **WS-JSON op protocol** over one WebSocket and this process maps it
onto a zenoh session beside `zenohd`. It is the Python sibling of the ESP
firmware's `ws_zenoh_bridge.c` — same protocol, so **one dashboard serves both
tiers**:

| Client → adapter | Meaning |
|---|---|
| `{op:"sub", key}` / `{op:"unsub", key}` | declare/drop a per-client key filter |
| `{op:"pub", key, val}` | `session.put` (a `fleet/estop` write is gated on auth) |
| `{op:"get", key, val, id}` → `{op:"reply", id, val}` | `session.get` (set_led, e-stop latch) |
| `{op:"auth", password}` → `{op:"auth", ok}` | the one operator gate |
| adapter → client: `{key, val}` | a delivered subscription sample |

The hub owns the `fleet/estop` latch: an authed estop pub updates it and a
queryable answers a (re)joining rover's join-time `get` — the retained MQTT
message, as a query.

## Run (beside `zenohd`)

```sh
pip install eclipse-zenoh websockets
ZENOH_CONNECT=tcp/127.0.0.1:7447 WS_PORT=9001 OPERATOR_PASS=<the classroom code> \
  python3 ws_zenoh_adapter.py
```

`ZENOH_CONNECT` points at the local `zenohd` (client mode). For a self-contained
bench (no router) set `ZENOH_LISTEN=tcp/127.0.0.1:7447` instead to run a peer with
its own listen endpoint. The dashboard connects to `ws://<host>:9001` — the same
`wsPort` convention the MQTT WS bridge used, so `dashboard.html` reaches this
unchanged.

## Authorization (deliberate: the Wi-Fi perimeter is the boundary)

The adapter reproduces the hub's Mosquitto ACL exactly (`pi/CLAUDE.md` §
Permissions; `../../CONTRACT.md` § Discovery & isolation): **everything under
`robots/**` and `pair/**` is open** to any client, authenticated or not — a
robot's name is a topic address, not a credential, so a per-topic gate would
protect nothing the hub's own Wi-Fi doesn't already. The **one** gated action is
engaging/clearing `fleet/estop`, which requires `{op:auth}` with the operator
code — deliberate friction so a stray tap can't halt or release the room. This is
the same posture as the ESP hub (`ws_zenoh_bridge.c`) and the broker it replaces;
gating `cmd/*` here would diverge from the contract, not harden it. `OPERATOR_PASS`
is a placeholder to rotate at deploy — an unset value warns loudly on startup
rather than silently admitting the public default.

## Validated (2026-07-21)

Browser-tested end-to-end against a real `dashboard.html` (the `zenohTransport`
swap) with a local zenoh peer + a test rover: the fleet card rendered from
telemetry, the e-stop banner armed from the latch query, operator sign-in
unlocked controls via `{op:auth}`, and an authed estop-clear + a joystick drive
both reached the rover. No Wi-Fi join — the browser hit `ws://localhost:9001`.
