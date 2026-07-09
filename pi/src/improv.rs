//! Improv Wi-Fi BLE protocol — the open standard for Wi-Fi provisioning over
//! BLE GATT (improv-wifi.com), as used by the ESPHome/ESP ecosystem. We
//! implement the *device* side (no off-the-shelf Linux server exists — it's all
//! microcontroller C++); the *client* is Improv's hosted Web-Bluetooth page, so
//! a phone provisions the hub with no app. This module is the pure wire layer:
//! exact byte values + framing + checksum, no BlueZ. `provisiond` drives it.
//!
//! Wire values are quoted from the spec — getting them from memory risks silent
//! interop failure with the standard client, so they are pinned here verbatim.

// ---- GATT UUIDs (service + 5 characteristics) ----
pub const SERVICE_UUID: &str = "00467768-6228-2272-4663-277478268000";
pub const CHAR_CURRENT_STATE: &str = "00467768-6228-2272-4663-277478268001";
pub const CHAR_ERROR_STATE: &str = "00467768-6228-2272-4663-277478268002";
pub const CHAR_RPC_COMMAND: &str = "00467768-6228-2272-4663-277478268003";
pub const CHAR_RPC_RESULT: &str = "00467768-6228-2272-4663-277478268004";
pub const CHAR_CAPABILITIES: &str = "00467768-6228-2272-4663-277478268005";

/// Provisioning state machine (Current State characteristic, notify).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Physical authorization required before credentials are accepted. We run
    /// open (no button), so we never emit this — start at Authorized.
    AuthorizationRequired = 0x01,
    Authorized = 0x02,
    Provisioning = 0x03,
    Provisioned = 0x04,
}

/// Error State characteristic (notify). Cleared to `None` on each new command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorState {
    None = 0x00,
    InvalidRpcPacket = 0x01,
    UnknownRpcCommand = 0x02,
    UnableToConnect = 0x03,
    NotAuthorized = 0x04,
    BadHostname = 0x05,
    Unknown = 0xFF,
}

/// Capabilities characteristic — LSB-first feature bits. We advertise Wi-Fi
/// scan only (bit 2); no identify/hostname/device-name on a headless router.
pub const CAP_IDENTIFY: u8 = 1 << 0;
pub const CAP_SCAN_WIFI: u8 = 1 << 2;

/// RPC command ids (first byte of an RPC Command packet).
pub mod cmd {
    pub const SEND_WIFI: u8 = 0x01;
    pub const IDENTIFY: u8 = 0x02;
    pub const DEVICE_INFO: u8 = 0x03;
    pub const SCAN_WIFI: u8 = 0x04;
}

/// A parsed, checksum-validated RPC command from the client.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Join this network. Empty password = open network.
    SendWifi { ssid: String, password: String },
    Scan,
    DeviceInfo,
    Identify,
}

/// Why a raw RPC packet was rejected — maps 1:1 to an `ErrorState` the device
/// writes back so the client can react per the standard.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Malformed framing: short packet, length mismatch, bad checksum, or a
    /// truncated SSID/password field. → ErrorState::InvalidRpcPacket.
    Invalid,
    /// Well-framed but an id we don't implement. → ErrorState::UnknownRpcCommand.
    UnknownCommand,
}

impl ParseError {
    pub fn error_state(&self) -> ErrorState {
        match self {
            ParseError::Invalid => ErrorState::InvalidRpcPacket,
            ParseError::UnknownCommand => ErrorState::UnknownRpcCommand,
        }
    }
}

/// Sum of all bytes, low byte only — the Improv checksum.
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

/// Parse one RPC Command packet: `[cmd][len][data..len][checksum]`. Validates
/// the declared length and the trailing checksum before trusting any field.
pub fn parse_command(packet: &[u8]) -> Result<Command, ParseError> {
    // Minimum: command + length + checksum.
    if packet.len() < 3 {
        return Err(ParseError::Invalid);
    }
    let command = packet[0];
    let data_len = packet[1] as usize;
    // Total must be cmd + len + data + checksum.
    if packet.len() != data_len + 3 {
        return Err(ParseError::Invalid);
    }
    let (body, &[given_sum]) = packet.split_at(packet.len() - 1) else {
        return Err(ParseError::Invalid);
    };
    if checksum(body) != given_sum {
        return Err(ParseError::Invalid);
    }
    let data = &packet[2..2 + data_len];

    match command {
        cmd::SEND_WIFI => parse_wifi(data).ok_or(ParseError::Invalid),
        cmd::SCAN_WIFI => Ok(Command::Scan),
        cmd::DEVICE_INFO => Ok(Command::DeviceInfo),
        cmd::IDENTIFY => Ok(Command::Identify),
        _ => Err(ParseError::UnknownCommand),
    }
}

/// Wi-Fi credentials data: `[ssid_len][ssid..][pass_len][pass..]`.
fn parse_wifi(data: &[u8]) -> Option<Command> {
    let ssid_len = *data.first()? as usize;
    let ssid_end = 1 + ssid_len;
    let ssid = data.get(1..ssid_end)?;
    let pass_len = *data.get(ssid_end)? as usize;
    let pass_start = ssid_end + 1;
    let password = data.get(pass_start..pass_start + pass_len)?;
    let ssid = String::from_utf8(ssid.to_vec()).ok()?;
    let password = String::from_utf8(password.to_vec()).ok()?;
    // Validate at the protocol boundary so a malformed value never reaches the
    // nmcli call (defense in depth — the args are argv-separated, so there's no
    // shell, but an SSID like "-x" could be mis-read as an nmcli flag, and
    // control chars are never valid). 802.11: SSID is 1..=32 bytes.
    if ssid.is_empty()
        || ssid.len() > 32
        || ssid.starts_with('-')
        || ssid.chars().any(|c| c.is_control())
    {
        return None;
    }
    // WPA PSK is at most 63 chars (64 hex); reject control chars either way.
    if password.len() > 64 || password.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(Command::SendWifi { ssid, password })
}

/// Encode an RPC Result: `[cmd][total_len][ (len, bytes).. ][checksum]`, where
/// `total_len` covers each string's length byte plus its bytes. The standard
/// client reads results as a list of length-prefixed strings.
pub fn encode_result(command: u8, strings: &[String]) -> Vec<u8> {
    let mut body = vec![command, 0];
    for s in strings {
        body.push(s.len() as u8);
        body.extend_from_slice(s.as_bytes());
    }
    body[1] = (body.len() - 2) as u8; // total data length
    let sum = checksum(&body);
    body.push(sum);
    body
}

/// A scan result is RPC-Result triplets `[ssid, rssi, auth]`. The client knows
/// the scan is done when it receives the empty result (`encode_result(SCAN, &[])`).
pub fn scan_triplet(ssid: &str, rssi_dbm: i32, auth: &str) -> [String; 3] {
    [ssid.to_string(), rssi_dbm.to_string(), auth.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_low_byte_sum() {
        assert_eq!(checksum(&[0x01, 0xFF]), 0x00); // wraps
        assert_eq!(checksum(&[0x04, 0x00]), 0x04);
    }

    #[test]
    fn parses_scan_command() {
        // 04 00 CS  → checksum = 0x04
        let pkt = [0x04, 0x00, 0x04];
        assert_eq!(parse_command(&pkt), Ok(Command::Scan));
    }

    #[test]
    fn parses_send_wifi() {
        // ssid="hi" (2), pass="pw" (2): data = [02 'h' 'i' 02 'p' 'w'] (6 bytes)
        let mut pkt = vec![cmd::SEND_WIFI, 6, 2, b'h', b'i', 2, b'p', b'w'];
        pkt.push(checksum(&pkt));
        assert_eq!(
            parse_command(&pkt),
            Ok(Command::SendWifi { ssid: "hi".into(), password: "pw".into() })
        );
    }

    #[test]
    fn open_network_empty_password() {
        let mut pkt = vec![cmd::SEND_WIFI, 4, 2, b'h', b'i', 0];
        pkt.push(checksum(&pkt));
        assert_eq!(
            parse_command(&pkt),
            Ok(Command::SendWifi { ssid: "hi".into(), password: String::new() })
        );
    }

    fn wifi_packet(ssid: &[u8], pass: &[u8]) -> Vec<u8> {
        let mut data = vec![ssid.len() as u8];
        data.extend_from_slice(ssid);
        data.push(pass.len() as u8);
        data.extend_from_slice(pass);
        let mut pkt = vec![cmd::SEND_WIFI, data.len() as u8];
        pkt.extend_from_slice(&data);
        pkt.push(checksum(&pkt));
        pkt
    }

    #[test]
    fn rejects_flag_smuggling_ssid() {
        // SSID "-x" could be read as an nmcli flag — never let it through.
        assert_eq!(parse_command(&wifi_packet(b"-x", b"")), Err(ParseError::Invalid));
    }

    #[test]
    fn rejects_control_chars_in_credentials() {
        assert_eq!(parse_command(&wifi_packet(b"net\nx", b"")), Err(ParseError::Invalid));
        assert_eq!(parse_command(&wifi_packet(b"net", b"pa\0ss")), Err(ParseError::Invalid));
    }

    #[test]
    fn rejects_oversized_ssid() {
        assert_eq!(parse_command(&wifi_packet(&[b'a'; 33], b"")), Err(ParseError::Invalid));
    }

    #[test]
    fn accepts_normal_credentials() {
        assert_eq!(
            parse_command(&wifi_packet(b"Classroom", b"s3cret-pw")),
            Ok(Command::SendWifi { ssid: "Classroom".into(), password: "s3cret-pw".into() })
        );
    }

    #[test]
    fn rejects_bad_checksum() {
        assert_eq!(parse_command(&[0x04, 0x00, 0xFF]), Err(ParseError::Invalid));
    }

    #[test]
    fn rejects_length_mismatch() {
        // declares 5 data bytes but only 1 present
        assert_eq!(parse_command(&[cmd::SEND_WIFI, 5, 0x00, 0x00]), Err(ParseError::Invalid));
    }

    #[test]
    fn rejects_truncated_ssid() {
        // ssid_len=9 but no bytes follow
        let mut pkt = vec![cmd::SEND_WIFI, 1, 9];
        pkt.push(checksum(&pkt));
        assert_eq!(parse_command(&pkt), Err(ParseError::Invalid));
    }

    #[test]
    fn unknown_command_distinguished() {
        let mut pkt = vec![0x06, 0x00];
        pkt.push(checksum(&pkt));
        assert_eq!(parse_command(&pkt), Err(ParseError::UnknownCommand));
    }

    #[test]
    fn result_roundtrip_framing() {
        // empty scan result: [04][00][checksum]
        assert_eq!(encode_result(cmd::SCAN_WIFI, &[]), vec![0x04, 0x00, 0x04]);
        // one string "ok": [03][03][02 'o' 'k'][cs]
        let r = encode_result(cmd::DEVICE_INFO, &["ok".to_string()]);
        assert_eq!(&r[..5], &[0x03, 0x03, 0x02, b'o', b'k']);
        assert_eq!(*r.last().unwrap(), checksum(&r[..r.len() - 1]));
    }
}
