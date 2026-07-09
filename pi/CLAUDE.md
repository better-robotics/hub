# hub-mqtt — project context

The classroom Robotics Hub, **MQTT transport variant** — the preferred/target
implementation (2026-07-08: MQTT is canonical going forward; Zenoh is kept as
the evaluation baseline it's measured against, not a symmetric contender).
Split off from a sibling
[`better-robotics/hub-zenoh`](https://github.com/better-robotics/hub-zenoh)
(2026-07-08, greenfield evaluation origin:
[hub-zenoh#4](https://github.com/better-robotics/hub-zenoh/issues/4)). A third
variant, hand-rolled WebSocket, lost an earlier round and is kept experimental
at [`better-robotics/hub-ws`](https://github.com/better-robotics/hub-ws).
Rover firmware: [`better-robotics/robot`](https://github.com/better-robotics/robot).

**Broker chosen, no-relay architecture landed (2026-07-08) — see [hub#1](../../issues/1)
for what's left.** hubd is **not an MQTT client**: Mosquitto is the broker,
and every MQTT party (rover firmware, the browser dashboard's `mqtt.js`, sim
clients) talks to it directly, scoped by Mosquitto's own ACL. This repo is a
fork of hub-zenoh's shared chassis (dashboard, uplink probe, BLE provisioning,
Pi image build) with the Zenoh-only session/subscribe/publish call sites
deleted rather than reimplemented — there was nothing to reimplement once the
relay itself was removed.

## Source of truth
- **Contract:** `protocol/` here is canonical (flipped 2026-07-08, hub#1) —
  MQTT is the target implementation; hub-zenoh's copy is now the synced
  evaluation-baseline reference (see `protocol/README.md`).
- **Broker: Mosquitto, not embedded `rumqttd`.** Decided after concrete
  research, not familiarity: `rumqttd` runs MQTT v4 and v5 on **fully separate
  queues** by default (a v4 client and a v5 client never see each other's
  messages on the same topic) — the same class of hardware-discovered interop
  landmine as zenoh-pico's missing `usrpwd` support. Mosquitto is Espressif's
  own tested reference broker for `esp-mqtt` (its examples default to
  `test.mosquitto.org`), has mature MQTT5 request/response support
  (`response_topic`/`correlation_data`, its own `mosquitto_rr` CLI), and
  unifies all protocol versions on one topic space. Costs the single-binary
  property (Mosquitto is a separate process) — a packaging cost, not a
  reliability one, which is where this project has actually been burned
  before.
- **hubd carries no MQTT client library** — not `rumqttc`, not anything.
  Once classroom scoping moved to Mosquitto's broker-native ACL (see below),
  hubd's remaining jobs (dashboard HTML, uplink probe, locator string, BLE
  provisioning) never needed one.

## Architecture
Three layers; the hub (this repo) is no longer the middle one for MQTT
traffic — it's a plain HTTP server sitting *beside* the broker:
- **ESP32 rover** — `esp-mqtt` (first-party ESP-IDF component, actively
  maintained, supports MQTT 3.1.1 and 5.0 natively including
  `esp_mqtt5_publish_property_config`'s `response_topic`/`correlation_data`
  fields — exactly what the RPC binding below needs). Not yet wired into
  `better-robotics/robot`'s firmware (hub#1 phase 5). Note: this is the
  native C component, not the separate Rust `esp-idf-svc` MQTT binding, which
  is v3-only.
- **Mosquitto** (`mosquitto.example.conf`, a separate process from hubd) —
  the actual broker. Raw MQTT on 1883 (rover, sim clients, `mosquitto_pub`/
  `sub`), MQTT-over-WebSocket on 9001 (the browser dashboard's `mqtt.js`,
  connecting directly — no relay).
- **Device/laptop** — the browser dashboard connects with `mqtt.js`, inlined
  directly into `dashboard.html` (2026-07-08, no CDN — the classroom Pi may
  have no internet uplink, which is exactly what the uplink probe below
  exists to detect). Inlining also makes the page a genuine **standalone
  artifact**: download `public/dashboard.html` on its own, open it as
  `file://`, type in a hub address (remembered in `localStorage`), and it
  works with no hubd behind it at all — verified live, a `file://` origin can
  open a plain `ws://` connection with no mixed-content block (unlike an
  `https:`-hosted copy, e.g. GitHub Pages, which would need `wss://` and a
  browser-trusted cert for a dynamic local IP — impractical). Python/Rust sim
  clients TBD (hub#1 phase 4).

**Naming ladder (inherited from hub-zenoh, settled 2026-07-05):** deployment
context = **classroom / home** — the axis that changes config (ACL, AP,
audience); host hardware (Pi, laptop) named only when the sentence is
literally about the host. The hub is a role, not a device — same hubd on the
Pi appliance, a laptop, someday cloud.

Identity is the topic (`robots/<id>/<channel>`), never a body field. Hub→robot
commands are planned on the **command plane** `robots/<id>/cmd/<verb>` (first
verb in hub-zenoh: `reprovision`) — not yet wired here.

**Fleet HTTP (dashboard):** hubd serves plain HTTP — `/` is the embedded
`public/dashboard.html` (mqtt.js inlined — no separate `/mqtt.min.js` route),
`/fleet` just `{uplink, locator}` now. The live per-robot fleet table is **not**
server-aggregated any more: `dashboard.html` opens its own anonymous
`mqtt.js` connection and subscribes `robots/+/sys` directly — Mosquitto's ACL
scopes anonymous clients to that one read-only topic (public by design, same
contract `/fleet`'s `robots` array used to serve). HTTP for the page itself
because the audience is any browser on the hub's network, and because an
`https:`-served page can't open a plain `ws://` connection (mixed content) —
serving the dashboard from the hub's own plain-HTTP origin is what makes the
direct MQTT-over-WS connection possible at all, same reasoning that already
ruled out the public github.io setup page fetching `/fleet` directly. BLE
(Chrome-only Web Bluetooth) stays only where no network exists yet — day-zero
hub setup, home-mode rover.

- **Address:** code default `:8000` (unprivileged dev); the appliance unit
  binds `:80` via `AmbientCapabilities=CAP_NET_BIND_SERVICE`. The classroom
  name is **`http://hub.local`** (avahi/mDNS, hostname `hub`); **`10.42.0.1`**
  is the always-works fallback. Bare **`http://hub`** (a dnsmasq drop-in) was
  dropped 2026-07-08: Apple devices never resolved single-label names anyway
  (verified on iPhone against the hub-esp32 build — `.local` is the reliable
  Apple path), so the dnsmasq-hub-name machinery was a whole moving part
  serving only some Android clients. Trade recorded: an *older* Android with
  no mDNS now falls back to the IP. (hub-zenoh still carries the dnsmasq
  drop-in — mirror the removal there if it earns it.)
- **Uplink probe:** background task, `GET generate_204` (IPv4 only) every
  10 s — 204 → `full`, any other answer → `portal`, none → `none`. Downgrades
  debounced (3 agreeing probes). *Not nmcli*: a DynamicUser UID has no D-Bus
  identity. (Inherited unchanged — transport-agnostic.)
- **Portal UX:** the pill's remediation is free by topology — venue portals
  authorize by MAC, every classroom client shares the hub's venue-side MAC via
  NAT, so any phone joining the hub's Wi-Fi gets the venue sign-in sheet and
  one sign-in unlocks everyone. (Inherited unchanged.)

## Permissions (ACL)
**Designed and verified (2026-07-08)** — broker-native, not a Rust relay.
`mosquitto-acl.example.conf` + `mosquitto-passwd.example` (generated by
`examples/classroom-mosquitto-demo.sh`, which also proves it — three phases
mirroring hub-zenoh's `acl-demo.sh`: authorized paths, cross-team denial, bad
password rejection — all verified live against a real Mosquitto instance,
not just written).

- **Anonymous clients** get read-only `robots/+/sys` only — the public fleet
  view, same contract as the old `/fleet`'s public-read JSON.
- **`rover`** — one shared demo credential for the whole fleet (mirrors
  hub-zenoh's ACL demo pattern: one identity, not per-device). Real per-device
  identity is still open (hub#1 future work), but unlike hub-zenoh, MQTT
  starts from a stricter baseline for free: **`esp-mqtt` supports
  username/password natively**, so there's no equivalent of zenoh-pico's
  missing-`usrpwd` gap forcing an open router.
- **`professor`** — read `robots/#`, write `robots/+/pwm` and `robots/+/led`
  (matches hubd's old `Scope::All`).
- **Each team** (`classroom.example.json5`) — read `robots/<their-robot>/#`,
  write only `pwm`/`led` on their own robot (matches the old `Scope::One`).
  **MQTT username IS the identity, and for a team it's always that team's
  robot id** — no separate token/robot mapping, so `dashboard.html` can infer
  scope from the username alone. The "code" a browser types is
  `username:password` (e.g. `team1:change-me-team1`); professor's username is
  always literally `"professor"`.

`classroom.example.json5`'s role changed accordingly: it's no longer loaded
by any binary, just the human-readable intent that the ACL/passwd files
implement by hand (same split as hub-zenoh's `protocol/README` vs
`zenoh-acl.example.json5`) — keep all three in sync on touch.

## Hub-AP mode (inherited from hub-zenoh, live on the classroom Pi since 2026-07-04)
Not transport-specific — this is Pi/Wi-Fi-radio topology, unchanged here.
Topology is canonical in hub-zenoh's README § "Network: the hub is the access
point" (wlan0 AP `hub-XXXX` / wlan1 STA uplink, NM `ipv4.method=shared`,
gateway 10.42.0.1 = constant locator). Scars:
- **brcmfmac (built-in) is the reliable AP; the Edimax RTL8188CUS is not** —
  the dongle takes the STA leg.
- **Open AP for now**: ESP32-C3 WPA2 join fails against this AP (4-way
  handshake timeout; open joins in ~6 s). Interop unresolved — see
  `better-robotics/robot` CLAUDE.md.

## BLE verification scar (inherited from hub-zenoh, 2026-07-04)
Not transport-specific — this is about `provisiond`'s BLE stack, unchanged
here. **macOS is a dishonest BLE observer** — CoreBluetooth misses whole 8 s
scan windows and serves stale names/identities from its cache. A "device
vanished" read from a Mac scan is evidence of nothing. Honest tests: for
rovers, `sudo btmgmt find` on the Pi; for the hub's own adv, repeated Mac scan
windows — any hit means alive.

## Conventions
- **Measured data only** — a real board's IMU omits fields it can't sense; no
  synthetic telemetry on real topics. (Inherited — applies once a rover sim
  exists here.)
- **Identity in the topic, not the body** — mirrors hub-zenoh's
  identity-in-the-key convention. The `rpc_set_led.json` request carries no
  `topic` field; MQTT5's `response_topic`/`correlation_data` properties (on a
  fixed `robots/<id>/led/reply` pattern, not a fully dynamic topic — see
  `protocol/README.md`) keep that holding without the queryable primitive
  Zenoh has.
- **No relay, ever, for MQTT-native jobs.** Before adding a Rust call site
  that touches MQTT pub/sub, check whether Mosquitto's own ACL/broker
  features already do the job — that's the lesson of this whole redesign
  (classroom scoping used to be ~80 lines of Rust; it's now a broker config
  file).

## Run
Two processes, not one: `mosquitto -c mosquitto.example.conf` (broker;
`examples/classroom-mosquitto-demo.sh` generates the passwd file first) and
`cargo run --bin hubd` (dashboard/HTTP chassis — `HUB_MQTT_ADDR` tells it what
broker address to report to rovers/the dashboard; it does not bind that
address itself). No sim clients exist yet (hub-zenoh's `rover`/`device`/
`watch`/`intruder` bins were Zenoh-specific and were deleted rather than left
broken).

## Ops (`tools/`)
`tools/deploy-hubd.sh` and `tools/pi-serial.py` are transport-agnostic,
unchanged from hub-zenoh — though the Pi image/systemd unit will need a
second unit for Mosquitto (not yet done; hub#1). `tools/reprovision.py` is a
stub — it used a Zenoh Python client (`pip install eclipse-zenoh`) in
hub-zenoh; port it to an MQTT client library once sim clients land (hub#1
phase 4).
