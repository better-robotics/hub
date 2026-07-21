//! hub — shared contract: typed envelopes + topic helpers, transport-agnostic.
//!
//! The transport is Zenoh — zenohd + the ws-adapter, separate from hubd; these
//! types are the shared contract every party mirrors (robot firmware on
//! zenoh-pico, sim clients, the Rust side). Envelopes mirror the monorepo
//! top-level contract (`../CONTRACT.md` + `../envelopes/`, canonical there).
//! Identity is the topic (`robots/<id>/<channel>`), never the body.

use serde::{Deserialize, Serialize};

/// Device-served Wi-Fi setup over NetworkManager (`nmcli`). hubd exposes it as
/// `/wifi/*` HTTP; a phone on the hub's own AP configures the uplink from the
/// dashboard. Replaced the Improv-over-BLE path (`provisiond`) on 2026-07-09 —
/// no hosted website, no Web Bluetooth, works on iOS.
pub mod wifi;

// ---- envelopes (mirror of the top-level envelopes/*.json) ----

/// IMU sample — robot → device. `synthetic` is set only by the demo robot
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
