//! hub — shared contract: typed envelopes + topic helpers, transport-agnostic.
//!
//! The broker is Mosquitto (a separate process, not hubd); these types are for
//! MQTT clients — rover firmware, sim clients. Envelopes mirror `protocol/`
//! (canonical here — see `protocol/README.md`). Identity is the topic
//! (`robots/<id>/<channel>`), never the body.

use serde::{Deserialize, Serialize};

/// Improv Wi-Fi BLE provisioning protocol (the `provisiond` wire layer).
pub mod improv;

/// Hub-info BLE service — the courier read (hub#3). A phone/laptop connects,
/// reads one JSON blob `{ssid, locator}`, disconnects (~2 s hub slot); the
/// values then ride that browser into the rover's provisioning session, so no
/// student ever types the hub's address. provisiond composes the JSON live at
/// read time — the characteristic can't drift from actual network state.
/// The setup page (better-robotics.github.io/provision) hard-codes these UUIDs.
pub mod hubinfo {
    pub const SERVICE_UUID: &str = "5ecf89a4-9f99-45dc-b3e6-998933c06dd8";
    pub const CHAR_INFO: &str = "69e7f1d7-a25e-4f0a-ba18-caabeaaa72e1";
}

/// Default robot id for the demos. Override with `ROBOT_ID`.
pub const ROBOT_ID: &str = "rover_01";

// ---- key expressions (identity lives here, not in the body) ----
pub fn imu_key(id: &str) -> String {
    format!("robots/{id}/imu")
}
pub fn pwm_key(id: &str) -> String {
    format!("robots/{id}/pwm")
}
pub fn led_key(id: &str) -> String {
    format!("robots/{id}/led")
}

// ---- envelopes (mirror of protocol/envelopes/*.json) ----

/// IMU sample — robot → device. `synthetic` is set only by the demo rover
/// (no hardware); a real board omits it.
#[derive(Serialize, Deserialize, Debug)]
pub struct Imu {
    pub timestamp: f64,
    pub accel_x: f64,
    pub accel_y: f64,
    pub accel_z: f64,
    pub gyro_x: f64,
    pub gyro_y: f64,
    pub gyro_z: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
}

/// PWM drive command — device → robot (pub/sub).
#[derive(Serialize, Deserialize, Debug)]
pub struct Pwm {
    pub timestamp: f64,
    pub left_motor: i32,
    pub right_motor: i32,
    pub duration_ms: u32,
}

/// set_led request — device → robot (RPC via queryable `get`).
#[derive(Serialize, Deserialize, Debug)]
pub struct SetLedRequest {
    pub method: String, // "set_led"
    pub on: bool,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

/// set_led response — robot → device. No `id`/`topic` field: hub-zenoh's
/// Zenoh queryable correlates the reply to its query by construction; the
/// MQTT equivalent (response-topic + correlation-data, MQTT5) hasn't landed
/// — see hub#1.
#[derive(Serialize, Deserialize, Debug)]
pub struct SetLedResponse {
    pub status: String, // "ok" | "error"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ---- config helpers ----
// The MQTT session/broker config (connect endpoint, auth, ACL) goes here
// once the transport is chosen — see hub#1 (fka better-robotics/hub#5).

/// Epoch seconds (float) — matches the envelope `timestamp` contract.
pub fn now_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs_f64()
}
