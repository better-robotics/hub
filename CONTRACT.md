# CONTRACT — the hub wire contract

The transport-agnostic message contract every hub — the Pi (`pi/`) and the ESP32
hub role (in `better-robotics/robot`) — and client (`dashboard.html`,
`mcp-bridge/`) agrees on:
envelopes + topic scheme. Currently the imu/pwm/led subset. Planned data
channels, in the order the kits will need them (each lands with an envelope
file when a device first publishes it):

- **`range`** (robot → device) — forward distance. Sensor-agnostic by design:
  the current kit carries an HC-SR04 ultrasonic, the next one a VL53L0X ToF —
  same channel, `sensor` is payload metadata (like `hw` in sys).
- **`cmd_vel`** (device → robot) / **`odom`** (robot → device) — the
  drive-boundary pair for the encoder+IMU kit: velocity setpoints in, derived
  odometry (pose + velocity, fused on the MCU from wheel encoders + IMU) out.
  This *supersedes* the raw `wheel_encoders` channel sketched earlier — the MCU
  owns the PID/odometry math and publishes state, not ticks, matching the ROS 2
  on-ramp (`cmd_vel`/`odom` are its native boundary). `pwm` stays: it is the
  mission-one manual-drive channel, not a deprecation target.

Classroom *scoping*
is not a protocol channel but the broker's ACL identity model
(`pi/mosquitto-acl.example.conf`). The Zenoh
column below is the evaluation baseline (`better-robotics/hub-zenoh`), kept for
comparison.

Identity lives in the topic (`robots/<id>/<channel>`), never the body. The
`rpc_set_led.json` envelope carries no `topic` field on the request side —
the MQTT5 request/response properties (response-topic + correlation-data)
carry that instead, keeping identity-in-the-key. Topic scheme settled
2026-07-08 (see `mosquitto-acl.example.conf`): request on `robots/<id>/led`,
response on the fixed `robots/<id>/led/reply` — a stable pattern rather than a
fully dynamic response-topic, so the broker ACL can scope it. Wiring the
MQTT5 properties themselves (esp-mqtt's `esp_mqtt5_publish_property_config`
on the robot side) is an open thread in the hub state tracker (#4).

| Message | File | Direction | MQTT (both hubs) | Zenoh (baseline) | BLE (workbench) |
|---------|------|-----------|--------------------------|------------------|-----------------|
| IMU sample | `envelopes/imu.json` | robot → device | pub/sub `robots/<id>/imu` | pub/sub `robots/<id>/imu` | — (no IMU in those kits) |
| PWM drive | `envelopes/pwm.json` | device → robot | pub/sub `robots/<id>/pwm` | pub/sub `robots/<id>/pwm` | MOTOR char write |
| set_led (req/resp) | `envelopes/rpc_set_led.json` | device ↔ robot | `robots/<id>/led` req, `robots/<id>/led/reply` resp (MQTT5 correlation-data) | queryable `robots/<id>/led` | LED char (on/off) + RGB char (r,g,b); no reply |
| Fleet e-stop | `envelopes/estop.json` | device → robot (fleet-wide) | pub/sub `fleet/estop`, **retained** | — | — |

Language bindings (which mirror these envelopes): Rust in `pi/src/lib.rs`; the
ESP32 firmware hardcodes the same topics in C.

**Body frame (pwm):** `left_motor` / `right_motor` are the robot's **own** left
and right — stand behind the robot, face the way it drives forward (REP 103's
body frame: x forward, y left). Positive = that wheel rolls forward. Every
client (joystick, tilt, LLM bridge, student code) and every firmware pin map
speaks this frame; a robot that mirrors its turns is wired against it (see the
robot repo's wiring convention), not a reason to flip signs in a client.

### Addressing one board when several share an identity

Identity lives in the topic — but several boards can legitimately answer to one
id, and every fresh board answers to `unassigned` until it is named. They all
receive `robots/unassigned/*`, so any device → robot payload may carry an
optional **`"target": "<board-id>"`** (the `sys` payload's MAC-derived `board`
field, e.g. `robot-b79c`). A board ignores a payload whose `target` names a
different board; a payload with no `target` is accepted by every subscriber.

This applies to **`pwm` as well as `cmd/*`** — `pwm` is the one that bites. On
day one of a class every board on the desk is `unassigned` and subscribed to
`robots/unassigned/pwm`, so an untargeted drive command moves all of them at
once. `dashboard.html` sends `target`; a client that omits it is not addressing
a robot, it is addressing a pool.

Named boards make this moot, which is why it went unnoticed: the failure only
exists in the window before anyone has assigned a name — i.e. exactly the demo.

## The BLE transport (workbench)

[`workbench`](https://github.com/better-robotics/workbench) speaks the same
contract semantics over Web Bluetooth GATT — one characteristic per channel
instead of one topic per channel. The mapping, with the unit differences that
make a blind rename wrong:

- **Drive**: MOTOR char, binary. 4-byte pulse `[l, r, dur_hi, dur_lo]` —
  signed int8 **percent** (±100), big-endian uint16 `duration_ms`; a 2-byte
  `[l, r]` form is the joystick's persistent shape, bounded by a 500 ms
  firmware watchdog. Scale conversion to this contract's `pwm`:
  `duty = percent * 255 / 100`. Same safety floor, same 4000 ms cap
  (`workbench protocol/constants.json` `LLM_MAX_DURATION_MS`).
- **led**: two chars — LED (1 byte on/off) and RGB (3 bytes, 0–255/channel) —
  covering `set_led`'s `{on, red, green, blue}` split in two; write-ack via
  GATT, no application-level reply.
- **`sys` ≈ workbench telemetry**: its TELEMETRY char notifies the same
  vitals JSON shape (`free_heap`, `rssi_dbm`, uptime, per-kit sensors);
  field names already overlap where the hardware does.

**Identity converges by construction**: both projects suffix device names
with the last two MAC bytes as 4 hex digits — `robot-a3f2` here,
`ESP-A3F2` there — so one physical board is recognizable across both
dashboards without a registry.

## Safety floor — every drive self-expires

Enforced in the robot firmware, *below* every client (dashboard joystick,
mcp-bridge, user code) — a malformed or malicious payload cannot bypass it:

- A `pwm` command is a bounded pulse: the firmware stops the motors
  `duration_ms` after the last command (watchdog re-armed per message).
- `duration_ms` absent → **400 ms** default. Non-zero drive with
  `duration_ms <= 0` gets the default too — "no expiry" is not encodable.
- Upper clamp **4000 ms**: an oversized value can't defeat the watchdog. The
  value matches workbench's `LLM_MAX_DURATION_MS`, so one planner-issued
  command is bounded the same on every transport.
- Zero drive (stop) is always honored, any `duration_ms`.

Sustained motion is therefore a *refreshing command stream* — the human
joystick shape (the dashboard republishes while held). A seconds-latency
planner gets one capped pulse per decision; a dropped session coasts to a
stop. (The openpilot-panda layering: safety under the intelligent layer,
never inside it. Enforcement: `robot/src/robot_role.c` `motor_apply`.)

### Fleet e-stop — the retained latch above the per-command floor

The self-expiry above makes every *individual* command safe; `fleet/estop`
is the room-wide latch on top of it, for the moment the instructor needs
everything stopped and **staying** stopped:

- **Topic `fleet/estop`, published retained** (`envelopes/estop.json`;
  `engaged` is the only field the firmware reads — `by`/`reason` are for
  humans on dashboards). Retained is the load-bearing property: a robot that
  reconnects mid-emergency receives the latch on subscribe, so a reboot or
  Wi-Fi blip cannot walk a robot out of an engaged stop.
- **Latch semantics** (firmware, `robot_role.c` `estop_apply`): engaged →
  motors stop now and every non-zero `pwm` is refused until a clear arrives.
  Zero drive (stop) is always honored, engaged or not. The robot reports the
  latch as `"estop":true` in its `sys` beacon while engaged (absent = clear),
  so a fleet view can verify each robot actually heard it.
- **Clear** = retained `{"engaged": false}`. An *empty* retained publish (the
  MQTT idiom for deleting retained state) also reads as clear; any other
  unparseable payload on this topic reads as **engaged** — parse failure
  fails toward stopped.
- The latch is broker-state, not robot-state: a broker restart forgets an
  engaged e-stop (retained store is in-memory on the ESP32 hub). That is the
  intended shape — a hub power-cycle is a room reset, and every drive is
  still individually bounded by the self-expiry floor either way.

Scoping: **read for everyone, write for the instructor.** Anonymous included —
the read-only fleet view must show the engaged banner. On the Pi this is ACL
(`pi/mosquitto-acl.example.conf`); the ESP32 hub has no per-topic ACL, so
there write-restraint is convention, like the rest of its scoping.

## Discovery & isolation — how a client reaches *either* hub

The robot (`better-robotics/robot`) is a raw-TCP MQTT client, so the two hosts
(the Pi hub, and the ESP32 hub role) are **the same broker to it** — same `:1883`, same topics,
same auth. One firmware runs against both. The only host-specific concern is
*finding* the broker, and it resolves to two host-agnostic rules:

- **Discovery = the DHCP gateway.** On any hub AP the gateway *is* the hub, which
  runs the broker → connect to **`<gateway>:1883`**. `hub.local` (mDNS, both hubs
  set hostname `hub`) is the named fallback. **Never a hardcoded IP** — the Pi AP
  defaults to `10.42.0.1` (NetworkManager `shared`), the ESP32 SoftAP to
  `192.168.4.1` (ESP-IDF default); both are overridable, but gateway-discovery
  makes the value irrelevant, so we don't pin it (and `10.0.0.x` specifically
  would risk colliding with the STA uplink's subnet).
- **SSID = `hub-<suffix>`** (suffix from the AP MAC, e.g. `hub-a3f2`). The robot
  scan-joins the strongest open `hub-*`. Single-hub rooms need zero Wi-Fi
  provisioning; multi-hub rooms bind a robot's suffix via BLE Improv.

**No isolation unit — the Wi-Fi perimeter is the isolation** (confirmed
2026-07-13). A robot's name (`robots/<id>/…`) is a topic address, not a
credential: the hub's own Wi-Fi is the classroom's real boundary, so the
whole ACL (`pi/mosquitto-acl.example.conf`) is three top-level rules plus one
user block:

| identity | scope | why |
|----------|-------|-----|
| anonymous — any robot or browser, authenticated or not | `robots/#` rw, `pair/#` rw, `fleet/estop` read | nothing durable is protected by gating drive/read access once the Wi-Fi perimeter is the real boundary — the per-identity password/rotate/pairing machinery this replaced never stopped a determined student from reading a credential off a card, it just made every fresh board a manual provisioning step |
| `instructor` | + `fleet/estop` rw | the one thing the open ACL can't hand out for free: engaging/clearing the room-wide e-stop needs a real credential so a stray keypress can't halt or release the room (§ Fleet e-stop) |

**`pair/#` gets the same open rw as `robots/#`** — a rendezvous namespace for
WebRTC signaling: workbench's phone↔desktop pairing exchanges offer/answer/ICE
over `pair/<room>/…`, then media flows LAN-direct. The signaling transport is
untrusted by design regardless — peers authenticate end-to-end via the ECDSA
P-256 pair ceremony, and rooms are unguessable UUIDs carried by the pairing
QR. The ESP32 hub role grants this for free too (its `connect_cb` admits every
client; only username `instructor` needs a password, and only for
`fleet/estop`).

**Control channels** (`robots/<id>/cmd/*`, device → robot, ad-hoc JSON — no
envelope files; the firmware is the schema): `cmd/config` assigns a board's
name post-join (`{"name":"scout"}` — no password field; a name is an address,
not a credential) — plus an optional `"hub":"hub-XXXX"` **pin**
(trust-on-first-use rogue-hub guard: a pinned board's discovery admits only
that exact SSID, so a student raising their own `hub-*` can't absorb it;
`"hub":""` clears; an SSID pin deters mischief, not a deliberate spoof of the
exact name — that escalation is WPA2 on the hub AP), `cmd/identify` blinks the board's LED (~6 s) so
a physical board can be matched to its on-screen id, `cmd/reprovision` reboots
it (the BOOT button's remote twin). Each payload takes an optional `"target"` —
see § Addressing one board when several share an identity, which covers `pwm`
too.

Directional per-channel rules (imu robot→device, pwm device→robot) are dropped:
they guard a robot spoofing *its own* telemetry — not a classroom threat.
Enforcement is now nearly the same shape on both hosts: the **Pi**'s ACL is
three top-level rules plus one user block; the **ESP32** hub role's
`connect_cb` (`robot/src/hub_role.c`) mirrors it at connect time — admit every
client, check a password only for username `instructor`. MQTT still beats
Zenoh for the same reason as before, narrowed to the one identity that still
needs it: esp-mqtt authenticates with **username/password** natively, the
capability `zenoh-pico` lacked (`robot/CLAUDE.md` usrpwd scar) — without it,
even the single `instructor` credential would have been unenforceable at the
ESP32 hub.

## Captive onboarding — the greeting flow (both hubs)

**This section is the single spec both hubs reconcile to.** It is implemented
twice — the Pi in `pi/src/bin/hubd.rs` + `pi/deploy/hub-ap-setup.sh` (nftables),
the ESP32 hub role in `robot/src/{wifi_portal,dns_server,captive_nat}.c`.
The two share no code (a Linux HTTP server + packet filter vs an ESP-IDF httpd
on a microcontroller), so this table is what keeps them from drifting: a change
lands here first, then in both. It is *values*, not a library — reconcile by
review against this list, not by copying a binary.

When any device joins a hub's own `hub-XXXX` Wi-Fi, its OS immediately fetches a
fixed connectivity-probe URL. The hub answers that probe to drive the OS's own
captive sheet: **not yet greeted → 302 to `/welcome`** (which opens the sheet on
the greeting page, never the dashboard); **greeted (tapped Accept) → each OS's
exact genuine-success signature**, the only thing that makes the OS dismiss its
sheet. Greeting is **per device and uplink-independent** — a hub with a live
internet uplink still greets a joining device, because the sheet is how a phone
without the dashboard bookmark first reaches it. The design floor:

- **Never intercept 443/TLS.** Only plain-HTTP port 80 + DNS (port 53) are
  steered to the hub. A probe over HTTPS is left alone — MITM of a
  publicly-trusted name is neither possible nor wanted; the OSes that force the
  API-over-HTTPS path (below) just fall back to their legacy plain-HTTP probe.
- **Answer all DNS with a short TTL** (5 s) so a released device re-resolves
  promptly instead of caching the hijack.
- **Absent-grace = 90 s.** A greeted device that leaves the AP for longer than
  this loses its greeted state, so its next join is greeted fresh (a reused seat
  in the next class is a new student). Both hubs pin the same 90 s.
- **Uplink self-probe:** `GET http://connectivitycheck.gstatic.com/generate_204`
  — `204` → clean uplink, any other answer → a venue portal is walling it, no
  answer → no uplink. IPv4-only (a broken venue IPv6 would otherwise eat the
  whole timeout).

**The genuine-success table** — what a *greeted* device's probe must receive,
byte-for-byte, per OS. A mismatch on any row leaves that OS's sheet stuck open:

| Probe path | OS | Greeted response |
|---|---|---|
| `/generate_204` | Android / Chrome | `204 No Content`, empty body |
| `/hotspot-detect.html` | Apple | `200`, `text/html`, body `<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>` |
| `/connecttest.txt` | Windows | `200`, `text/plain`, body `Microsoft Connect Test` |
| `/ncsi.txt` | Windows (NCSI) | `200`, `text/plain`, body `Microsoft NCSI` |
| `/success.txt` | Firefox (`detectportal`) | `200`, `text/plain`, body `success\n` (lowercase, trailing newline — exact) |

Each of these paths must be an *explicit* handler on both hubs — a path that
falls through to the catch-all is answered without the greeted check and bounces
a released device back to `/welcome`. The legacy Apple path
`/library/test/success.html` (pre-2015 macOS) is best-effort: the Pi serves it,
the ESP32 leaves it to the catch-all.

A probe path *not* in this table, arriving for a public (non-hub) Host, is still
somebody's probe: greeted → quiet `204`, not-yet-greeted → `302 /welcome`. A
request addressed to the hub *by name* (its IP, `hub.local`, a bare label) keeps
an honest `404` — a typo'd dashboard URL should fail loudly, not bounce home.

**One design, two release layers** (the reason the table is consumed
differently on each hub, and must not be assumed identical in mechanism):

- **Pi — packet-layer release.** `hub-ap-setup.sh`'s `hub-captive` nftables
  table DNATs ports 53 + 80 of *not-yet-greeted* AP clients to the hub; hubd's
  Accept adds the client IP to the `acked` set, which bypasses the DNAT, so a
  greeted device's probes flow to the *real* net. hubd's own genuine-success
  arms are the offline-hub fallback for that case.
- **ESP32 — app-layer release.** `dns_server.c` keeps answering the probe names
  with the hub's own IP even for greeted clients, so the probe always reaches
  the hub; `wifi_portal.c` serves the table above (or the 302) based on
  `captive_accepted()`. `captive_nat.c` only decides packet capture for
  not-yet-greeted clients.

**Captive Portal API (RFC 8908) over DHCP option 114 (RFC 8910):** both hubs
advertise a `/captive`-style JSON endpoint. Apple **requires** that endpoint over
a publicly-trusted HTTPS cert and ignores a plain-HTTP option-114 URI
(confirmed empirically 2026-07-19), so it is a progressive-enhancement layer for
Android/Windows on top of the legacy probe table above — never a replacement for
it. All probe and API responses carry `Cache-Control: no-store`: a cached "still
captive" would strand a released device, a cached "success" would skip greeting
a fresh one.
