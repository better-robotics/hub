# CONTRACT — the hub wire contract

The transport-agnostic message contract every hub — the Pi (`pi/`) and the ESP32
hub role (in `better-robotics/robot`) — and client (`dashboard.html`,
`mcp-bridge/`) agrees on:
envelopes + topic scheme. Currently the imu/pwm/led subset. `wheel_encoders`
is a planned robot→device channel (no envelope yet); classroom/team *scoping*
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
on the rover side) is still hub#1 phase 3.

| Message | File | Direction | MQTT (both hubs) | Zenoh (baseline) |
|---------|------|-----------|--------------------------|------------------|
| IMU sample | `envelopes/imu.json` | robot → device | pub/sub `robots/<id>/imu` | pub/sub `robots/<id>/imu` |
| PWM drive | `envelopes/pwm.json` | device → robot | pub/sub `robots/<id>/pwm` | pub/sub `robots/<id>/pwm` |
| set_led (req/resp) | `envelopes/rpc_set_led.json` | device ↔ robot | `robots/<id>/led` req, `robots/<id>/led/reply` resp (MQTT5 correlation-data) | queryable `robots/<id>/led` |

Language bindings (which mirror these envelopes): Rust in `pi/src/lib.rs`; the
ESP32 firmware hardcodes the same topics in C.

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
team/name/motor-pins post-join, `cmd/identify` blinks the board's LED (~6 s) so
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
