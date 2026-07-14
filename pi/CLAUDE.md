# pi/ — Raspberry Pi hub context

The Raspberry Pi implementation of the classroom Robotics Hub. Was the
standalone `better-robotics/hub-mqtt` repo until the 2026-07-08 monorepo merge
(hub-mqtt → `hub`, this directory); MQTT won the transport bake-off and
[`better-robotics/hub-zenoh`](https://github.com/better-robotics/hub-zenoh)
(the evaluation baseline, greenfield origin
[hub-zenoh#4](https://github.com/better-robotics/hub-zenoh/issues/4)) was
archived read-only 2026-07-09 as the baseline record.
Rover firmware: [`better-robotics/robot`](https://github.com/better-robotics/robot).

**Broker chosen, no-relay architecture landed (2026-07-08) — see [hub#1](../../issues/1)
for what's left.** hubd is **not an MQTT client**: Mosquitto is the broker,
and every MQTT party (rover firmware, the browser dashboard's `mqtt.js`, sim
clients) talks to it directly, scoped by Mosquitto's own ACL. This repo is a
fork of hub-zenoh's shared chassis (dashboard, uplink probe, device-served
Wi-Fi setup, Pi image build) with the Zenoh-only session/subscribe/publish call sites
deleted rather than reimplemented — there was nothing to reimplement once the
relay itself was removed.

## Source of truth
- **Contract:** `../CONTRACT.md` + `../envelopes/` (monorepo top level) is
  canonical (flipped 2026-07-08, hub#1) — hub-zenoh's copy is frozen with that
  archived repo.
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
  hubd's remaining jobs (dashboard HTML, uplink probe, locator string,
  device-served Wi-Fi setup) never needed one.

## Architecture
Three layers; the hub (this repo) is no longer the middle one for MQTT
traffic — it's a plain HTTP server sitting *beside* the broker:
- **ESP32 rover** — `esp-mqtt` (first-party ESP-IDF component, actively
  maintained, supports MQTT 3.1.1 and 5.0 natively including
  `esp_mqtt5_publish_property_config`'s `response_topic`/`correlation_data`
  fields — exactly what the RPC binding below needs). Shipped in
  `better-robotics/robot`'s unified firmware (2026-07-09): sys telemetry,
  pwm drive, and the `cmd/config` assign flow all run over esp-mqtt against
  this broker. Note: this is the native C component, not the separate Rust
  `esp-idf-svc` MQTT binding, which is v3-only.
- **Mosquitto** (`mosquitto.example.conf`, a separate process from hubd) —
  the actual broker. Raw MQTT on 1883 (rover, sim clients, `mosquitto_pub`/
  `sub`), MQTT-over-WebSocket on 9001 (the browser dashboard's `mqtt.js`,
  connecting directly — no relay).
- **Device/laptop** — the browser dashboard connects with `mqtt.js`, inlined
  directly into `dashboard.html` (2026-07-08, no CDN — the classroom Pi may
  have no internet uplink, which is exactly what the uplink probe below
  exists to detect). Inlining also makes the page a genuine **standalone
  artifact**: download the top-level `../dashboard.html` on its own, open it as
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
top-level `../dashboard.html` (mqtt.js inlined — no separate `/mqtt.min.js`
route), `/fleet` just `{uplink, locator}` now. The live per-robot fleet table
is **not** server-aggregated any more: `dashboard.html` opens its own anonymous
`mqtt.js` connection and subscribes `robots/+/sys` directly — Mosquitto's ACL
grants anonymous clients read-only `robots/#` (public by design, same
contract `/fleet`'s `robots` array used to serve). HTTP for the page itself
because the audience is any browser on the hub's network, and because an
`https:`-served page can't open a plain `ws://` connection (mixed content) —
serving the dashboard from the hub's own plain-HTTP origin is what makes the
direct MQTT-over-WS connection possible at all, same reasoning that already
ruled out the public github.io setup page fetching `/fleet` directly.

**Wi-Fi setup is device-served** (replaced BLE/Improv provisioning, deleted
2026-07-09). hubd exposes `GET /wifi/scan`, `GET /wifi/status`, `POST
/wifi/connect` (see `src/wifi.rs` — nmcli glue); the dashboard's "Set up Wi-Fi"
panel drives them and hides when the page isn't hub-served. A phone joins the
hub's own `hub-XXXX` AP, opens `http://hub.local`, and picks the uplink network
there — no hosted website, no Web Bluetooth, works on iOS. The join is pinned to
the uplink radio, never the AP's (the 2026-07-04 outage lesson; see
`uplink_device` in `src/wifi.rs`).

- **Address:** code default `:8000` (unprivileged dev); the appliance unit
  binds `:80` via `AmbientCapabilities=CAP_NET_BIND_SERVICE`. The classroom
  name is **`http://hub.local`** (avahi/mDNS, hostname `hub`); **`10.42.0.1`**
  is the always-works fallback. Bare **`http://hub`** (a dnsmasq drop-in) was
  dropped 2026-07-08: Apple devices never resolved single-label names anyway
  (verified on iPhone against the hub-esp32 build — `.local` is the reliable
  Apple path), so the dnsmasq-hub-name machinery was a whole moving part
  serving only some Android clients. Trade recorded: an *older* Android with
  no mDNS now falls back to the IP.
- **Uplink probe:** background task, `GET generate_204` (IPv4 only) every
  10 s — 204 → `full`, any other answer → `portal`, none → `none`. Downgrades
  debounced (3 agreeing probes). *Not nmcli* — deliberately: hubd runs root now
  and could ask NM, but a raw self-probe tests the path packets actually take,
  not NM's opinion of it. (Probe logic inherited unchanged — transport-agnostic.)
- **Portal UX:** the pill's remediation is free by topology — venue portals
  authorize by MAC, every classroom client shares the hub's venue-side MAC via
  NAT, so any phone joining the hub's Wi-Fi gets the venue sign-in sheet and
  one sign-in unlocks everyone. (Inherited unchanged.)

## Permissions (ACL)
**Designed and verified (2026-07-08)** — broker-native, not a Rust relay.
`mosquitto-acl.example.conf` + `mosquitto-passwd.example` (generated by
`examples/classroom-mosquitto-demo.sh`, which also proves it — three phases
mirroring hub-zenoh's `acl-demo.sh`: authorized paths, cross-robot denial, bad
password rejection — all verified live against a real Mosquitto instance,
not just written).

- **Anonymous clients** — read-only `robots/#` (top-level rule, no
  `user`/`pattern` block): the public fleet view, same contract as the old
  `/fleet`'s public-read JSON.
- **Each robot** — one `pattern readwrite robots/%u/#` rule: an authenticated
  identity owns exactly the subtree named by its username, and BOTH the
  robot's browser and its rover connect as that identity (**username IS the
  robot's own name** — hardware MAC is telemetry metadata, never a name). No
  per-robot blocks, no edits as the class grows; provisioning the credential
  is the only step. Directional per-channel rules (imu robot→device, pwm
  device→robot) were deliberately dropped — they'd guard a robot spoofing its
  OWN telemetry, not a classroom threat. The "code" a browser types is
  `username:password` (e.g. `scout:change-me-scout`).
- **`professor`** — `readwrite robots/#`, the only named user block.
- Rovers get their name credential post-join over `robots/<name>/cmd/config`
  (published from the dashboard's assign panel, target-filtered by board id);
  until then a fresh board authenticates as **`unassigned`** — the pool
  identity (seeded in install.sh/the image; no student holds it, so only the
  professor can drive an unassigned board) —
  `esp-mqtt` supports username/password natively, so there's no equivalent of
  zenoh-pico's missing-`usrpwd` gap forcing an open broker.

`classroom.example.json5`'s role changed accordingly: it's no longer loaded
by any binary, just the human-readable intent that the ACL/passwd files
implement by hand (same split as hub-zenoh's `protocol/README` vs
`zenoh-acl.example.json5`) — keep all three in sync on touch.

## Hub-AP mode (live on the classroom Pi since 2026-07-04)
Not transport-specific — this is Pi/Wi-Fi-radio topology: wlan0 AP `hub-XXXX`
/ wlan1 STA uplink, NM `ipv4.method=shared`, gateway 10.42.0.1 = constant
locator. (Original record: archived hub-zenoh README § "Network: the hub is
the access point".) Baked into the image since 2026-07-10:
`deploy/hub-ap-setup.sh` + `hub-ap.service` create the NM profile on first
boot (idempotent; suffix from wlan0's MAC) — **hardware-verified 2026-07-10**
(first real-Pi flash: `hub-a2f5` on the air, dashboard + broker up; the boot
also caught two usb0 recovery-link bugs, see `image/README.md` § First
hardware boot). Scars:
- **brcmfmac (built-in) is the reliable AP; the Edimax RTL8188CUS is not** —
  the dongle takes the STA leg.
- **Radio roles are selected by driver, never by interface name** (hardware-
  discovered 2026-07-10, second flash): wlan0/wlan1 is a per-boot kernel
  enumeration coin flip between the SDIO builtin and the USB dongle. The v2
  image's first boot lost it — the AP came up on the Edimax as `hub-e959`
  (suffix followed the wrong MAC, so rovers lost `hub-a2f5`) while the builtin
  took the uplink. `hub-ap-setup.sh` now picks the brcmfmac by driver and
  self-heals a wrong-radio profile; the capport dnsmasq option tags `!usb0`
  instead of `wlan0`; `uplink_device()` was already role-based (it dodged the
  bug by avoiding the AP's *device*, not a name).
- **Single-radio AP+STA (dropping the dongle) is measured-out, not assumed-out**
  (bench 2026-07-10). brcmfmac supports 1 AP + 1 STA (`iw phy`: `#channels <= 1`),
  but live against three AP clients: the STA leg's off-channel *scanning alone*
  starved the AP (repeated 1.5–5.6 s gaps), a join to the uplink's different
  channel failed outright, the retry "succeeded" incoherently (STA ch 52 vs AP
  ch 6, kernel `chanspec failed (-52)`), and the run totaled ~47% AP-client loss
  with a 17.9 s worst gap. Control on the two-radio split: a full uplink
  disconnect+reconnect cost AP clients 3% loss / 0.42 s max gap. Two radios stay.
- **Open AP for now**: ESP32-C3 WPA2 join fails against this AP (4-way
  handshake timeout; open joins in ~6 s). Interop unresolved — see
  `better-robotics/robot` CLAUDE.md.

## Conventions
- **Measured data only** — a real board's IMU omits fields it can't sense; no
  synthetic telemetry on real topics. (Inherited — applies once a rover sim
  exists here.)
- **Identity in the topic, not the body** — mirrors hub-zenoh's
  identity-in-the-key convention. The `rpc_set_led.json` request carries no
  `topic` field; MQTT5's `response_topic`/`correlation_data` properties (on a
  fixed `robots/<id>/led/reply` pattern, not a fully dynamic topic — see
  `../CONTRACT.md`) keep that holding without the queryable primitive
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
