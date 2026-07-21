# Zenoh migration — the wire contract on Zenoh

A design note for the MQTT→Zenoh transport migration. **Live state and decisions
belong in the tracking issue ([#9](https://github.com/better-robotics/hub/issues/9));
this file is the durable target spec, not a status page.** It folds into
`CONTRACT.md` as the implementation lands (M2–M5) — until then `CONTRACT.md`
stays truthful that MQTT/Mosquitto is the *live* transport, and this doc is where
the Zenoh shape is settled ahead of code. Precedent: `dashboard-redesign.md`.

Why Zenoh: the ROS 2 on-ramp (`cmd_vel`/`odom` are Zenoh's native boundary,
bridgeable to real ROS 2 via `zenoh-bridge-ros2dds`), plus brokerless peer
discovery and multicast efficiency. Decision and the full source evaluation
that shaped this spec are in #9.

## What does NOT change

The migration is a **transport swap, not a semantics change.** These are carried
verbatim:

- **The envelopes** (`envelopes/*.json`) — `imu`, `pwm`, `estop`, `rpc_set_led`
  ride unchanged as the payloads; only their carrier changes.
- **Identity lives in the key**, never the body (`robots/<id>/<channel>`), and the
  optional **`target`** field still disambiguates several boards sharing one id.
- **The safety floor** — per-command `pwm` self-expiry watchdog (400 ms default,
  4000 ms cap), enforced in firmware below every client. Unchanged and still the
  reason a freshly-joined robot is stopped by default.
- **The Wi-Fi perimeter is the boundary.** A robot's name is an address, not a
  credential; the hub's own Wi-Fi is the classroom's real isolation.
- **Captive onboarding** (§ CONTRACT.md) — pure HTTP/DNS, transport-independent,
  untouched.

## Key expressions

Zenoh key-expressions are `/`-separated hierarchical keys — the same shape as the
MQTT topics — so the scheme maps directly:

| Channel | Direction | MQTT topic | Zenoh key expression | Carrier |
|---|---|---|---|---|
| IMU sample | robot → device | `robots/<id>/imu` | `robots/<id>/imu` | pub/sub |
| PWM drive | device → robot | `robots/<id>/pwm` | `robots/<id>/pwm` | pub/sub |
| sys beacon | robot → device | `robots/<id>/sys` | `robots/<id>/sys` | pub/sub |
| set_led | device ↔ robot | `robots/<id>/led` + `…/led/reply` | `robots/<id>/led` | **queryable** (§ below) |
| Fleet e-stop | device → robot | `fleet/estop` (retained) | `fleet/estop` | pub/sub + **queryable latch** (§ below) |
| control | device → robot | `robots/<id>/cmd/*` | `robots/<id>/cmd/*` | pub/sub |
| pairing | device ↔ device | `pair/#` | `pair/**` | pub/sub |

Note the wildcard change: MQTT `#` → Zenoh `**` (multi-level), MQTT `+` → Zenoh
`*` (single-level).

**ROS 2 alignment.** The `robots/<id>/<channel>` scheme is already ROS-2-namespace
shaped: a ROS 2 node in namespace `/robots/<id>` publishing `cmd_vel`/`odom` maps,
through `zenoh-bridge-ros2dds`, onto `robots/<id>/cmd_vel` — so the planned
`cmd_vel`/`odom` drive-boundary channels bridge to real ROS 2 as **bridge
configuration, not a rewrite**. *To verify against the bridge's exact key-mangling
during M2/M3* — the bridge applies type/QoS mangling in some configurations; the
principle (our slash-keys are namespace-shaped) holds, the exact string is a
bench check, not an assumption.

## `set_led` → queryable (native RPC)

MQTT modelled request/response with a second topic (`…/led/reply`) and MQTT5
correlation-data. Zenoh has query/reply as a first-class primitive, so this gets
*simpler*:

- The **robot declares a queryable** on `robots/<id>/led`.
- A client issues `get(robots/<id>/led, payload = rpc_set_led.request)` and
  receives the reply as the query's reply sample —
  `rpc_set_led.response_ok` / `response_err`.
- No reply topic, no correlation-data: the query/reply pairing carries the
  correlation itself. The `rpc_set_led.json` envelope is unchanged.

This is the pattern the whole contract reuses for request/response, and
`fleet/estop`'s current-state read (below) rides the same primitive.

## `fleet/estop` → hub-owned queryable latch

The retained-message latch becomes a **hub-owned queryable + live pub/sub**
(full design + rejected alternatives in [#9's M1 e-stop note](https://github.com/better-robotics/hub/issues/9)).
The invariant is unchanged: *no robot drives while the room is in an engaged
e-stop, including a robot that reboots or reconnects after it was engaged.*

- The **hub is the e-stop authority** (it already held the latch in memory). It
  **publishes** transitions on `fleet/estop` for live subscribers, and **answers a
  query** on `fleet/estop` for current state.
- A robot on boot/reconnect **declares its subscriber first, then `get`s** the
  current latch from the hub — closing the race window — then follows live updates.
- Implemented one abstraction, two tiers: a Zenoh **storage**
  (`zenoh-plugin-storage-manager`) on the Pi; an **application-level queryable** on
  the ESP hub (reusing the `set_led` queryable pattern — no unstable
  `Z_FEATURE_ADVANCED_PUBLICATION`).
- Preserved: operator-only engage/clear (app-layer auth, § below); robot refuses
  non-zero `pwm` while engaged, always honors zero-drive; `"estop":true` in the
  `sys` beacon; parse-failure fails toward stopped; **hub reboot forgets the latch =
  intended room reset** (state is hub-local).

## Discovery — a star through the hub

The topology is a **star**: robots and the dashboard all talk *to/through the hub*;
no robot↔robot traffic. This matters because zenoh-pico peer mode does not forward
between peers — but it never needs to here. The hub's *own* publications reach every
connected robot, and robot publications reach the hub's *own* subscriptions; the
hub is the origin for downlink (`pwm`, `estop`, `cmd/*`) and the sink for uplink
(`imu`, `sys`), bridging uplink onward to the dashboard.

Finding the hub is the same rule as today, minus the MQTT port:

- **Discovery = the DHCP gateway.** On any hub AP the gateway *is* the hub →
  connect to the hub's Zenoh endpoint at **`tcp/<gateway>:7447`** (Zenoh's default
  port). `hub.local` (mDNS) is the named fallback. Never a hardcoded IP.
- **SSID = `hub-<suffix>`**, scan-join the strongest open `hub-*` — unchanged.
- Robot session mode (client-to-hub vs. peer-with-hub-listen) is an M2/M3 bench
  call; client-to-hub is the simplest star and the working assumption. The Pi runs
  full `zenohd` (a listen endpoint); the ESP hub runs `zenoh-pico` with a TCP
  listen endpoint (`_z_tcp_esp32_listen`/`accept` — confirmed present).

## Scoping → application-layer operator auth

The MQTT ACL (anonymous `rw` on `robots/**`+`pair/**`, `operator` adds
`fleet/estop` write) becomes:

- **Open read+write for everyone** on `robots/**` / `pair/**` — nothing durable is
  protected once the Wi-Fi perimeter is the boundary; unchanged intent.
- **`operator` is the one gated identity**, and its only power is engaging/clearing
  `fleet/estop`. zenoh-pico has no usrpwd (the keys are inert), so this is enforced
  at the **application layer**: the hub accepts an e-stop state-change only from an
  authenticated operator (validated in the browser adapter / `hub_role`), which is
  *stronger* than the MQTT `connect_cb`'s whole-session accept. The Pi may
  additionally use `zenohd`'s native access-control as defense-in-depth.

## The browser edge — a WS-JSON adapter

Browsers cannot speak native Zenoh (zenoh-pico's WS link is Emscripten-only and
CMake-gated off the ESP; `zenoh-ts` needs a Rust `zenohd`+remote-api). The browser
was always a bridged edge — it reached MQTT over a WebSocket too. So the dashboard
speaks a small **WS-JSON adapter protocol** to whichever hub it's on; the hub maps
it onto its local Zenoh session. **UI and JSON envelopes are unchanged — only the
dashboard's transport calls swap** (out of `mqtt.js`).

On the ESP hub this reuses `ws_mqtt_bridge.c`'s existing WebSocket termination and
bounded-slot management (it already does the real RFC 6455 handshake for `mqtt.js`);
the byte-pump is replaced with the command surface below. The Pi hub exposes the
same surface (a small process beside `zenohd`) so the dashboard has one transport
across both tiers.

Adapter surface (JSON over one WebSocket):

| Client → hub | Meaning |
|---|---|
| `{op:"sub", key}` / `{op:"unsub", key}` | declare/undeclare a subscriber on the hub session |
| `{op:"pub", key, val}` | publish an envelope (`val`) on `key` |
| `{op:"get", key, val}` → reply | query (set_led, e-stop current-state); reply returns the sample |
| `{op:"auth", role:"operator", password}` | authenticate before e-stop engage/clear |

| Hub → client | Meaning |
|---|---|
| `{key, val}` | a delivered subscription sample |
| `{op:"reply", id, val}` | a query reply |
| `{op:"auth", ok}` | auth result |

Keys carry the same `robots/<id>/<channel>` scheme; `val` is the same envelope the
dashboard already builds. The tree, the wire log, the rendered↔raw toggle — all
unchanged, because a topic is still an address and a message is still an envelope.

## Open / to-verify (gates for M2–M3)

- **ros2dds exact key-mangling** — bench against `zenoh-bridge-ros2dds`; confirm
  `robots/<id>/cmd_vel` round-trips to a ROS 2 `/robots/<id>/cmd_vel`.
- **Robot session mode** — client-to-hub vs. peer-with-listen; measure reconnect
  behavior and the star-forwarding assumption on real hardware.
- **ESP relay + WS-JSON adapter round-trip** — the one M3 spike (hub subscribes and
  republishes; adapter maps browser↔session).
- **`fleet/estop` queryable latch** — verify join-time `get` delivers the engaged
  latch to a rebooting robot before it accepts any drive.
