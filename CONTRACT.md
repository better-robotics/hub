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

Classroom/team *scoping*
is not a protocol channel but the broker's ACL identity model
(`pi/mosquitto-acl.example.conf` + `pi/classroom.example.json5`). The Zenoh
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
on the rover side) is an open thread in the hub state tracker (#4).

| Message | File | Direction | MQTT (both hubs) | Zenoh (baseline) | BLE (workbench) |
|---------|------|-----------|--------------------------|------------------|-----------------|
| IMU sample | `envelopes/imu.json` | robot → device | pub/sub `robots/<id>/imu` | pub/sub `robots/<id>/imu` | — (no IMU in those kits) |
| PWM drive | `envelopes/pwm.json` | device → robot | pub/sub `robots/<id>/pwm` | pub/sub `robots/<id>/pwm` | MOTOR char write |
| set_led (req/resp) | `envelopes/rpc_set_led.json` | device ↔ robot | `robots/<id>/led` req, `robots/<id>/led/reply` resp (MQTT5 correlation-data) | queryable `robots/<id>/led` | LED char (on/off) + RGB char (r,g,b); no reply |

Language bindings (which mirror these envelopes): Rust in `pi/src/lib.rs`; the
ESP32 firmware hardcodes the same topics in C.

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
with the last two MAC bytes as 4 hex digits — `rover-a3f2` here,
`ESP-A3F2` there — so one physical board is recognizable across both
dashboards without a registry.

## Safety floor — every drive self-expires

Enforced in the rover firmware, *below* every client (dashboard joystick,
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
never inside it. Enforcement: `robot/src/rover_role.c` `motor_apply`.)

## Discovery & isolation — how a client reaches *either* hub

The rover (`better-robotics/robot`) is a raw-TCP MQTT client, so the two hosts
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
- **SSID = `hub-<suffix>`** (suffix from the AP MAC, e.g. `hub-a3f2`). The rover
  scan-joins the strongest open `hub-*`. Single-hub rooms need zero Wi-Fi
  provisioning; multi-hub rooms bind the team's suffix via BLE Improv.

**Isolation unit = `robots/<id>`** — a team owns its subtree, and that is the
whole ACL:

| identity | scope | why |
|----------|-------|-----|
| team | `robots/<id>/#` rw | drive/read only your own rover |
| `unassigned` | `robots/unassigned/#` rw | the fresh-board pool: the firmware's flash-time default identity; no student holds this credential, so only the professor can drive a board nobody has assigned yet |
| professor | `robots/#` rw | oversight + drive any |
| anonymous | `robots/#` read | the read-only fleet view (dashboard) |

**Control channels** (`robots/<id>/cmd/*`, device → robot, ad-hoc JSON — no
envelope files; the firmware is the schema): `cmd/config` assigns a board's
team/name/motor-pins post-join — plus an optional `"hub":"hub-XXXX"` **pin**
(trust-on-first-use rogue-hub guard: a pinned board's discovery admits only
that exact SSID, so a student raising their own `hub-*` can't absorb it;
`"hub":""` clears; an SSID pin deters mischief, not a deliberate spoof of the
exact name — that escalation is WPA2 on the hub AP), `cmd/identify` blinks the board's LED (~6 s) so
a physical board can be matched to its on-screen id, `cmd/reprovision` reboots
it (the BOOT button's remote twin). Boards sharing one identity all see these
topics, so each payload takes an optional `"target": "<board-id>"` (the sys
payload's MAC-derived `board` field) to address exactly one.

Directional per-channel rules (imu robot→device, pwm device→robot) are dropped:
they guard a team spoofing *its own* rover's telemetry — not a classroom threat.
Enforcement differs by host, ownership model does not: the **Pi** enforces this
per-topic ACL; the **ESP32** has no per-topic ACL (connect-only `connect_cb`), so
there isolation is team-level connect-auth + rover convention (each rover only
subscribes its own id). Per-team identity is real on both because the rover
authenticates with **username/password** over `esp-mqtt` — the capability
`zenoh-pico` lacked (`robot/CLAUDE.md` usrpwd scar), and the concrete reason the
MQTT transport, not Zenoh, is what the rover ships on.
