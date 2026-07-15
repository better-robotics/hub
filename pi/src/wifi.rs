//! Wi-Fi provisioning over NetworkManager (`nmcli`) — the device-served
//! replacement for the old Improv-over-BLE path (`provisiond`, deleted
//! 2026-07-09). A phone joins the hub's own `hub-XXXX` AP, opens the
//! dashboard hubd already serves, and scans/joins a venue network from a
//! panel there. No hosted website, no Web Bluetooth, works on iOS.
//!
//! hubd calls these directly (it runs privileged now — see deploy/hubd.service,
//! DynamicUser dropped so it can drive NM). The functions shell out to `nmcli`,
//! so they compile anywhere but only do anything on a Linux host with
//! NetworkManager (the Pi); on a dev Mac they just return empty/err.

use serde::Serialize;

/// One scanned network, as the setup panel wants it.
#[derive(Serialize)]
pub struct Net {
    pub ssid: String,
    /// nmcli SIGNAL, 0..100 (not dBm — the panel shows bars, not physics).
    pub signal: i32,
    /// "WPA3" | "WPA2" | "WPA" | "WEP" | "NO" (open).
    pub security: String,
}

async fn nmcli_out(args: &[&str]) -> String {
    tokio::process::Command::new("nmcli")
        .args(args)
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Split an `nmcli -t` line on unescaped `:` and unescape `\:` / `\\`
/// (terse-mode escaping — an SSID can contain a literal colon).
fn split_nmcli(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ':' => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

/// Map nmcli's SECURITY field to a short token the panel can badge.
fn map_auth(sec: &str) -> String {
    let s = sec.to_uppercase();
    if s.contains("WPA3") {
        "WPA3"
    } else if s.contains("WPA2") {
        "WPA2"
    } else if s.contains("WPA") {
        "WPA"
    } else if s.contains("WEP") {
        "WEP"
    } else {
        "NO"
    }
    .to_string()
}

/// The classroom AP's own SSID (the `hub-ap` NM profile) — the dashboard's
/// identity chip names the network the user just joined, so the page and the
/// Wi-Fi picker agree on which hub this is (two hubs on the air is a normal
/// bench/classroom state, and their dashboards are otherwise pixel-identical).
pub async fn ap_ssid() -> String {
    nmcli_out(&["-g", "802-11-wireless.ssid", "con", "show", "hub-ap"]).await.trim().to_string()
}

/// Visible networks, strongest-labelled first duplicate wins, AP's own SSID
/// excluded is unnecessary (nmcli lists infrastructure APs, not our own AP).
pub async fn scan() -> Vec<Net> {
    let out = tokio::process::Command::new("nmcli")
        .args(["-t", "-f", "SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "yes"])
        .output()
        .await;
    let Ok(out) = out else {
        return vec![];
    };
    let mut nets = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let f = split_nmcli(line);
        if f.len() < 3 || f[0].is_empty() || !seen.insert(f[0].clone()) {
            continue;
        }
        nets.push(Net {
            ssid: f[0].clone(),
            signal: f[1].parse().unwrap_or(0),
            security: map_auth(&f[2]),
        });
    }
    nets
}

/// Is this NM connection profile an access point (`802-11-wireless.mode=ap`)?
async fn is_ap_profile(name: &str) -> bool {
    nmcli_out(&["-g", "802-11-wireless.mode", "connection", "show", name]).await.trim() == "ap"
}

/// A Wi-Fi device the AP does not own — the uplink join must be pinned to
/// one. Unpinned `nmcli device wifi connect` grabs whichever radio NM
/// fancies; on 2026-07-04 that was wlan0, and the classroom AP silently
/// became a venue-Wi-Fi client. Avoid the device of any active mode=ap
/// connection AND any mode=ap profile's pinned interface (covers the AP
/// being down at join time).
async fn uplink_device() -> Option<String> {
    let mut ap_devs: Vec<String> = Vec::new();
    let active = nmcli_out(&["-t", "-f", "NAME,TYPE,DEVICE", "connection", "show", "--active"]).await;
    for line in active.lines() {
        let f = split_nmcli(line);
        if f.len() < 3 || f[1] != "802-11-wireless" {
            continue;
        }
        if is_ap_profile(&f[0]).await {
            ap_devs.push(f[2].clone());
        }
    }
    for line in nmcli_out(&["-t", "-f", "NAME,TYPE", "connection", "show"]).await.lines() {
        let f = split_nmcli(line);
        if f.len() < 2 || f[1] != "802-11-wireless" {
            continue;
        }
        if !is_ap_profile(&f[0]).await {
            continue;
        }
        let dev = nmcli_out(&["-g", "connection.interface-name", "connection", "show", &f[0]]).await;
        let dev = dev.trim();
        if !dev.is_empty() {
            ap_devs.push(dev.to_string());
        }
    }
    for line in nmcli_out(&["-t", "-f", "DEVICE,TYPE", "device"]).await.lines() {
        let f = split_nmcli(line);
        if f.len() >= 2 && f[1] == "wifi" && !ap_devs.contains(&f[0]) {
            return Some(f[0].clone());
        }
    }
    None
}

/// `nmcli device wifi connect` parses its args in a keyword loop — a token is
/// only treated as the SSID *after* it fails to match `password`, `ifname`,
/// `bssid`, `hidden`, `name`, `private`, `wep-key-type`. So an SSID that IS one
/// of those words makes nmcli consume the *next* argv token (our `ifname`) as
/// that keyword's value — an argv misparse from an untrusted `/wifi/connect`
/// body. There's no shell here (`Command::args`, one token per value — no
/// word-splitting/`$()`/`;`), so this is the whole exposure: reject the
/// colliding words, an over-length or empty SSID (802.11 caps it at 32 octets),
/// and a leading `-` (belt-and-suspenders against flag smuggling). A real
/// network almost never uses these names; a clear error beats a silent misjoin.
const NMCLI_KEYWORDS: [&str; 7] =
    ["password", "ifname", "bssid", "hidden", "name", "private", "wep-key-type"];

fn check_ssid(ssid: &str) -> Result<(), String> {
    if ssid.is_empty() {
        return Err("pick a network".into());
    }
    if ssid.len() > 32 {
        return Err("network name too long (max 32 characters)".into());
    }
    if ssid.starts_with('-') || NMCLI_KEYWORDS.contains(&ssid) {
        return Err(format!("can't join a network named \"{ssid}\" from here — connect it manually"));
    }
    Ok(())
}

/// Join a venue network on the uplink radio, never the AP's. Returns Ok on a
/// successful `nmcli` join, Err with a human message otherwise (the panel
/// shows it verbatim).
pub async fn connect(ssid: &str, password: &str) -> Result<(), String> {
    check_ssid(ssid)?;
    let Some(dev) = uplink_device().await else {
        return Err("no spare Wi-Fi radio — the only one carries the classroom AP".into());
    };
    let mut cmd = tokio::process::Command::new("nmcli");
    cmd.args(["device", "wifi", "connect", ssid, "ifname", &dev]);
    if !password.is_empty() {
        cmd.args(["password", password]);
    }
    match cmd.output().await {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Erase the stored uplink credential(s) — the Pi's side of the same
/// "Forget this network" contract the ESP32 hub already answers
/// (rover_config_clear_wifi / POST /wifi/forget). Deletes every non-AP
/// Wi-Fi connection profile, active or not: realistically there's ever
/// exactly one (the venue/home network set from this panel), but
/// enumerating rather than assuming a fixed name reuses the same
/// discriminator `uplink_ssid`/`uplink_device` already rely on, not a new
/// piece of state to track. `nmcli connection delete` deactivates an
/// active profile as part of deleting it, so no separate disconnect step
/// is needed.
pub async fn forget() -> Result<(), String> {
    let mut deleted = 0;
    for line in nmcli_out(&["-t", "-f", "NAME,TYPE", "connection", "show"]).await.lines() {
        let f = split_nmcli(line);
        if f.len() < 2 || f[1] != "802-11-wireless" {
            continue;
        }
        if is_ap_profile(&f[0]).await {
            continue;
        }
        match tokio::process::Command::new("nmcli").args(["connection", "delete", &f[0]]).output().await {
            Ok(o) if o.status.success() => deleted += 1,
            Ok(o) => return Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => return Err(e.to_string()),
        }
    }
    if deleted == 0 {
        return Err("no stored uplink to forget".into());
    }
    Ok(())
}

/// The venue network the uplink leg is currently joined to (the active
/// non-AP wireless connection), or None if not joined. Lets the panel show
/// "currently on <ssid>" and confirm a join landed.
pub async fn uplink_ssid() -> Option<String> {
    let active = nmcli_out(&["-t", "-f", "NAME,TYPE", "connection", "show", "--active"]).await;
    for line in active.lines() {
        let f = split_nmcli(line);
        if f.len() < 2 || f[1] != "802-11-wireless" {
            continue;
        }
        if is_ap_profile(&f[0]).await {
            continue;
        }
        let ssid = nmcli_out(&["-g", "802-11-wireless.ssid", "connection", "show", &f[0]]).await;
        let ssid = ssid.trim();
        if !ssid.is_empty() {
            return Some(ssid.to_string());
        }
    }
    None
}
