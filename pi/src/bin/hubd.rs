//! hubd — the hub's HTTP side, MQTT-transport variant. **hubd is not an MQTT
//! client at all** (flipped 2026-07-08, see CLAUDE.md § Architecture):
//! Mosquitto is the broker, and every MQTT party — rover firmware, the
//! browser dashboard's `mqtt.js`, sim clients — talks to it directly, scoped
//! by Mosquitto's own ACL (mosquitto-acl.example.conf). hubd only serves
//! plain HTTP: the dashboard page (which then makes its own MQTT-over-WS
//! connection), `/fleet` (uplink verdict + broker locator, for the parts a
//! browser can't get over MQTT), and device-served Wi-Fi setup at `/wifi/*`
//! (nmcli, `src/wifi.rs`) — the day-zero provisioning that used to be a
//! separate BLE `provisiond` binary (deleted 2026-07-09).
//!
//! `GET /` is the embedded dashboard; `GET /fleet` is `{uplink, locator}`.
//! The dashboard has `mqtt.js` inlined directly (2026-07-08) rather than
//! served as a separate file — that's also what makes it a genuine
//! standalone artifact: download the top-level `dashboard.html` on its own, open it
//! as `file://`, type in a hub address, and it works with no hubd behind it
//! at all (verified: a `file://` origin can open a plain `ws://` connection
//! with no mixed-content block, unlike an `https:`-hosted copy would need).
//! Inlining also means it works with no internet uplink when hubd *is*
//! serving it — the classroom Pi may have none, which is exactly what the
//! uplink probe below exists to detect.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const DASHBOARD_HTML: &str = include_str!("../../../dashboard.html");
const ICON_SVG: &str = include_str!("../../public/icon.svg");

/// Uplink verdict: "full" | "portal" | "none" | "unknown" (pre-first-probe
/// only — the dashboard hides the pill for it).
type Uplink = Arc<Mutex<String>>;

/// Probe the internet uplink the way phones do: fetch a known 204 endpoint
/// over plain HTTP. 204 → clear internet; any other HTTP answer → something
/// answered in the endpoint's place, i.e. a captive portal; no answer → no
/// uplink. Self-probing rather than asking nmcli (hubd runs as root now and
/// could — see deploy/hubd.service) because the probe tests the path packets
/// actually take, not NM's opinion of it (NM's cached verdict lags its probe
/// interval and can disagree with reality right after a join).
async fn probe_uplink() -> &'static str {
    const HOST: &str = "connectivitycheck.gstatic.com";
    let probe = async {
        // IPv4 only: a venue advertising broken IPv6 would otherwise eat the
        // whole timeout before the reachable A record is tried.
        let addr = tokio::net::lookup_host((HOST, 80)).await.ok()?.find(|a| a.is_ipv4())?;
        let mut sock = tokio::net::TcpStream::connect(addr).await.ok()?;
        let req =
            format!("GET /generate_204 HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n");
        sock.write_all(req.as_bytes()).await.ok()?;
        let mut buf = [0u8; 64];
        let n = sock.read(&mut buf).await.ok()?;
        String::from_utf8_lossy(&buf[..n]).split_whitespace().nth(1).map(|s| s == "204")
    };
    match tokio::time::timeout(Duration::from_secs(8), probe).await {
        Ok(Some(true)) => "full",
        Ok(Some(false)) => "portal",
        _ => "none",
    }
}

/// A captive portal blocks only the NAT'd internet uplink — the classroom
/// (AP, fabric, this dashboard) is unaffected. The dashboard's job is to say
/// so, and how to clear it: any client behind the NAT shares the hub's
/// venue-side MAC, so one sign-in from any phone unlocks everyone.
async fn poll_uplink(uplink: Uplink) {
    // Downgrades are debounced: one failed probe (busy uplink dongle, slow
    // venue DNS) must not flash "no internet" at a classroom that has it.
    // Recovery to "full" is instant; none/portal needs 3 agreeing probes.
    let mut streak: (&str, u32) = ("unknown", 0);
    loop {
        let verdict = probe_uplink().await;
        streak = if verdict == streak.0 { (verdict, streak.1 + 1) } else { (verdict, 1) };
        if verdict == "full" || streak.1 >= 3 {
            *uplink.lock().unwrap() = verdict.into();
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

fn fleet_json(uplink: &Uplink, locator: &str, ssid: &str) -> String {
    // `ssid`/`host` feed the dashboard's identity chip: which hub serves this
    // page (two hubs on the air render otherwise-identical dashboards).
    serde_json::json!({
        "uplink": *uplink.lock().unwrap(), "locator": locator,
        "ssid": ssid, "host": "pi",
    })
    .to_string()
}

/// `GET /wifi/status` — the venue network the uplink is on (or null) plus the
/// live internet verdict, so the setup panel can show "currently on X · online"
/// and confirm a join landed.
async fn wifi_status_json(uplink: &Uplink) -> String {
    let ssid = hub::wifi::uplink_ssid().await;
    serde_json::json!({ "ssid": ssid, "uplink": *uplink.lock().unwrap() }).to_string()
}

/// `POST /wifi/connect` — body `{ssid, password}`. Joins on the uplink radio
/// (never the classroom AP's) and reports the outcome; the panel shows `error`
/// verbatim on failure.
async fn wifi_connect_json(body: &str) -> (&'static str, &'static str, String) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let ssid = v.get("ssid").and_then(|s| s.as_str()).unwrap_or("");
    let password = v.get("password").and_then(|s| s.as_str()).unwrap_or("");
    if ssid.is_empty() {
        return ("400 Bad Request", "application/json", r#"{"ok":false,"error":"missing ssid"}"#.into());
    }
    match hub::wifi::connect(ssid, password).await {
        Ok(()) => ("200 OK", "application/json", r#"{"ok":true}"#.into()),
        Err(e) => (
            "200 OK",
            "application/json",
            serde_json::json!({ "ok": false, "error": e }).to_string(),
        ),
    }
}

async fn accept_forever(listener: TcpListener, uplink: Uplink, locator: String, ssid: String) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let uplink = uplink.clone();
        let locator = locator.clone();
        let ssid = ssid.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let mut words = req.split_whitespace();
            let method = words.next().unwrap_or("GET");
            let path = words.next().unwrap_or("/");
            // CORS/PNA preflight: a PUBLIC https page (the setup wizard on
            // github.io) fetching this LOCAL server triggers Chrome's Private
            // Network Access check — an OPTIONS preflight that must be
            // answered with Allow-Private-Network, or the fetch is blocked.
            if method == "OPTIONS" {
                let resp = "HTTP/1.1 204 No Content\r\n\
                            Access-Control-Allow-Origin: *\r\n\
                            Access-Control-Allow-Private-Network: true\r\n\
                            Access-Control-Allow-Methods: GET, POST\r\n\
                            Access-Control-Allow-Headers: Content-Type\r\n\
                            Connection: close\r\n\r\n";
                let _ = sock.write_all(resp.as_bytes()).await;
                return;
            }
            // The Wi-Fi setup panel POSTs `{ssid, password}` here; the body is
            // whatever follows the header terminator (creds are tiny — one read
            // of `buf` holds them).
            let post_body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            let (status, ctype, body) = match (method, path) {
                ("GET", "/fleet") => ("200 OK", "application/json", fleet_json(&uplink, &locator, &ssid)),
                ("GET", "/") | ("GET", "/index.html") => {
                    ("200 OK", "text/html; charset=utf-8", DASHBOARD_HTML.into())
                }
                ("GET", "/icon.svg") => ("200 OK", "image/svg+xml", ICON_SVG.into()),
                // Device-served Wi-Fi setup (replaces the old BLE/website flow).
                ("GET", "/wifi/scan") => (
                    "200 OK",
                    "application/json",
                    serde_json::to_string(&hub::wifi::scan().await).unwrap_or_else(|_| "[]".into()),
                ),
                ("GET", "/wifi/status") => ("200 OK", "application/json", wifi_status_json(&uplink).await),
                ("POST", "/wifi/connect") => wifi_connect_json(post_body).await,
                // Team-code management (dashboard "Team codes" panel). Reads are
                // public (usernames = topic ids the anonymous fleet view already
                // shows); writes re-verify the professor's code per request.
                ("GET", "/codes/list") => ("200 OK", "application/json", hub::codes::list_json()),
                ("POST", "/codes/set") => hub::codes::set_json(post_body).await,
                ("POST", "/codes/del") => hub::codes::del_json(post_body).await,
                // Captive Portal API (RFC 8908), pointed at by DHCP option 114
                // (RFC 8910, dnsmasq drop-in in the image). `captive:false` is
                // the whole point: nothing is blocked, we're only advertising
                // the dashboard as the venue page so joining devices surface it
                // unprompted. 10.42.0.1 = NM-shared's AP address, the same
                // always-works fallback the docs teach. Caveat, recorded
                // honestly: RFC 8908 wants this endpoint over TLS, which an
                // offline LAN appliance can't validly present — some clients
                // may ignore the plain-HTTP form. Zero blast radius either
                // way; verify on a real phone, keep if it helps, shrug if not.
                ("GET", "/captive") => (
                    "200 OK",
                    "application/captive+json",
                    r#"{"captive":false,"venue-info-url":"http://10.42.0.1/"}"#.into(),
                ),
                _ => ("404 Not Found", "text/plain", "not found".into()),
            };
            // ACAO *: /fleet is public-read JSON, and the rover setup page
            // (better-robotics.github.io) prefills the hub address from it.
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    }
}

async fn serve_http(uplink: Uplink, addr: String, locator: String, ssid: String) {
    let listener = TcpListener::bind(&addr).await.expect("bind HUB_HTTP");
    // `localhost` resolves to both ::1 and 127.0.0.1; Chrome tries ::1 first
    // and (confirmed live, 07-07) does not fall back to the working IPv4
    // address when that attempt is refused — the landing page's local-hub
    // probe (`fetch('http://localhost:PORT/fleet')`) failed outright against
    // this IPv4-only listener even though curl on the same host succeeded.
    // Best-effort second bind on the IPv6 loopback; the classroom deployment
    // (a Pi's own AP, IPv4-only LAN clients) doesn't need it and is
    // unaffected if it's unavailable.
    if let Some(port) = addr.rsplit(':').next() {
        if let Ok(v6) = TcpListener::bind(format!("[::1]:{port}")).await {
            tokio::spawn(accept_forever(v6, uplink.clone(), locator.clone(), ssid.clone()));
        }
    }
    accept_forever(listener, uplink, locator, ssid).await;
}

#[tokio::main]
async fn main() {
    let http = std::env::var("HUB_HTTP").unwrap_or_else(|_| "0.0.0.0:8000".to_string());
    // Not bound by hubd — this is Mosquitto's own listener address (see
    // mosquitto.example.conf), reported here purely so the dashboard and the
    // rover setup page have something to prefill. Default matches Mosquitto's
    // conventional raw-MQTT port.
    let mqtt_addr = std::env::var("HUB_MQTT_ADDR").unwrap_or_else(|_| "0.0.0.0:1883".to_string());

    // The address students need twice (dashboard URL, rover locator) — print
    // it, and serve it in /fleet so the dashboard can show what to paste
    // into the rover setup page. Filter by interface
    // KIND, not address range: real Wi-Fi can hand out CGNAT space (measured:
    // en0 at 100.110.x.x), so an RFC1918 filter rejects real LANs, while the
    // routing-table shortcut (UDP connect + local_addr) reports the tunnel
    // address on VPN machines. NIC names (en*/eth*/wl*) first, then anything
    // that isn't a known tunnel/bridge.
    let usable = |ip: &std::net::IpAddr| match ip {
        std::net::IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local(),
        _ => false,
    };
    const TUNNELS: [&str; 6] = ["utun", "tun", "tailscale", "docker", "veth", "br-"];
    let host = local_ip_address::list_afinet_netifas()
        .map(|ifs| {
            let good: Vec<_> = ifs.into_iter().filter(|(_, ip)| usable(ip)).collect();
            good.iter()
                .find(|(n, _)| ["en", "eth", "wl"].iter().any(|p| n.starts_with(p)))
                .or_else(|| good.iter().find(|(n, _)| !TUNNELS.iter().any(|p| n.starts_with(p))))
                .map(|(_, ip)| ip.to_string())
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| "<this-machine>".into());
    let mqtt_port = mqtt_addr.rsplit(':').next().unwrap_or("1883");
    let locator = format!("mqtt://{host}:{mqtt_port}");

    let uplink: Uplink = Arc::new(Mutex::new("unknown".into()));
    tokio::spawn(poll_uplink(uplink.clone()));
    let ap_ssid = hub::wifi::ap_ssid().await; // stable while running (MAC-derived profile)
    tokio::spawn(serve_http(uplink, http.clone(), locator.clone(), ap_ssid));

    let port = http.rsplit(':').next().unwrap_or("8000");
    println!("[hubd] dashboard: http://{host}:{port} (fleet JSON at /fleet)");
    println!("[hubd] rovers/sim clients: point at the broker, {locator} (see mosquitto.example.conf)");

    // hubd holds no transport session — Mosquitto is a separate process.
    // Park so the HTTP chassis (dashboard, /fleet) stays up.
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
