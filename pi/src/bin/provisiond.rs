//! provisiond — headless Wi-Fi provisioning for the hub Pi over the Improv BLE
//! standard. When the Pi has no Wi-Fi, a phone opens Improv's hosted web client
//! (improv-wifi.com, Web Bluetooth — no app), scans, and sends credentials; we
//! join via NetworkManager. The wire protocol is `hub::improv` (unit-tested,
//! platform-independent); this binary is the BlueZ (`bluer`) + `nmcli` glue.
//!
//! It also serves the read-only hub-info characteristic (`hub::hubinfo`): the
//! courier read that lets the rover setup page fetch `{ssid, locator}` from
//! the hub instead of asking the student to type them (hub#3).
//!
//! Recovery/logs are a separate channel (USB-gadget ssh+serial) — Improv is
//! provisioning-only by design, so we don't bend it into a log transport.
//!
//! Linux-only: `bluer` needs the system bluetoothd. On other platforms this
//! compiles to a stub so `cargo build`/`cargo test` stay green on dev machines.

// Gated on linux-GNU (matches the bluer dependency in Cargo.toml): the musl
// hubd target is also linux but has no bluer, so it gets the stub.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn main() {
    eprintln!("provisiond requires Linux/BlueZ (bluer); it runs on the hub Pi.");
    std::process::exit(1);
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[tokio::main]
async fn main() {
    if let Err(e) = imp::run().await {
        eprintln!("provisiond: {e}");
        std::process::exit(1);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod imp {
    use std::sync::Arc;

    use bluer::gatt::local::{
        Application, Characteristic, CharacteristicNotify, CharacteristicNotifyMethod,
        CharacteristicNotifier, CharacteristicRead, CharacteristicWrite,
        CharacteristicWriteMethod, Service,
    };
    use futures::FutureExt;
    use tokio::sync::{broadcast, watch};
    use uuid::Uuid;

    use hub::hubinfo;
    use hub::improv::{
        cmd, encode_result, parse_command, scan_triplet, Command, ErrorState, State, CAP_SCAN_WIFI,
        CHAR_CAPABILITIES, CHAR_CURRENT_STATE, CHAR_ERROR_STATE, CHAR_RPC_COMMAND, CHAR_RPC_RESULT,
        SERVICE_UUID,
    };

    /// State shared between the GATT callbacks and the worker tasks. The two
    /// `watch` channels back the State/Error characteristics (latched current
    /// value + change notifications); `broadcast` fans RPC-Result frames out to
    /// the subscribed client.
    struct Shared {
        state: watch::Sender<u8>,
        error: watch::Sender<u8>,
        result: broadcast::Sender<Vec<u8>>,
    }

    pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;

        // No physical-auth button on the hub → start Authorized (the client may
        // send credentials immediately).
        let (state, _) = watch::channel(State::Authorized as u8);
        let (error, _) = watch::channel(ErrorState::None as u8);
        let (result, _) = broadcast::channel(32);
        let shared = Arc::new(Shared { state, error, result });

        let app = Application {
            services: vec![improv_service(shared.clone()), hubinfo_service()],
            ..Default::default()
        };
        let _app = adapter.serve_gatt_application(app).await?;

        let name = device_name().await;
        // Chrome's chooser shows the GAP name it cached at last connect (the
        // adapter alias, i.e. the hostname "hub") — not the scan-response name.
        // Alias must match the advertised name or the fix is invisible.
        let _ = adapter.set_alias(name.clone()).await;
        // Advertise via BlueZ's LEGACY mgmt path, NOT bluer's adapter.advertise().
        // The Pi 4's BCM4345C0 controller reports LE Extended Advertising support
        // it can't honor, so bluetoothd's ext-adv RegisterAdvertisement is rejected
        // (mgmt "Add Extended Advertising Data" → Invalid Parameters) and the daemon
        // crash-loops. The legacy "Add Advertising" mgmt op works; GATT still rides
        // bluetoothd, whose GATT path is fine — only this one operation bypasses it.
        // Ordered AFTER serve_gatt_application so bluetoothd has settled and won't
        // reconcile-clear our instance. (Root-caused + verified on hardware via btmon.)
        start_legacy_advert(&name)?;

        eprintln!("provisiond: advertising Improv service as {name} on {}", adapter.name());

        // No resurface loop: the mgmt advertisement survives connections
        // (measured 2026-07-04 — and no mgmt bit reports instance liveness,
        // so a poll would have nothing truthful to poll; see hub CLAUDE.md).
        // Hold the GATT handle for the process lifetime; systemd restarts us if we exit.
        std::future::pending::<()>().await;
        Ok(())
    }

    /// Register the Improv advertisement through BlueZ's legacy mgmt API
    /// (`btmgmt add-adv`) — see `run()` for why bluer's advertise() can't be used
    /// on this controller. Adv data carries the Improv service UUID (`-u`, what the
    /// web client filters on); the scan response (`-s`) carries the device name.
    /// `rm-adv` first so a leftover instance from a prior crash doesn't fail the add.
    fn start_legacy_advert(name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Scan-response AD structure: [len][0x09 = Complete Local Name][name bytes].
        let mut scan_rsp = vec![(name.len() + 1) as u8, 0x09];
        scan_rsp.extend_from_slice(name.as_bytes());
        let scan_hex: String = scan_rsp.iter().map(|b| format!("{b:02x}")).collect();

        let _ = std::process::Command::new("btmgmt").args(["rm-adv", "1"]).status();
        let ok = std::process::Command::new("btmgmt")
            .args(["add-adv", "-c", "-g", "-u", SERVICE_UUID, "-s", &scan_hex, "1"])
            .status()?
            .success();
        if !ok {
            return Err("btmgmt add-adv failed (is bluetoothd up and are we root?)".into());
        }
        Ok(())
    }

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).expect("static Improv UUID")
    }

    fn improv_service(shared: Arc<Shared>) -> Service {
        Service {
            uuid: uuid(SERVICE_UUID),
            primary: true,
            characteristics: vec![
                capabilities_char(),
                state_char(shared.clone()),
                error_char(shared.clone()),
                rpc_command_char(shared.clone()),
                rpc_result_char(shared),
            ],
            ..Default::default()
        }
    }

    /// Hub-info — the courier read (hub#3, see `hub::hubinfo`). Read-only;
    /// the value is composed live per read so it can't drift from the actual
    /// network state. Not in the advertisement — the setup page filters on
    /// the `hub-…` device name and lists this service as optional.
    fn hubinfo_service() -> Service {
        Service {
            uuid: uuid(hubinfo::SERVICE_UUID),
            primary: true,
            characteristics: vec![Characteristic {
                uuid: uuid(hubinfo::CHAR_INFO),
                read: Some(CharacteristicRead {
                    read: true,
                    fun: Box::new(|req| {
                        async move {
                            let v = hub_info_json().await;
                            // Serve offset reads (ATT Read Blob) for small MTUs.
                            Ok(v.get(req.offset as usize..).unwrap_or(&[]).to_vec())
                        }
                        .boxed()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// Capabilities — we support Wi-Fi scan only (no identify/hostname on a
    /// headless router).
    fn capabilities_char() -> Characteristic {
        Characteristic {
            uuid: uuid(CHAR_CAPABILITIES),
            read: Some(CharacteristicRead {
                read: true,
                fun: Box::new(|_req| async { Ok(vec![CAP_SCAN_WIFI]) }.boxed()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// A read+notify characteristic backed by a `watch<u8>` — current value on
    /// read, single-byte notification on change. Used for both State and Error.
    fn byte_state_char(char_uuid: &str, ch: &watch::Sender<u8>) -> Characteristic {
        let read_rx = ch.subscribe();
        let notify_rx = ch.subscribe();
        Characteristic {
            uuid: uuid(char_uuid),
            read: Some(CharacteristicRead {
                read: true,
                fun: Box::new(move |_req| {
                    let v = *read_rx.borrow();
                    async move { Ok(vec![v]) }.boxed()
                }),
                ..Default::default()
            }),
            notify: Some(CharacteristicNotify {
                notify: true,
                method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                    notify_byte(notifier, notify_rx.clone()).boxed()
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn state_char(shared: Arc<Shared>) -> Characteristic {
        byte_state_char(CHAR_CURRENT_STATE, &shared.state)
    }
    fn error_char(shared: Arc<Shared>) -> Characteristic {
        byte_state_char(CHAR_ERROR_STATE, &shared.error)
    }

    /// Push the current byte immediately, then on every change, until the client
    /// unsubscribes (notify returns Err) or the sender is dropped.
    async fn notify_byte(mut notifier: CharacteristicNotifier, mut rx: watch::Receiver<u8>) {
        loop {
            let v = *rx.borrow_and_update();
            if notifier.notify(vec![v]).await.is_err() {
                break;
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// RPC Result — notify-only delivery of length-prefixed result frames
    /// (scan triplets, command acks) produced by the worker tasks.
    fn rpc_result_char(shared: Arc<Shared>) -> Characteristic {
        Characteristic {
            uuid: uuid(CHAR_RPC_RESULT),
            notify: Some(CharacteristicNotify {
                notify: true,
                method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                    notify_results(notifier, shared.result.subscribe()).boxed()
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    async fn notify_results(mut notifier: CharacteristicNotifier, mut rx: broadcast::Receiver<Vec<u8>>) {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    if notifier.notify(frame).await.is_err() {
                        break;
                    }
                }
                // A slow client may lag the buffer; keep going rather than drop the link.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// RPC Command — the client writes framed commands here.
    fn rpc_command_char(shared: Arc<Shared>) -> Characteristic {
        Characteristic {
            uuid: uuid(CHAR_RPC_COMMAND),
            write: Some(CharacteristicWrite {
                write: true,
                write_without_response: true,
                method: CharacteristicWriteMethod::Fun(Box::new(move |value, _req| {
                    let shared = shared.clone();
                    async move {
                        handle_command(shared, value).await;
                        Ok(())
                    }
                    .boxed()
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Parse + dispatch one command. Long work (scan, join) is spawned so the
    /// BlueZ write callback returns promptly.
    async fn handle_command(shared: Arc<Shared>, value: Vec<u8>) {
        let _ = shared.error.send(ErrorState::None as u8);
        match parse_command(&value) {
            Ok(Command::Scan) => {
                tokio::spawn(do_scan(shared));
            }
            Ok(Command::SendWifi { ssid, password }) => {
                tokio::spawn(do_join(shared, ssid, password));
            }
            Ok(Command::DeviceInfo) => {
                let _ = shared.result.send(encode_result(cmd::DEVICE_INFO, &device_info().await));
            }
            // We advertise no identify capability, but ack silently if asked.
            Ok(Command::Identify) => {}
            Err(e) => {
                let _ = shared.error.send(e.error_state() as u8);
            }
        }
    }

    async fn do_scan(shared: Arc<Shared>) {
        for net in nmcli_scan().await {
            let [s, r, a] = scan_triplet(&net.ssid, net.rssi, &net.auth);
            let _ = shared.result.send(encode_result(cmd::SCAN_WIFI, &[s, r, a]));
        }
        // Empty result = scan complete (the standard's terminator).
        let _ = shared.result.send(encode_result(cmd::SCAN_WIFI, &[]));
    }

    async fn do_join(shared: Arc<Shared>, ssid: String, password: String) {
        let _ = shared.state.send(State::Provisioning as u8);
        if nmcli_connect(&ssid, &password).await {
            let _ = shared.state.send(State::Provisioned as u8);
            // No redirect URL (empty result) — the hub has no web UI to open.
            let _ = shared.result.send(encode_result(cmd::SEND_WIFI, &[]));
        } else {
            let _ = shared.error.send(ErrorState::UnableToConnect as u8);
            let _ = shared.state.send(State::Authorized as u8);
        }
    }

    // ---- NetworkManager glue (nmcli) ----

    /// The join info a rover (or student laptop) needs — plus hub status —
    /// read live. `ssid`/`locator` are what to join and dial: an active
    /// AP-mode connection wins (clients of our AP dial its gateway, the
    /// constant `10.42.0.1` in NM shared mode); otherwise the hub's own Wi-Fi
    /// + LAN address (hub and rover as siblings on someone else's network).
    /// `uplink` is the venue-facing STA leg with NM's connectivity verdict
    /// (`portal` is the one that matters: joined but a sign-in page blocks
    /// internet — needs a check URI configured, see image/stage-hub), and
    /// `hubd` is the router's systemd state. Absent facts are `null`; the
    /// setup page keeps its manual path for that case.
    async fn hub_info_json() -> Vec<u8> {
        let mut ap: Option<(String, String)> = None; // (profile, device)
        let mut sta: Option<(String, String)> = None;
        let active = nmcli_out(&["-t", "-f", "NAME,TYPE,DEVICE", "connection", "show", "--active"]).await;
        for line in active.lines() {
            let f = split_nmcli(line);
            if f.len() < 3 || f[1] != "802-11-wireless" {
                continue;
            }
            let mode = nmcli_out(&["-g", "802-11-wireless.mode", "connection", "show", &f[0]]).await;
            let slot = if mode.trim() == "ap" { &mut ap } else { &mut sta };
            slot.get_or_insert((f[0].clone(), f[2].clone()));
        }
        let uplink = match &sta {
            Some((profile, _)) => serde_json::json!({
                "ssid": profile_ssid(profile).await,
                // "general" reads NM's cached probe (stale up to its 300s interval,
                // so a race right after joining can show `portal` long after the
                // uplink is actually fine) — "check" forces a fresh probe instead.
                "connectivity": nmcli_out(&["networking", "connectivity", "check"]).await.trim(),
            }),
            None => serde_json::Value::Null,
        };
        let hubd = tokio::process::Command::new("systemctl")
            .args(["is-active", "hubd"])
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        // Which leg is being reported: "ap" is the healthy answer; "sta" means
        // the AP is down and ssid/locator below are the *uplink's* — join info
        // that would misprovision a rover (three took the venue Wi-Fi's values during
        // the 2026-07-04 outage). The page warns on anything but "ap".
        let mode = if ap.is_some() {
            "ap"
        } else if sta.is_some() {
            "sta"
        } else {
            "none"
        };
        let (ssid, locator) = match ap.or(sta) {
            Some((profile, dev)) => {
                let addr = nmcli_out(&["-g", "IP4.ADDRESS", "device", "show", &dev]).await;
                let ip = addr.lines().next().and_then(|a| a.split('/').next()).unwrap_or("");
                (
                    profile_ssid(&profile).await,
                    Some(format!("tcp/{ip}:7447")).filter(|_| !ip.is_empty()),
                )
            }
            None => (None, None),
        };
        serde_json::json!({ "mode": mode, "ssid": ssid, "locator": locator, "uplink": uplink, "hubd": hubd })
            .to_string()
            .into_bytes()
    }

    async fn profile_ssid(profile: &str) -> Option<String> {
        let s = nmcli_out(&["-g", "802-11-wireless.ssid", "connection", "show", profile]).await;
        Some(s.trim().to_string()).filter(|s| !s.is_empty())
    }

    async fn nmcli_out(args: &[&str]) -> String {
        tokio::process::Command::new("nmcli")
            .args(args)
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    struct Net {
        ssid: String,
        rssi: i32,
        auth: String,
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
            if f.len() < 3 || f[1] != "802-11-wireless" { continue; }
            let mode = nmcli_out(&["-g", "802-11-wireless.mode", "connection", "show", &f[0]]).await;
            if mode.trim() == "ap" { ap_devs.push(f[2].clone()); }
        }
        for line in nmcli_out(&["-t", "-f", "NAME,TYPE", "connection", "show"]).await.lines() {
            let f = split_nmcli(line);
            if f.len() < 2 || f[1] != "802-11-wireless" { continue; }
            let mode = nmcli_out(&["-g", "802-11-wireless.mode", "connection", "show", &f[0]]).await;
            if mode.trim() != "ap" { continue; }
            let dev = nmcli_out(&["-g", "connection.interface-name", "connection", "show", &f[0]]).await;
            let dev = dev.trim();
            if !dev.is_empty() { ap_devs.push(dev.to_string()); }
        }
        for line in nmcli_out(&["-t", "-f", "DEVICE,TYPE", "device"]).await.lines() {
            let f = split_nmcli(line);
            if f.len() >= 2 && f[1] == "wifi" && !ap_devs.contains(&f[0]) {
                return Some(f[0].clone());
            }
        }
        None
    }

    async fn nmcli_connect(ssid: &str, password: &str) -> bool {
        let Some(dev) = uplink_device().await else {
            eprintln!("provisiond: refusing to join '{ssid}' — the only Wi-Fi radio carries the AP");
            return false;
        };
        let mut cmd = tokio::process::Command::new("nmcli");
        cmd.args(["device", "wifi", "connect", ssid, "ifname", &dev]);
        if !password.is_empty() {
            cmd.args(["password", password]);
        }
        cmd.status().await.map(|s| s.success()).unwrap_or(false)
    }

    async fn nmcli_scan() -> Vec<Net> {
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
            // nmcli SIGNAL is 0..100; Improv wants RSSI dBm. Approximate.
            let signal: i32 = f[1].parse().unwrap_or(0);
            nets.push(Net {
                ssid: f[0].clone(),
                rssi: signal / 2 - 100,
                auth: map_auth(&f[2]),
            });
        }
        nets
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

    /// Map nmcli's SECURITY field to an Improv auth token.
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

    // ---- identity ----

    /// `hub-XXXX`, suffix = last 4 of the Pi chip serial, matching the USB-gadget
    /// product string so the same board is recognizable across channels.
    /// BLE name = the AP profile's SSID when one is configured (active or not),
    /// so both radios carry one identity token — the chooser entry names the
    /// network the rover will join. Fallback: hub-XXXX from the CPU serial.
    async fn device_name() -> String {
        for line in nmcli_out(&["-t", "-f", "NAME,TYPE", "connection", "show"]).await.lines() {
            let f = split_nmcli(line);
            if f.len() < 2 || f[1] != "802-11-wireless" { continue; }
            let mode = nmcli_out(&["-g", "802-11-wireless.mode", "connection", "show", &f[0]]).await;
            if mode.trim() != "ap" { continue; }
            if let Some(ssid) = profile_ssid(&f[0]).await {
                return ssid;
            }
        }
        let serial = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|c| {
                c.lines()
                    .find(|l| l.starts_with("Serial"))
                    .and_then(|l| l.split_whitespace().last().map(str::to_string))
            })
            .unwrap_or_default();
        let suffix: String = serial.chars().rev().take(4).collect::<String>().chars().rev().collect();
        let suffix = if suffix.is_empty() { "0000".into() } else { suffix.to_uppercase() };
        format!("hub-{suffix}")
    }

    /// Improv device-info result strings: firmware name, version, hardware, name.
    async fn device_info() -> Vec<String> {
        vec![
            "hub".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            "Raspberry Pi".to_string(),
            device_name().await,
        ]
    }
}
