# CONTRACT — the hub wire contract

The transport-agnostic message contract every implementation in this repo
(`pi/`, `esp32/`) and client (`dashboard.html`, `mcp-bridge/`) agrees on:
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

| Message | File | Direction | MQTT (`pi/` + `esp32/`) | Zenoh (baseline) |
|---------|------|-----------|--------------------------|------------------|
| IMU sample | `envelopes/imu.json` | robot → device | pub/sub `robots/<id>/imu` | pub/sub `robots/<id>/imu` |
| PWM drive | `envelopes/pwm.json` | device → robot | pub/sub `robots/<id>/pwm` | pub/sub `robots/<id>/pwm` |
| set_led (req/resp) | `envelopes/rpc_set_led.json` | device ↔ robot | `robots/<id>/led` req, `robots/<id>/led/reply` resp (MQTT5 correlation-data) | queryable `robots/<id>/led` |

Language bindings (which mirror these envelopes): Rust in `pi/src/lib.rs`; the
ESP32 firmware hardcodes the same topics in C.
