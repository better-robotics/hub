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
//! `GET /` is the embedded dashboard; `GET /fleet` is `{uplink, locator}`;
//! `GET /ide/` serves the better-robotics/ide bundle from disk when installed.
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
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const DASHBOARD_HTML: &str = include_str!("../../../dashboard.html");
const ICON_SVG: &str = include_str!("../../public/icon.svg");

/// The ide bundle (better-robotics/ide's built dist — source + vendored
/// Monaco/mqtt.js), served at `/ide/`. On-disk rather than embedded: it's a
/// large tree with binary/vendored assets, ships on its own release cadence,
/// and the image/installer drop it in place — hubd needs no rebuild when the
/// IDE updates. Serving it from the hub is what makes the IDE reachable over
/// plain http on the classroom LAN: same protocol as the broker (ws://) and
/// the rovers' camera endpoints, so no mixed-content wall — the only shape
/// that works on iOS phones (no insecure-content override exists there).
fn ide_dir() -> std::path::PathBuf {
    std::env::var("HUB_IDE_DIR").unwrap_or_else(|_| "/usr/share/hub/ide".into()).into()
}

fn ide_mime(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        // .mjs must be JS or the browser refuses the ES-module import —
        // the IDE's MicroPython runtime (vendor/micropython/micropython.mjs)
        // loads that way since ide-v6.
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "webmanifest" => "application/manifest+json",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        "md" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Build the full HTTP response bytes for a `/ide` request. Bytes, not
/// String — the bundle has binary assets (png/ico/woff2) that the string
/// response path used by the API routes would corrupt.
async fn ide_serve(raw_path: &str) -> Vec<u8> {
    let path = raw_path.split('?').next().unwrap_or(raw_path);
    if path == "/ide" {
        // Trailing slash matters: the page's relative module imports resolve
        // against the directory, not the bare segment.
        return b"HTTP/1.1 301 Moved Permanently\r\nLocation: /ide/\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec();
    }
    let rel = path.trim_start_matches("/ide/");
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let dir = ide_dir();
    // Bundle filenames are plain ASCII; rejecting dot-dot segments is the
    // whole traversal surface (no percent-decoding happens above).
    if rel.split('/').any(|seg| seg == "..") || rel.starts_with('/') {
        return b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    }
    match tokio::fs::read(dir.join(rel)).await {
        Ok(bytes) => {
            let mut resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
                 Cache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
                ide_mime(std::path::Path::new(rel)),
                bytes.len()
            )
            .into_bytes();
            resp.extend_from_slice(&bytes);
            resp
        }
        Err(_) => {
            let body: &[u8] = if dir.exists() {
                b"not found"
            } else {
                b"IDE bundle not installed \xe2\x80\x94 run deploy/install.sh (needs internet), \
                  or place better-robotics/ide's built dist at /usr/share/hub/ide"
            };
            let mut resp = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            resp.extend_from_slice(body);
            resp
        }
    }
}

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
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "hub".into());
    serde_json::json!({
        "uplink": *uplink.lock().unwrap(), "locator": locator,
        "ssid": ssid, "host": host,
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

/// Devices that tapped "Accept" on /welcome, with when they were last seen
/// on the network. From then on their OS probes get the genuine success
/// answers, so the captive sheet's button turns into "Done" and the user can
/// finish in a real browser — the public-Wi-Fi splash-then-release flow.
/// Per-device opt-in on purpose: a device that never accepts keeps honest
/// "captive" answers, so an offline hub never tricks it into believing the
/// uplink works (that lie is what breaks phones' cellular fallback — the
/// reason blanket fake-success stayed chosen-against). An Accept means "this
/// visit", not "this device forever" — `reap_acks` forgets devices that
/// leave, so the popup greets every fresh join and a DHCP-reused address
/// can't inherit a stranger's Accept. Classroom-day scale.
static ACKED: Mutex<Vec<(std::net::IpAddr, Instant)>> = Mutex::new(Vec::new());

/// Best-effort packet-layer release/recapture of one device in the AP's
/// captive NAT — the `acked` set in hub-ap-setup.sh's hub-captive table
/// (`op` is nft's "add" / "delete"). An acked device's DNS and HTTP then go
/// real instead of being steered to the hub: once Accept has been tapped
/// there is nothing left to hold its packets for (the ESP portal's
/// stop-lying-once-real lesson, robot@f313b57). Dev boxes without the table
/// (or an IPv6 peer, which the set can't hold) just no-op — the in-memory
/// ACKED check still answers hubd-level probes correctly either way.
async fn nft_acked(op: &'static str, ip: std::net::IpAddr) {
    if !ip.is_ipv4() {
        return;
    }
    let _ = tokio::process::Command::new("nft")
        .args([op, "element", "ip", "hub-captive", "acked", &format!("{{ {ip} }}")])
        .output()
        .await;
}

/// Presence reaper for ACKED. Every minute, poke each acked address (`ping`
/// is only the ARP trigger — the kernel resolves the neighbor before ICMP,
/// and ARP is the one layer nothing opts out of: sleeping iPhones keep ARP
/// offload, Windows firewalls drop ICMP but must still answer ARP) and read
/// the kernel neighbor table's verdict; a device unreachable for the whole
/// grace window is gone and forgets its ack. The grace is deliberately
/// generous: iPhones drop off the AP while asleep in a pocket, and an ack
/// expiring under a still-present device would flip its probes back to
/// "captive" and re-summon the sheet mid-class. Tool failure never expires
/// anyone. (`iw station dump` would be the authoritative association check,
/// but the appliance image doesn't carry `iw` — neighbor reachability is
/// the same answer one layer up.)
async fn reap_acks() {
    const POLL: Duration = Duration::from_secs(60);
    const GRACE: Duration = Duration::from_secs(15 * 60);
    // A fresh hubd means a fresh ack list — clear any packet-layer releases a
    // previous run left in the nft set, so the two layers can't disagree.
    let _ = tokio::process::Command::new("nft")
        .args(["flush", "set", "ip", "hub-captive", "acked"])
        .output()
        .await;
    loop {
        tokio::time::sleep(POLL).await;
        let ips: Vec<std::net::IpAddr> =
            ACKED.lock().unwrap().iter().map(|(ip, _)| *ip).collect();
        if ips.is_empty() {
            continue;
        }
        let pokes: Vec<_> = ips
            .iter()
            .map(|ip| {
                let ip = ip.to_string();
                tokio::spawn(async move {
                    let _ = tokio::process::Command::new("ping")
                        .args(["-c", "1", "-W", "1", &ip])
                        .output()
                        .await;
                })
            })
            .collect();
        for p in pokes {
            let _ = p.await;
        }
        let Ok(out) =
            tokio::process::Command::new("ip").args(["neigh", "show"]).output().await
        else {
            continue;
        };
        let neigh = String::from_utf8_lossy(&out.stdout);
        let now = Instant::now();
        // Scoped so the MutexGuard (not Send) is dropped before the awaits
        // below — required for reap_acks' future to stay spawnable.
        let expired: Vec<std::net::IpAddr> = {
            let mut acks = ACKED.lock().unwrap();
            for (ip, seen) in acks.iter_mut() {
                let ip_s = ip.to_string();
                let reachable = neigh.lines().any(|l| {
                    l.starts_with(&ip_s)
                        && l.as_bytes().get(ip_s.len()) == Some(&b' ')
                        && l.trim_end().ends_with("REACHABLE")
                });
                if reachable {
                    *seen = now;
                }
            }
            let mut expired = Vec::new();
            acks.retain(|(ip, seen)| {
                let keep = now.duration_since(*seen) < GRACE;
                if !keep {
                    expired.push(*ip);
                }
                keep
            });
            expired
        };
        for ip in expired {
            nft_acked("delete", ip).await; // back under capture on next join
        }
    }
}

/// The captive sheet's landing page (probe 302s point here, NOT at the
/// dashboard): Apple's CNA sandboxes localStorage away from Safari, so a
/// sign-in done inside the sheet would silently vanish — this page's whole
/// job is to release the sheet and hand the user to a real browser.
const WELCOME_HTML: &str = include_str!("../welcome.html");

/// Is this request addressed to some other server than us? True means the
/// captive-capture NAT dropped it in our lap (see hub-ap-setup.sh). Every
/// name the hub legitimately answers to — the AP address, mDNS, loopback dev
/// binds — counts as ours; an absent Host header does too (probe clients are
/// not all HTTP/1.1-polite, and a false "foreign" would turn honest 404s
/// into redirects).
fn foreign_host(req: &str) -> bool {
    let Some(host) = req.lines().find_map(|l| {
        l.split_once(':').and_then(|(name, v)| {
            name.eq_ignore_ascii_case("host").then(|| v.trim())
        })
    }) else {
        return false;
    };
    if host.starts_with('[') {
        return false; // bracketed IPv6 — only the ::1 dev bind ever sees it
    }
    let host = host.split(':').next().unwrap_or(host); // strip :port
    !(host == "10.42.0.1"
        || host == "hub.local"
        || host == "hub"
        || host == "localhost"
        || host.starts_with("127.")
        || host == "10.55.0.1")
}

async fn accept_forever(listener: TcpListener, uplink: Uplink, locator: String, ssid: String) {
    loop {
        let Ok((mut sock, peer)) = listener.accept().await else { continue };
        let uplink = uplink.clone();
        let locator = locator.clone();
        let ssid = ssid.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let mut words = req.split_whitespace();
            let method = words.next().unwrap_or("GET");
            let raw_path = words.next().unwrap_or("/");
            // Strip the query string before any route match below — every
            // arm compares against the bare path (e.g. "/welcome"), and
            // welcome.html's Accept button navigates to "/welcome?done=1"
            // (the ESP portal's fix, ported here: a captive sheet only
            // re-checks captivity on a full-page load, not a DOM swap) —
            // without this the query string fell through every arm to the
            // 404 handler, live-observed on macOS's CNA sheet.
            let path = raw_path.split('?').next().unwrap_or(raw_path);
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
            // Workbench IDE bundle — binary-safe path, bypasses the string
            // response builder below.
            if method == "GET" && (path == "/ide" || path.starts_with("/ide/")) {
                let _ = sock.write_all(&ide_serve(path).await).await;
                return;
            }
            // The Wi-Fi setup panel POSTs `{ssid, password}` here; the body is
            // whatever follows the header terminator (creds are tiny — one read
            // of `buf` holds them).
            let post_body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            // Did this device already tap Accept on /welcome? Decides whether
            // its OS probes get "captive" (302) or genuine success answers.
            let acked = ACKED.lock().unwrap().iter().any(|(ip, _)| *ip == peer.ip());
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
                // "Forget this network" (dashboard.html's Set-up-Wi-Fi panel)
                // — the Pi side of the same contract the ESP32 hub already
                // answers. The panel only checks res.ok, so the JSON body
                // shape matches this file's other endpoints ({"ok":...}),
                // not the ESP32's plain-text "forgotten".
                ("POST", "/wifi/forget") => match hub::wifi::forget().await {
                    Ok(()) => ("200 OK", "application/json", r#"{"ok":true}"#.into()),
                    Err(e) => (
                        "200 OK",
                        "application/json",
                        serde_json::json!({ "ok": false, "error": e }).to_string(),
                    ),
                },
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
                // This is the MDM/classroom-safe path (unmanaged AND managed
                // devices); it stays exactly as it was — untouched by the
                // probe-intercept handlers below.
                ("GET", "/captive") => (
                    "200 OK",
                    "application/captive+json",
                    r#"{"captive":false,"venue-info-url":"http://10.42.0.1/"}"#.into(),
                ),
                // OS captive-portal auto-popup, personal/unmanaged devices only —
                // see 00-run.sh's `30-ap-captive-probes.conf` heredoc for the
                // audience split. The dnsmasq drop-in there resolves each OS's
                // connectivity-check
                // hostname to this hub's AP address, so these specific paths
                // are the only requests that can ever land here for them.
                // Each OS's checker expects an exact "network is clean"
                // answer; deliberately failing that expectation is what makes
                // the OS treat the network as captive and auto-launch its own
                // mini-browser, which the Location header then points at the
                // dashboard instead of leaving it blank.
                //   Apple  (captive.apple.com): expects an exact
                //     `<HTML>...Success...</HTML>` body — a 302 fails that
                //     comparison and the CNA mini-browser follows Location.
                //   Android (connectivitycheck.{gstatic,android}.com):
                //     expects a bare 204 — a 302 trips the "sign in to
                //     network" notification, which opens Location.
                //   Windows (www.msftconnecttest.com / www.msftncsi.com):
                //     expects exact plaintext bodies; a 302 fails NCSI's
                //     check too, but note honestly: NCSI's auto-launch-a-
                //     browser behavior is less consistent across Windows
                //     versions than Apple/Android's — sometimes it's only a
                //     taskbar toast, not an auto-opened browser. Don't
                //     overclaim it "just works" there.
                // The captive sheet's own flow: the splash page the probe
                // 302s land on, and the Accept that releases the sheet.
                ("GET", "/welcome") => {
                    ("200 OK", "text/html; charset=utf-8", WELCOME_HTML.into())
                }
                ("POST", "/captive/ack") => {
                    let ip = peer.ip();
                    {
                        let mut acks = ACKED.lock().unwrap();
                        match acks.iter_mut().find(|(a, _)| *a == ip) {
                            Some((_, seen)) => *seen = Instant::now(),
                            None => acks.push((ip, Instant::now())),
                        }
                    }
                    tokio::spawn(nft_acked("add", ip)); // packet-layer release
                    ("200 OK", "application/json", r#"{"ok":true}"#.into())
                }
                // An acked device's probes get the exact "network is clean"
                // answer each OS expects — that's what flips the sheet's
                // Cancel into Done and lets it close. Only ever per-device,
                // post-Accept (see ACKED above).
                ("GET", "/hotspot-detect.html") | ("GET", "/library/test/success.html")
                    if acked =>
                {
                    ("200 OK", "text/html",
                     "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>".into())
                }
                ("GET", "/generate_204") if acked => {
                    ("204 No Content", "text/plain", String::new())
                }
                ("GET", "/connecttest.txt") if acked => {
                    ("200 OK", "text/plain", "Microsoft Connect Test".into())
                }
                ("GET", "/ncsi.txt") if acked => {
                    ("200 OK", "text/plain", "Microsoft NCSI".into())
                }
                ("GET", "/hotspot-detect.html") | ("GET", "/library/test/success.html")
                | ("GET", "/generate_204")
                | ("GET", "/connecttest.txt") | ("GET", "/ncsi.txt") => {
                    ("302 Found", "text/plain", String::new())
                }
                // Any other GET that arrives asking for a FOREIGN host can
                // only be here because the AP's captive-capture NAT
                // (hub-ap-setup.sh) steered it in — the client thinks it's
                // talking to the internet. This is what catches probe
                // hostnames/paths the explicit arms above don't enumerate
                // (Firefox's detectportal, Android's /gen_204, whatever
                // ships next): unacked devices get the captive redirect,
                // acked ones a quiet no-content (their real traffic isn't
                // ours to bounce around). Requests addressed to US by name
                // (10.42.0.1, hub.local, localhost dev) keep their honest
                // 404 below — a typo'd dashboard URL should fail loudly,
                // not bounce home.
                ("GET", _) if foreign_host(&req) => {
                    if acked {
                        ("204 No Content", "text/plain", String::new())
                    } else {
                        ("302 Found", "text/plain", String::new())
                    }
                }
                _ => ("404 Not Found", "text/plain", "not found".into()),
            };
            // Optional Location header, minimally bolted onto the response
            // builder below (same hand-rolled style, no framework) — every
            // 302 this server issues is captive steering, and it points at
            // the welcome/release page, NOT the dashboard: the sheet's
            // sandbox is a dead end for sign-ins (see WELCOME_HTML).
            let location = (status == "302 Found")
                .then_some("Location: http://10.42.0.1/welcome\r\n")
                .unwrap_or_default();
            // ACAO *: /fleet is public-read JSON, and the rover setup page
            // (better-robotics.github.io) prefills the hub address from it.
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                 {location}Access-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
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
    tokio::spawn(reap_acks());
    let ap_ssid = hub::wifi::ap_ssid().await; // stable while running (MAC-derived profile)
    tokio::spawn(serve_http(uplink, http.clone(), locator.clone(), ap_ssid));

    let port = http.rsplit(':').next().unwrap_or("8000");
    println!("[hubd] dashboard: http://{host}:{port} (fleet JSON at /fleet)");
    if ide_dir().exists() {
        println!("[hubd] ide: http://{host}:{port}/ide/?hub={host}");
    }
    println!("[hubd] rovers/sim clients: point at the broker, {locator} (see mosquitto.example.conf)");

    // hubd holds no transport session — Mosquitto is a separate process.
    // Park so the HTTP chassis (dashboard, /fleet) stays up.
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
