# pi/ — Raspberry Pi hub context

The Raspberry Pi implementation of the classroom Robotics Hub. Was the
standalone `sprocket-robotics/hub-mqtt` repo until the 2026-07-08 monorepo merge
(hub-mqtt → `hub`, this directory); MQTT won the transport bake-off and
[`sprocket-robotics/hub-zenoh`](https://github.com/sprocket-robotics/hub-zenoh)
(the evaluation baseline, greenfield origin
[hub-zenoh#4](https://github.com/sprocket-robotics/hub-zenoh/issues/4)) was
archived read-only 2026-07-09 as the baseline record. The transport then
migrated MQTT→Zenoh (the ROS 2 on-ramp won the re-evaluation,
[hub#9](https://github.com/sprocket-robotics/hub/issues/9)); this doc is the Zenoh
hub. Robot firmware: [`sprocket-robotics/robot`](https://github.com/sprocket-robotics/robot).

**hubd is a client of no transport.** The fabric is `zenohd` (the Zenoh router,
`tcp/<gateway>:7447`) plus the **ws-adapter** (the browser edge, `:9001`,
WS-JSON) — each its own on-Pi service beside hubd. Robots and the ESP hub role
speak `zenoh-pico` (the ESP hub role adds a peer-listen endpoint +
`ws_zenoh_bridge.c`, its own copy of the adapter). hubd only serves the dashboard
page (which opens its own WS-JSON connection to the adapter), `/fleet` (the Zenoh
locator), and device-served Wi-Fi setup. Migration decisions and the source
evaluation are in hub#9; the settled spec is `../CONTRACT.md`.

## Source of truth
- **Contract:** `../CONTRACT.md` + `../envelopes/` (monorepo top level) is
  canonical (flipped 2026-07-08, hub#1) — hub-zenoh's copy is frozen with that
  archived repo.
- **Transport: `zenohd` (the Zenoh router) + the ws-adapter, not an embedded
  broker.** The Pi runs the official `zenohd` release — a downloaded standalone
  (`deploy/install.sh` → `/opt/hub/zenoh`, version-pinned to the firmware's
  zenoh-pico, 1.9.0) — plus the `ws-adapter` (`ws-adapter/`, the browser edge,
  its own venv). Robots and the ESP hub role speak `zenoh-pico`. Why Zenoh over
  the MQTT that won the first bake-off: the ROS 2 on-ramp (`cmd_vel`/`odom` are
  Zenoh's native boundary, bridgeable via `zenoh-bridge-ros2dds`), query/reply as
  a first-class RPC primitive (`set_led`, the e-stop latch), and brokerless peer
  discovery — full evaluation in hub#9. Costs a routed transport's scoping work
  on the Pi (a full router routes, so the ws-adapter is made the sole drive path —
  see Permissions), not a reliability one.
- **hubd is a client of no transport** — no MQTT client, no Zenoh session.
  Classroom scoping lives in the ws-adapter + the `zenohd` router ACL, so hubd's
  remaining jobs (dashboard HTML, uplink probe, `/fleet` locator, device-served
  Wi-Fi setup) never needed one.

## Architecture
Three layers; the hub (this repo) is not the middle one for fabric traffic —
hubd is a plain HTTP server sitting *beside* the transport:
- **ESP32 robot** — `zenoh-pico` (Eclipse's C Zenoh, the MCU-sized client;
  version-pinned to the Pi's `zenohd`, 1.9.0). Shipped in
  `sprocket-robotics/robot`'s unified firmware: sys telemetry, pwm drive, and the
  `cmd/config` assign flow all run over zenoh-pico against the hub's endpoint —
  the robot connects to `tcp/<gateway>:7447` (`../CONTRACT.md` § Discovery).
- **`zenohd` + the ws-adapter** (separate processes from hubd) — the transport.
  `zenohd` (`zenoh-router.example.json5`) listens on `tcp/0.0.0.0:7447` for
  robots and the on-Pi adapter; the **ws-adapter** (`ws-adapter/`, a Python
  process beside it) is the browser edge, terminating one WebSocket on `:9001`
  and mapping a small **WS-JSON op protocol** onto its local Zenoh session. Same
  protocol as the ESP hub's `ws_zenoh_bridge.c`, so one dashboard serves both.
- **Device/laptop** — the browser dashboard speaks WS-JSON to the ws-adapter
  over that one WebSocket (no CDN — the classroom Pi may have no internet uplink,
  which is exactly what the uplink probe below exists to detect). The page is a
  genuine **standalone artifact**: download the top-level `../dashboard.html` on
  its own, open it as `file://`, type in a hub address (remembered in
  `localStorage`), and it reaches the adapter directly — a `file://` origin can
  open a plain `ws://` connection with no mixed-content block (unlike an
  `https:`-hosted copy, e.g. GitHub Pages, which would need `wss://` and a
  browser-trusted cert for a dynamic local IP — impractical). Python/Rust sim
  clients point at `tcp/<gateway>:7447` directly (TBD, hub#1 phase 4).

**Naming ladder (inherited from hub-zenoh, settled 2026-07-05):** deployment
context = **classroom / home** — the axis that changes config (ACL, AP,
audience); host hardware (Pi, laptop) named only when the sentence is
literally about the host. The hub is a role, not a device — same hubd on the
Pi appliance, a laptop, someday cloud.

Identity is the topic (`robots/<id>/<channel>`), never a body field. Hub→robot
commands are planned on the **command plane** `robots/<id>/cmd/<verb>` (first
verb in hub-zenoh: `reprovision`) — not yet wired here.

**Fleet HTTP (dashboard):** hubd serves plain HTTP — `/` is the embedded
top-level `../dashboard.html`, `/fleet` just `{uplink, locator}` (the `locator`
is the Zenoh endpoint, `tcp/<host>:7447`). The live per-robot fleet table is
**not** server-aggregated: `dashboard.html` opens its own WS-JSON connection to
the ws-adapter (no login needed) and subscribes `robots/*/sys` directly — the
open floor grants every client `robots/**` rw (open by design). HTTP for the page
itself because the audience is any browser on the hub's network, and because an
`https:`-served page can't open a plain `ws://` connection (mixed content) —
serving the dashboard from the hub's own plain-HTTP origin is what makes the
direct WS-JSON connection possible at all, same reasoning that already ruled out
the public github.io setup page fetching `/fleet` directly.

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
**A Wi-Fi-perimeter model (confirmed 2026-07-13)** — enforced at the
application layer, not per-robot. Zenoh has no broker ACL and zenoh-pico has no
session auth, so scoping lives in the **ws-adapter**
(`ws-adapter/ws_zenoh_adapter.py`) plus the `zenohd` router ACL
(`zenoh-router.example.json5`):

- **Every client — robot or browser, authenticated or not** — gets
  `robots/**` rw and `pair/**` rw, and read on `fleet/estop`. A robot's name
  (`robots/<id>/…`) is a key address, not a credential: the hub's own
  Wi-Fi is the real boundary, so there's nothing left for a per-robot
  password to protect that the perimeter doesn't already cover. Directional
  per-channel rules (imu robot→device, pwm device→robot) stay dropped — they'd
  guard a robot spoofing its OWN telemetry, not a classroom threat.
- **`operator`** — the one gated identity: engaging/clearing the fleet-wide
  emergency stop (`../CONTRACT.md` § Fleet e-stop). zenoh-pico has no usrpwd to
  lean on, so the ws-adapter (and the ESP hub's `ws_zenoh_bridge.c`) accepts an
  `fleet/estop` state-change only after an `{op:auth}` carrying the operator
  code — gating the one action, not the connection, which is *stronger* than a
  whole-session accept. It protects the thing the open floor can't hand out for
  free, so a stray keypress can't halt or release the room.
- **Per-owner claiming (hub#10)** — opt-in exclusivity on top of the open
  floor: a student can *claim* a robot (a physical BOOT-tap window, keyed to an
  opaque browser id) so nobody else drives it; the adapter drops non-zero drive
  to a claimed robot from anyone but the owner or the operator, while a stop
  always passes. Ownership lives only in the adapter, never on the wire; the Pi
  and ESP hub implement it identically. See `ws-adapter/README.md`.
- **Pi only — the ws-adapter is made the sole drive path.** A full `zenohd`
  *routes*, so a raw Zenoh client on the AP could reach a robot around the
  adapter (the ESP hub's zenoh-pico doesn't route, so it has this for free). The
  router ACL denies AP-radio clients (`wlan0`/`wlan1`) writes to the command
  channels (`robots/*/pwm`, `robots/*/cmd/**`, `fleet/estop`); only the on-Pi
  adapter, over loopback, may inject them — after it has applied the per-owner +
  operator logic. The **MCP bridge** rides this same edge (WS-JSON to the adapter
  as an operator, not raw Zenoh), so all drive flows through one place.

This replaced the per-robot-credential model: one `pattern readwrite
robots/%u/#` rule per identity, a `cmd/config`-assigned name+password, an
`unassigned` pool credential, and the Pi's `/codes` HTTP API for
minting/rotating/deleting those credentials and running the knock-and-approve
pairing ceremony — all deleted with the MQTT broker. None of that machinery
enforced anything a determined student couldn't already read off a card; it just
made every fresh board a manual provisioning step. `cmd/config` now only assigns
a board's name (`{"name":"scout"}`, no password field) — a name is an address,
never a credential.

**Stale-firmware diagnostic.** A board still running MQTT firmware (pre-cutover)
can't reach `zenohd` at all — a different protocol on a different port — so it
never joins the fabric and simply never appears in the fleet. Nothing is wrong
with the transport or the ACL: reflash the board. Expect this from any board that
sat out the cutover — the hub migrated in one commit, firmware migrates one flash
at a time. (Scar 2026-07-15, MQTT era: a supermini's *broker* rejection — CONNACK
*not authorized*, because its stale firmware still sent a username the
lone-`operator` passwd file didn't know — was chased as an ACL bug for hours; same
species, a stale-firmware board reads as an infra bug.)

**Where the operator credential lives:**

- `deploy/install.sh` seeds `/etc/hub/operator.env` (`OPERATOR_PASS=change-me`)
  with a placeholder, **only if absent** — re-running install never clobbers a
  rotated one; rotate with a `sed` + `systemctl restart ws-adapter` (the file's
  own install-time comment has the line).
- `/etc/hub/operator.env` is the live truth; the ws-adapter reads `OPERATOR_PASS`
  from it and compares each `{op:auth}` against it.
- **The ESP32 hub keeps its own**: `robot`'s hub role reads the operator code
  from NVS, falling back to the compile-time `OPERATOR_PASS`; set it via the
  portal's `POST /wifi/operator`, no reflash or reboot. Two hubs, two independent
  definitions of one secret — rotate the Pi and the ESP hub still admits its own.
  **The split is structural, not a TODO**: there is no shared store (the two hubs
  are alternatives, rarely on one network). Both now hold the code as plaintext
  (the ws-adapter and the ESP each compare it directly), so don't "fix" it by
  copying a value between them — rotate each hub at its own surface.

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
  (suffix followed the wrong MAC, so robots lost `hub-a2f5`) while the builtin
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
- **AP power save is poison — pinned OFF** (2026-07-10, `3eb8e51`) — the
  fallout of that same single-radio experiment, which left wlan0's power save
  enabled (dmesg stamped it to the minute). Every ESP32 association then
  flapped assoc→drop-before-DHCP for an hour while existing associations slowly
  fell off — a radio that looks up and idle while nothing can stay joined.
  `nmcli con modify hub-ap 802-11-wireless.powersave 2` + bounce recovered it:
  all three boards rejoined within 100 s, self-healing onto the hub.
  `hub-ap-setup.sh` pins it now, so a fresh card can't come up without it.
- **Open AP for now**: ESP32-C3 WPA2 join fails against this AP (4-way
  handshake timeout; open joins in ~6 s). Interop unresolved — see
  `sprocket-robotics/robot` CLAUDE.md.

## Conventions
- **Measured data only** — a real board's IMU omits fields it can't sense; no
  synthetic telemetry on real topics. (Inherited — applies once a robot sim
  exists here.)
- **Identity in the key, not the body** — the `rpc_set_led.json` request carries
  no `key`/`topic` field; the robot declares a **Zenoh queryable** on
  `robots/<id>/led` and a client `get`s it, so query/reply pairs the request to
  its answer with no reply key and no correlation-data (`../CONTRACT.md`). This
  is the native primitive the MQTT5 `response_topic`/`correlation_data` dance
  used to emulate.
- **No relay, ever.** hubd touches no fabric traffic; before adding a Rust call
  site that would, check whether the transport (the `zenohd` ACL, the ws-adapter's
  auth) or the firmware already does the job — that's the lesson of this whole
  redesign (classroom scoping used to be ~80 lines of Rust; it's now the adapter's
  auth check plus a router-ACL file).

## Run
Three processes: `zenohd -c zenoh-router.example.json5` (the router), the
**ws-adapter** beside it (`ZENOH_CONNECT=tcp/127.0.0.1:7447 WS_PORT=9001
OPERATOR_PASS=<code> python3 ws-adapter/ws_zenoh_adapter.py` — see its README; a
self-contained bench with no router can set `ZENOH_LISTEN=` instead), and
`cargo run --bin hubd` (dashboard/HTTP chassis — `HUB_ZENOH_ADDR` tells it what
Zenoh endpoint to advertise to robots/the dashboard as the `/fleet` locator; it
does not bind that endpoint itself). No sim clients exist yet (hub-zenoh's
`robot`/`device`/`watch`/`intruder` bins were deleted rather than left broken).

## Ops (`tools/`)
`tools/deploy-hubd.sh` and `tools/pi-serial.py` are transport-agnostic. The
appliance now runs `zenohd` and the ws-adapter as their own systemd units
(`deploy/zenohd.service`, `deploy/ws-adapter.service`, installed from
`deploy/payload.tsv`). `tools/reprovision.py` is a stub — port it to a Zenoh
publish on `robots/<id>/cmd/reprovision` (via the ws-adapter, or a
`pip install eclipse-zenoh` client) once sim clients land (hub#1 phase 4).
