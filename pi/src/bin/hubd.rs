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
/// Blockly/Monaco/mqtt.js/MicroPython-WASM), served at `/ide/`. On-disk
/// rather than embedded: it's a
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

// --- Response compression ---------------------------------------------------
//
// Everything big this server sends is text: the dashboard is ~646 KB of HTML
// with mqtt.js inlined, and the IDE bundle is mostly JS. Uncompressed, a cold
// `/ide/` open measured 5.4 MB over 27 requests — times a class, at the same
// minute, over the one AP radio that is already this deployment's measured
// bottleneck (single-radio AP+STA starved clients for up to 17.9 s; see
// pi/CLAUDE.md). gzip -6 takes the dashboard to 204 KB, a measured 31.6%.
//
// This is the half of the caching work that helps the load nobody had yet: the
// ETag below only pays off on a *re*visit, and a first day of class is all cold
// loads.
//
// Minification was measured and declined: ~3.6% on top of gzip, because DEFLATE
// already prices repeated identifiers near zero. It would buy a build step, a
// source/artifact split, and unreadable stack traces for a rounding error.

/// Below this, gzip's ~18-byte header and the CPU buy nothing — the response
/// already fit in one segment.
const GZIP_MIN: usize = 1024;

/// Types that actually shrink. png/jpg/ico/woff2 are already DEFLATE-compressed
/// internally, so gzipping them spends CPU to add bytes.
fn compressible(ctype: &str) -> bool {
    ["text/", "application/json", "application/javascript", "application/manifest+json",
     "image/svg+xml", "application/wasm"]
        .iter()
        .any(|p| ctype.starts_with(p))
}

/// Did the client offer gzip? Tolerates the `,`-list and `gzip;q=0.8` forms.
fn accepts_gzip(req: &str) -> bool {
    header_value(req, "accept-encoding")
        .is_some_and(|v| v.split(',').any(|t| t.trim().split(';').next() == Some("gzip")))
}

/// Memoized gzip output. Keyed by caller-supplied identity; `None` = never
/// cache, which is the only safe answer for a body computed per request.
static GZ_CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Arc<Vec<u8>>>>> =
    std::sync::OnceLock::new();

/// gzip `body`, or hand it back untouched if compression fails. Returns
/// `(bytes, is_gzipped)` — the flag reports what actually happened, so the
/// caller's `Content-Encoding` can never claim gzip over identity bytes.
///
/// `spawn_blocking` because this is real CPU on a Pi (~150 ms for the IDE's
/// 3.5 MB Monaco chunk) and the async workers are shared with the uplink probe:
/// 30 students opening the IDE at once would otherwise stall the runtime at
/// exactly the minute it must not.
///
/// Cached per (asset, version) — the bundle is static between installs (the
/// same property that makes the mtime+size ETag valid), so the class-start
/// burst compresses each file once rather than once per student. Unbounded by
/// design: the keyspace is the bundle's ~27 files, and install.sh restarts hubd.
async fn gzip_body(key: Option<String>, body: Vec<u8>) -> (Vec<u8>, bool) {
    let cache = GZ_CACHE.get_or_init(Default::default);
    if let Some(k) = &key {
        if let Some(hit) = cache.lock().unwrap().get(k).cloned() {
            return ((*hit).clone(), true);
        }
    }
    // Arc, not a move: the identity fallback below must still be able to send
    // the original bytes, so the blocking task borrows the body rather than
    // consuming it. Giving it away and having no way back is how "fall back to
    // identity" becomes "serve an empty page".
    let src = Arc::new(body);
    let out = tokio::task::spawn_blocking({
        let src = src.clone();
        move || {
            use std::io::Write;
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&src).and_then(|_| e.finish()).ok()
        }
    })
    .await;
    match out {
        Ok(Some(gz)) => {
            if let Some(k) = key {
                cache.lock().unwrap().insert(k, Arc::new(gz.clone()));
            }
            (gz, true)
        }
        // Compression failed, or the blocking task panicked. A Vec sink doesn't
        // fail in practice, but "in practice" is not a framing guarantee — send
        // identity rather than a body whose Content-Encoding lies about it.
        _ => (Arc::try_unwrap(src).unwrap_or_else(|a| (*a).clone()), false),
    }
}

/// `Vary` tracks whether a resource IS negotiable, not what this response
/// happened to be: a compressible asset served identity (because the client
/// offered no gzip) still has a gzip twin, and a cache that stored it without
/// `Vary` would key it on URL alone. Omitted for png/woff2, whose one
/// representation would otherwise fragment cache keys for nothing.
fn vary(ctype: &str) -> &'static str {
    if compressible(ctype) { "Vary: Accept-Encoding\r\n" } else { "" }
}

/// `Connection`/`Keep-Alive` headers. HTTP/1.1 is persistent by default and
/// needs no header at all, but HTTP/1.0 clients need the explicit token — and
/// saying it either way keeps the response self-describing.
fn conn_hdr(keep: bool) -> &'static str {
    if keep { "Connection: keep-alive\r\nKeep-Alive: timeout=15\r\n" } else { "Connection: close\r\n" }
}

/// Build the full HTTP response bytes for a `/ide` request. Bytes, not
/// String — the bundle has binary assets (png/ico/woff2) that the string
/// response path used by the API routes would corrupt.
async fn ide_serve(raw_path: &str, req: &str, keep: bool) -> Vec<u8> {
    let path = raw_path.split('?').next().unwrap_or(raw_path);
    if path == "/ide" {
        // Trailing slash matters: the page's relative module imports resolve
        // against the directory, not the bare segment.
        return format!(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: /ide/\r\nContent-Length: 0\r\n{}\r\n",
            conn_hdr(keep)
        )
        .into_bytes();
    }
    let rel = path.trim_start_matches("/ide/");
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let dir = ide_dir();
    // Bundle filenames are plain ASCII; rejecting dot-dot segments is the
    // whole traversal surface (no percent-decoding happens above).
    if rel.split('/').any(|seg| seg == "..") || rel.starts_with('/') {
        return format!("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n{}\r\n", conn_hdr(keep))
            .into_bytes();
    }
    let full = dir.join(rel);
    let meta = tokio::fs::metadata(&full).await.ok();
    let ctype = ide_mime(std::path::Path::new(rel));
    // Decided from metadata alone, before the file is read: the 304 below has
    // to know which representation it is validating without paying to build
    // either one.
    let gz = accepts_gzip(req)
        && compressible(ctype)
        && meta.as_ref().is_some_and(|m| m.len() >= GZIP_MIN as u64);
    // A validator from the file's own mtime+size, not a digest of its bytes:
    // hashing a 3.5 MB asset per request would cost the Pi more than the
    // download it saves. Weak by HTTP's definition, exact enough for a bundle
    // that only changes when install.sh replaces it.
    let etag_base = meta.as_ref().and_then(|m| {
        let secs = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(format!("{:x}-{:x}", secs, m.len()))
    });
    // The `-gz` suffix is not decoration. With `Vary: Accept-Encoding` this URL
    // now has two representations, and one ETag naming both is how a cache
    // hands gzip bytes to a client that never asked for them. Derived from
    // whether a response IS gzipped, never from whether we meant it to be —
    // gzip_body's identity fallback would otherwise ship bytes tagged `-gz`.
    let etag_of =
        |gz: bool| etag_base.as_ref().map(|b| format!("\"{b}{}\"", if gz { "-gz" } else { "" }));
    // `Cache-Control: no-cache` means "revalidate before reuse" — but shipped
    // with no validator there was nothing to revalidate WITH, so every browser
    // refetched the whole bundle every load: 27 requests, 5.4 MB measured on a
    // cold open, times a classroom, on the one AP radio. no-cache was never
    // wrong; it just needed an ETag to be honest. A 304 is ~90 bytes where the
    // body is up to 3.5 MB.
    //
    // Deliberately NOT `immutable` on the content-hashed names: it would need a
    // filename heuristic, and a false positive pins a mutable asset in every
    // student's cache forever with no way to bust it. Revalidating costs a LAN
    // round-trip and can never go stale — the bytes were the problem.
    if let (Some(tag), Some(want)) = (&etag_of(gz), header_value(req, "if-none-match")) {
        if want.split(',').any(|c| c.trim() == tag) {
            return format!(
                "HTTP/1.1 304 Not Modified\r\nETag: {tag}\r\nCache-Control: no-cache\r\n\
                 {}Access-Control-Allow-Origin: *\r\n{}\r\n",
                vary(ctype),
                conn_hdr(keep)
            )
            .into_bytes();
        }
    }
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            // Cache key = path + ETag, which encodes (mtime, size, encoding) —
            // so an install.sh that swaps the bundle can't be served the old
            // version's compressed bytes. Path-prefixed because mtime+size is
            // unique per *version of a file*, not across files: two assets
            // written in the same second at the same size would collide.
            let (body, gzipped) = if gz {
                gzip_body(etag_of(true).map(|t| format!("{rel}:{t}")), bytes).await
            } else {
                (bytes, false)
            };
            let mut resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                 Cache-Control: no-cache\r\n{}{}{}\
                 Access-Control-Allow-Origin: *\r\n{}\r\n",
                body.len(),
                vary(ctype),
                if gzipped { "Content-Encoding: gzip\r\n" } else { "" },
                etag_of(gzipped).map(|t| format!("ETag: {t}\r\n")).unwrap_or_default(),
                conn_hdr(keep),
            )
            .into_bytes();
            resp.extend_from_slice(&body);
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
                 Content-Length: {}\r\n{}\r\n",
                body.len(),
                conn_hdr(keep)
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

/// Board addresses the HUB OBSERVED, for the jobs that must not trust a board's
/// word about where it lives.
///
/// A rover's `sys` beacon carries an `ip` field, and the ACL grants `robots/#`
/// to every client with no credential (`mosquitto-acl.example.conf`), on an open
/// AP. So `sys.ip` is chosen by whoever published the beacon — fine for showing
/// a fact on a card, disqualifying for anything the dashboard sends a secret to:
/// a fake rover pointing at a laptop would collect the instructor password from
/// the next person who pressed Update.
///
/// dnsmasq's lease file is the counter-fact. It is written by the DHCP server
/// that handed the address out, and a board's id derives from the very MAC
/// holding the lease — `robot`'s `rover_format_robot_id` is `rover-%02x%02x` of
/// the last two bytes of its STA MAC, which is the MAC that joins this AP. A
/// beacon can claim any `ip`; it cannot make our own DHCP server agree.
///
/// Not proof of identity — MAC spoofing exists — but it costs an attacker a
/// collision with the real board on our own L2 segment instead of one MQTT
/// publish anyone on the AP can send.
///
/// Every `dnsmasq-*.leases` is merged rather than reading the AP's by name:
/// which radio is the AP is decided by DRIVER, and wlan0/wlan1 is a per-boot
/// enumeration coin flip (see this directory's CLAUDE.md — an image once came up
/// with the AP on the other radio). A filename would be right until it wasn't.
fn vouched_boards() -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    let dir = match std::fs::read_dir("/var/lib/NetworkManager") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for e in dir.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("dnsmasq-") && name.ends_with(".leases")) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(e.path()) else { continue };
        for line in body.lines() {
            // `<expiry> <mac> <ip> <hostname> <client-id>`
            let mut f = line.split_whitespace();
            let (Some(_expiry), Some(mac), Some(ip)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            let octets: Vec<&str> = mac.split(':').collect();
            if octets.len() != 6 {
                continue;
            }
            // Only real hardware MACs. Every phone and laptop on this AP
            // randomises its address, and a randomised MAC has the
            // locally-administered bit set; a rover never does, because its id
            // comes from esp_read_mac's Espressif-assigned global address. The
            // first cut skipped this and cheerfully reported a MacBook as
            // "rover-6f73" and a phone as "rover-5367" — every DHCP client got a
            // rover name, because only two bytes decide one.
            let Ok(first) = u8::from_str_radix(octets[0], 16) else { continue };
            if first & 0x02 != 0 {
                continue;
            }
            // Lowercase to match the firmware's own %02x formatting exactly —
            // dnsmasq writes lowercase today, and a case mismatch here would
            // fail open (no vouched address → no Update button) rather than
            // loudly, which is the kind of bug that gets found in a classroom.
            let id = format!("rover-{}{}", octets[4].to_lowercase(), octets[5].to_lowercase());
            // Two devices claiming one id is the exact substitution this map
            // exists to stop, so an ambiguous id vouches for NOBODY. Last-write
            // -wins would let an attacker who spoofs a rover's low two bytes
            // simply out-lease it; a collision fails closed instead, and the
            // Update button disappears rather than pointing somewhere chosen.
            if out.insert(id.clone(), serde_json::Value::String(ip.to_string())).is_some() {
                out.insert(id, serde_json::Value::Null);
            }
        }
    }
    out.retain(|_, v| !v.is_null());
    out
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
        // `boards` is the hub's own answer to "where does rover-xxxx live",
        // as opposed to the beacon's. See vouched_boards.
        "boards": vouched_boards(),
    })
    .to_string()
}

/// `GET /wifi/status` — the venue network the uplink is on (or null) plus the
/// live internet verdict, so the setup panel can show "currently on X · online"
/// and confirm a join landed.
async fn wifi_status_json(uplink: &Uplink) -> String {
    let ssid = hub::wifi::uplink_ssid().await;
    serde_json::json!({
        "ssid": ssid, "uplink": *uplink.lock().unwrap(),
        // Every radio and the address it holds — see hub::wifi::interfaces. The
        // hub is the only thing that reliably knows where the hub is: a venue
        // LAN filters multicast and isolates clients, so mDNS and scanning both
        // fail from off-network, while the AP at 10.42.0.1 always answers.
        "ifaces": hub::wifi::interfaces().await,
    })
    .to_string()
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

/// One device's release from the captive capture: the MAC that tapped Accept,
/// the address it held when it did, and when its association was last seen.
/// **The MAC is the identity; the address is only a lease.** A release keyed by
/// address is inherited by whoever DHCP hands that address to next — `ip` is
/// carried solely because the nft set is `type ipv4_addr` and the packet layer
/// has no other handle.
struct Ack {
    mac: Option<String>,
    ip: std::net::IpAddr,
    seen: Instant,
}

/// Devices that tapped "Accept" on /welcome. From then on their OS probes get
/// the genuine success answers, so the captive sheet's button turns into "Done"
/// and the user can finish in a real browser — the public-Wi-Fi
/// splash-then-release flow. Per-device opt-in on purpose: a device that never
/// accepts keeps honest "captive" answers, so an offline hub never tricks it
/// into believing the uplink works (that lie is what breaks phones' cellular
/// fallback — the reason blanket fake-success stayed chosen-against). An Accept
/// means "this visit", not "this device forever" — `reap_acks` forgets devices
/// that disassociate, so the sheet greets the next person to open a shared
/// laptop. Classroom-day scale.
static ACKED: Mutex<Vec<Ack>> = Mutex::new(Vec::new());

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

/// The AP-mode interface, found by nl80211 type rather than by name. Radio
/// roles are selected by driver, never by interface name (CLAUDE.md § Hub-AP
/// mode): wlan0/wlan1 is a per-boot enumeration coin flip between the SDIO
/// builtin and the USB dongle, so hardcoding `wlan0` is a guess that is wrong
/// every other boot. `iw dev` prints an indented `Interface <name>` block per
/// netdev, plus an `Unnamed/non-netdev interface` block for the P2P device —
/// which carries a `type` line but no name, hence the reset.
fn parse_ap_iface(text: &str) -> Option<String> {
    let mut name: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(n) = t.strip_prefix("Interface ") {
            name = Some(n.trim().to_string());
        } else if t.starts_with("Unnamed/non-netdev") {
            name = None;
        } else if t == "type AP" && name.is_some() {
            return name;
        }
    }
    None
}

async fn ap_iface() -> Option<String> {
    let out = tokio::process::Command::new("iw").arg("dev").output().await.ok()?;
    parse_ap_iface(&String::from_utf8_lossy(&out.stdout))
}

/// The MACs currently associated to the AP — the authoritative answer the
/// neighbour table cannot give (see `reap_acks`). `None` means the question
/// could not be asked at all (no AP interface up, `iw` missing or failing),
/// which callers must read as "don't know", never as "nobody is here".
fn parse_station_macs(text: &str) -> std::collections::HashSet<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("Station "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|m| m.to_ascii_lowercase())
        .collect()
}

async fn associated_macs() -> Option<std::collections::HashSet<String>> {
    let dev = ap_iface().await?;
    let out = tokio::process::Command::new("iw")
        .args(["dev", &dev, "station", "dump"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_station_macs(&String::from_utf8_lossy(&out.stdout)))
}

/// The MAC answering for `ip`, from the kernel neighbour table. Called only on
/// Accept, where the device has just completed this request's TCP handshake and
/// its entry is therefore as fresh as it will ever be. The table is far too
/// volatile to poll — measured 2026-07-16, it listed none of three
/// definitely-associated stations seconds after listing all three — which is
/// the other half of why association, not reachability, drives the reaper.
fn parse_mac_for_ip(text: &str, ip: std::net::IpAddr) -> Option<String> {
    let ip_s = ip.to_string();
    for line in text.lines() {
        // "10.42.0.161 dev wlan0 lladdr 16:3e:5a:1c:44:02 REACHABLE"
        let mut fields = line.split_whitespace();
        if fields.next() != Some(ip_s.as_str()) {
            continue;
        }
        let mut rest = fields.skip_while(|t| *t != "lladdr");
        rest.next()?; // the marker itself; absent on a FAILED entry
        return rest.next().map(|m| m.to_ascii_lowercase());
    }
    None
}

async fn mac_for_ip(ip: std::net::IpAddr) -> Option<String> {
    let out = tokio::process::Command::new("ip").args(["neigh", "show"]).output().await.ok()?;
    parse_mac_for_ip(&String::from_utf8_lossy(&out.stdout), ip)
}

/// Presence reaper for ACKED, keyed on **association**. `iw dev <ap> station
/// dump` is the authoritative "is this device on the AP" answer; the kernel
/// neighbour table is not, and believing it was the bug. It lied in both
/// directions, each measured 2026-07-16 on the bench hub: ARP offload keeps a
/// *sleeping* phone answering, so an ack refreshed on reachability never lapsed
/// at all — and the table is volatile the other way too, listing none of three
/// associated stations seconds after listing them. Association tracks a session
/// the way reachability cannot: a suspended lid disassociates, a pocketed phone
/// does not.
///
/// The failure this fixes is a shared laptop. The release is device-scoped but
/// the user is account-scoped, so student B opens the cart Chromebook student A
/// accepted on, gets no sheet, and — signed into their own profile — has no
/// bookmark either. The old 15-minute grace was longer than the passing period
/// it had to beat.
///
/// Tool failure never expires anyone: a round that cannot ask is skipped whole.
async fn reap_acks() {
    const POLL: Duration = Duration::from_secs(30);
    // Shorter than the gap between two users of one device (a passing period,
    // ~5 min) so the sheet greets the next student; longer than a transient
    // re-association so a blip cannot re-summon it mid-class.
    const GRACE: Duration = Duration::from_secs(90);
    // A fresh hubd means a fresh ack list — clear any packet-layer releases a
    // previous run left in the nft set, so the two layers can't disagree.
    let _ = tokio::process::Command::new("nft")
        .args(["flush", "set", "ip", "hub-captive", "acked"])
        .output()
        .await;
    // Edge-triggered: the skip below is SAFE (nobody loses a release they
    // earned) but it degrades silently into the exact bug this reaper exists to
    // fix — releases that never expire. If the station list ever stops being
    // askable, that has to be sayable.
    let mut warned = false;
    loop {
        tokio::time::sleep(POLL).await;
        let idle = ACKED.lock().unwrap().is_empty();
        if idle {
            continue;
        }
        // "Don't know" is not "nobody is here" — a round that can't ask expires
        // no one, the same guarantee the ip-neigh path made on tool failure.
        let Some(stations) = associated_macs().await else {
            if !warned {
                warned = true;
                println!(
                    "[hubd] captive: no AP station list (iw missing, or no AP interface up) \
                     — accepted devices will not be re-greeted until this clears"
                );
            }
            continue;
        };
        warned = false;
        let now = Instant::now();
        // Scoped so the MutexGuard (not Send) is dropped before the awaits
        // below — required for reap_acks' future to stay spawnable.
        let expired: Vec<std::net::IpAddr> = {
            let mut acks = ACKED.lock().unwrap();
            for a in acks.iter_mut() {
                // An ack we could not tie to a MAC can't be checked against the
                // station table, so it rides the grace out and re-earns itself
                // with one tap. Unreachable in practice — see `mac_for_ip`.
                if a.mac.as_deref().is_some_and(|m| stations.contains(m)) {
                    a.seen = now;
                }
            }
            let mut expired = Vec::new();
            acks.retain(|a| {
                let keep = now.duration_since(a.seen) < GRACE;
                if !keep {
                    expired.push(a.ip);
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

/// Read one HTTP request off `sock`, looping until the whole thing has
/// actually arrived. A single `sock.read()` returns as soon as ANY data is
/// available — headers and a POST body routinely land in separate TCP
/// segments, so one read can return the headers alone. That silently
/// truncated `/wifi/connect`'s body to empty, which parsed as invalid JSON
/// and got misreported as "missing ssid" even though the client sent a
/// complete request (live-observed 2026-07-14: a phone tapped Connect on a
/// correctly-selected network and got exactly that error). Keeps reading
/// until the `\r\n\r\n` header terminator is seen AND, if a Content-Length
/// header named a body, that many bytes past it have arrived too — capped
/// well above anything this server's own POST bodies carry (SSIDs,
/// passwords, small JSON), so a malformed or hostile Content-Length can't
/// hang the connection reading forever.
///
/// Reads ONE request out of `carry`, refilling from the socket as needed, and
/// leaves anything past it in `carry` for the next call. That draining is what
/// makes a connection reusable: a client may put its next request in the same
/// segment as this one's tail, and a reader that returned the whole buffer
/// would hand those bytes to the router as garbage — or silently drop them.
///
/// `None` = no more requests on this connection (clean EOF, read error, or a
/// head over the cap). Not "empty request": an oversized or truncated head is
/// answered by closing, never by routing a fragment.
async fn read_request(sock: &mut tokio::net::TcpStream, carry: &mut Vec<u8>) -> Option<Vec<u8>> {
    const MAX_REQUEST: usize = 16 * 1024;
    let mut chunk = [0u8; 1024];
    loop {
        // Checked before reading, so a request already sitting in `carry` from
        // the last call's tail is served without waiting on a segment that may
        // never come.
        if let Some(header_end) = find_bytes(carry, b"\r\n\r\n").map(|i| i + 4) {
            let total = header_end + content_length(&carry[..header_end]);
            if carry.len() >= total {
                let rest = carry.split_off(total);
                return Some(std::mem::replace(carry, rest));
            }
        }
        if carry.len() >= MAX_REQUEST {
            return None;
        }
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None; // peer closed
        }
        carry.extend_from_slice(&chunk[..n]);
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Case-insensitive `Content-Length` header scan over the raw header block
/// (bytes up to and including the `\r\n\r\n` terminator). 0 if absent — a GET
/// or a bodyless POST (`/wifi/forget`, `/captive/ack`) needs no more bytes.
fn content_length(headers: &[u8]) -> usize {
    let headers = String::from_utf8_lossy(headers);
    headers
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Case-insensitive scan for a request header's value — `content_length`'s
/// shape, over the `&str` form the route match already holds. Stops at the
/// blank line so a body can't impersonate a header.
fn header_value<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines()
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)))
        .map(|(_, v)| v.trim())
}

/// Should this connection stay open after the response? HTTP/1.1 is persistent
/// unless the client says `close`; HTTP/1.0 is the reverse.
///
/// Honouring it is safe here only because every response this server builds
/// carries an accurate `Content-Length` (or is a 304, which by definition has
/// no body) — that framing is the only thing telling a client where one
/// response ends and the next begins. A route that ever streams a body without
/// one must send `close`, or it will corrupt whatever follows it.
fn wants_keepalive(req: &str) -> bool {
    let conn = header_value(req, "connection").unwrap_or("").to_ascii_lowercase();
    let mut tokens = conn.split(',').map(str::trim);
    if tokens.clone().any(|t| t == "close") {
        return false;
    }
    tokens.any(|t| t == "keep-alive")
        || req.lines().next().is_some_and(|l| l.ends_with("HTTP/1.1"))
}

/// How long a reused connection may sit idle before hubd reclaims it. The IDE
/// bundle's requests come back-to-back, so this only reaps genuinely finished
/// sockets and browsers' speculative preconnects.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(15);

async fn accept_forever(listener: TcpListener, uplink: Uplink, locator: String, ssid: String) {
    loop {
        let Ok((mut sock, peer)) = listener.accept().await else { continue };
        let uplink = uplink.clone();
        let locator = locator.clone();
        let ssid = ssid.clone();
        tokio::spawn(async move {
            // One task per CONNECTION, serving requests until the peer leaves
            // or goes idle — not one per request. A cold /ide/ open is 27
            // requests; under `Connection: close` that was 27 TCP handshakes,
            // each a round-trip on an AP this deployment has already measured
            // starving under load.
            let mut carry = Vec::new();
            loop {
                let read = tokio::time::timeout(KEEPALIVE_IDLE, read_request(&mut sock, &mut carry));
                let Ok(Some(buf)) = read.await else { return };
                let req = String::from_utf8_lossy(&buf);
                let keep = wants_keepalive(&req);
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
                // NO CORS/PNA PREFLIGHT — deliberately, and it must not come back
                // without the threat model coming with it.
                //
                // This used to answer every OPTIONS, unconditionally and before any
                // path match, with `Access-Control-Allow-Origin: *` plus
                // `Access-Control-Allow-Private-Network: true` — the explicit opt-out
                // of the browser protection whose entire job is stopping public web
                // pages from reaching 10.42.0.1. With `Allow-Methods: GET, POST` and
                // `Allow-Headers: Content-Type`, ANY page on the internet passed
                // preflight for a JSON POST to any route here. A student browses a
                // page with a hostile ad on the classroom Wi-Fi; its JS posts
                // {ssid,password} to /wifi/connect; wifi.rs shells out to nmcli and
                // the room's uplink is repointed at an attacker's AP. The response
                // is unreadable cross-origin, which buys nothing — the side effect
                // already happened. The address isn't a secret; it's the documented
                // constant.
                //
                // It existed for the github.io setup wizard, and that flow was
                // deleted 2026-07-09 in favour of device-served Wi-Fi setup (see
                // CLAUDE.md § "Wi-Fi setup is device-served" — "no hosted website").
                // The device-served flow is SAME-ORIGIN: it never preflights, so
                // nothing legitimate here needs this. It was the deleted feature's
                // residue, and pure liability.
                //
                // Sending nothing means an unexpected OPTIONS falls through to the
                // 404 arm below, which is the correct answer for a server with no
                // cross-origin API.
                // Workbench IDE bundle — binary-safe path, bypasses the string
                // response builder below.
                if method == "GET" && (path == "/ide" || path.starts_with("/ide/")) {
                    let resp = ide_serve(path, &req, keep).await;
                    drop(req);
                    if sock.write_all(&resp).await.is_err() || !keep {
                        return;
                    }
                    continue;
                }
                // The Wi-Fi setup panel POSTs `{ssid, password}` here; the body is
                // whatever follows the header terminator (creds are tiny — one read
                // of `buf` holds them).
                let post_body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
                // Did this device already tap Accept on /welcome? Decides whether
                // its OS probes get "captive" (302) or genuine success answers.
                let acked = ACKED.lock().unwrap().iter().any(|a| a.ip == peer.ip());
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
                        // Resolved here rather than in the reaper: this request's
                        // own TCP handshake just populated the neighbour entry.
                        let mac = mac_for_ip(ip).await;
                        {
                            let mut acks = ACKED.lock().unwrap();
                            acks.retain(|a| a.ip != ip);
                            acks.push(Ack { mac, ip, seen: Instant::now() });
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
                // ACAO *, scoped to /fleet ONLY. The reason given for it — "/fleet
                // is public-read JSON, and the rover setup page prefills the hub
                // address from it" — is true of exactly one route, but the header
                // was being stamped on every response, including /wifi/* and the
                // dashboard itself. Handing a blanket read grant to every origin
                // for endpoints that drive nmcli is surface with no caller.
                let cors = (path == "/fleet")
                    .then_some("Access-Control-Allow-Origin: *\r\n")
                    .unwrap_or_default();
                // Clickjacking: the meta CSP in dashboard.html CANNOT express
                // frame-ancestors (it's header-only), and this server sent no
                // X-Frame-Options — so the instructor's dashboard was framable by
                // any page. SAMEORIGIN, not DENY: the dashboard frames its own
                // /ide/ bundle, and DENY would break that too.
                let frame = "X-Frame-Options: SAMEORIGIN\r\n";
                // Only the two embedded constants may be memoized — they cannot
                // change while the process lives. Every other body here is computed
                // per request (`/wifi/scan`'s networks, `/fleet`'s uplink verdict),
                // and keying those by path is exactly how a cache comes to serve
                // last week's Wi-Fi list. Most never reach GZIP_MIN anyway.
                let gz_key = match path {
                    "/" | "/index.html" => Some("dashboard".to_string()),
                    "/icon.svg" => Some("icon.svg".to_string()),
                    _ => None,
                };
                let want_gz = body.len() >= GZIP_MIN && compressible(ctype) && accepts_gzip(&req);
                drop(req);
                let (body, gzipped) =
                    if want_gz { gzip_body(gz_key, body.into_bytes()).await } else { (body.into_bytes(), false) };
                let enc = if gzipped { "Content-Encoding: gzip\r\n" } else { "" };
                let vary = vary(ctype);
                let mut resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                     {location}{cors}{frame}{vary}{enc}{}\r\n",
                    body.len(),
                    conn_hdr(keep),
                )
                .into_bytes();
                resp.extend_from_slice(&body);
                if sock.write_all(&resp).await.is_err() || !keep {
                    return;
                }
            }
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

/// Parser tests for the captive release's association check. The fixtures keep
/// the exact *shape* of the bench hub's output (2026-07-16) — a Pi with the USB
/// dongle present, which is the layout that makes interface NAMES a coin flip
/// and puts an `Unnamed/non-netdev` P2P block between the two real interfaces.
/// That shape is the whole reason these parse rather than index.
///
/// Client MACs and the uplink SSID are substituted — a public repo is no place
/// for the names of whatever network the bench happened to be sitting on.
/// Vendor OUIs and the locally-administered bit are kept, because those two
/// carry the meaning: `16:3e:…` has the LAA bit set, i.e. it is a phone's
/// private address, which is the case that must never stop resolving.
///
/// These pin the parse, not the policy: `parse_ap_iface` returning None is not
/// a loud failure — it silently stops every release from ever expiring (see
/// `reap_acks`), so the format is the thing worth freezing.
#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim `iw dev`. Note wlan1 (the Edimax uplink) sorts FIRST and the AP
    // is wlan0 here — the reverse is equally likely on the next boot, which is
    // why nothing may key on the name.
    const IW_DEV: &str = "\
phy#2
\tInterface wlan1
\t\tifindex 6
\t\twdev 0x200000001
\t\taddr 74:da:38:0c:22:08
\t\tssid campus-uplink
\t\ttype managed
\t\tchannel 1 (2412 MHz), width: 20 MHz, center1: 2412 MHz
\t\ttxpower 20.00 dBm
phy#0
\tUnnamed/non-netdev interface
\t\twdev 0x2
\t\taddr 8a:a2:9e:6c:a2:f5
\t\ttype P2P-device
\t\ttxpower 31.00 dBm
\tInterface wlan0
\t\tifindex 3
\t\twdev 0x1
\t\taddr 88:a2:9e:6c:a2:f5
\t\tssid hub-a2f5
\t\ttype AP
\t\tchannel 6 (2437 MHz), width: 20 MHz, center1: 2437 MHz
\t\ttxpower 31.00 dBm
";

    #[test]
    fn ap_iface_is_found_by_type_not_by_name() {
        assert_eq!(parse_ap_iface(IW_DEV).as_deref(), Some("wlan0"));
    }

    /// The P2P block carries `type P2P-device` and no name. Without the reset
    /// it would inherit wlan1's name — harmless only by luck, since the very
    /// next `type` line decides an interface.
    #[test]
    fn unnamed_p2p_block_never_inherits_the_previous_name() {
        let only_p2p = "phy#0\n\tInterface wlan1\n\t\ttype managed\n\tUnnamed/non-netdev interface\n\t\ttype AP\n";
        assert_eq!(parse_ap_iface(only_p2p), None);
    }

    #[test]
    fn no_ap_up_is_none_not_a_guess() {
        assert_eq!(parse_ap_iface("phy#2\n\tInterface wlan1\n\t\ttype managed\n"), None);
    }

    // Verbatim `iw dev wlan0 station dump`, trimmed to two stations. The first
    // is an iOS private (locally-administered) address — the case that must
    // keep working, since every modern phone presents one.
    const STATION_DUMP: &str = "\
Station 16:3e:5a:1c:44:02 (on wlan0)
\tinactive time:\t17000 ms
\trx bytes:\t1600165
\ttx bitrate:\t72.2 MBit/s
\tauthorized:\tno
\tassociated:\tyes
Station d4:e9:f4:0b:11:07 (on wlan0)
\tinactive time:\t120 ms
\tassociated:\tyes
";

    #[test]
    fn station_macs_are_every_associated_device() {
        let macs = parse_station_macs(STATION_DUMP);
        assert_eq!(macs.len(), 2);
        assert!(macs.contains("16:3e:5a:1c:44:02"));
        assert!(macs.contains("d4:e9:f4:0b:11:07"));
    }

    /// An empty AP is a real, common state (no students yet) and must parse as
    /// "nobody associated" — distinct from `associated_macs()`'s None, which
    /// means "couldn't ask". Collapsing the two is what would expire everyone.
    #[test]
    fn empty_station_dump_is_empty_not_an_error() {
        assert!(parse_station_macs("").is_empty());
    }

    // Verbatim `ip neigh show`, including the FAILED entry (an address the
    // kernel probed and got nothing for) and the v6 link-local that shares the
    // phone's MAC — both present on the live hub.
    const IP_NEIGH: &str = "\
10.42.0.174 dev wlan0 FAILED 
10.42.0.99 dev wlan0 lladdr d4:e9:f4:0b:11:07 REACHABLE 
10.42.0.161 dev wlan0 lladdr 16:3e:5a:1c:44:02 STALE 
fe80::143e:5aff:fe1c:4402 dev wlan0 lladdr 16:3e:5a:1c:44:02 STALE
";

    /// STALE resolves the same as REACHABLE on purpose: this asks "who holds
    /// this address", not "can I reach it". The old reaper's REACHABLE-only
    /// test is exactly what made a live phone look absent.
    #[test]
    fn mac_for_ip_reads_lladdr_regardless_of_nud_state() {
        let ip = "10.42.0.161".parse().unwrap();
        assert_eq!(parse_mac_for_ip(IP_NEIGH, ip).as_deref(), Some("16:3e:5a:1c:44:02"));
    }

    #[test]
    fn mac_for_ip_does_not_prefix_match_a_longer_address() {
        // .17 must not match the .174 line — the bug a starts_with() would have.
        let ip = "10.42.0.17".parse().unwrap();
        assert_eq!(parse_mac_for_ip(IP_NEIGH, ip), None);
    }

    /// A FAILED entry exists but carries no lladdr. That device gets acked with
    /// mac: None and rides the grace out, rather than being handed a wrong MAC.
    #[test]
    fn failed_entry_yields_no_mac() {
        let ip = "10.42.0.174".parse().unwrap();
        assert_eq!(parse_mac_for_ip(IP_NEIGH, ip), None);
    }

    #[test]
    fn unknown_ip_yields_no_mac() {
        let ip = "10.42.0.222".parse().unwrap();
        assert_eq!(parse_mac_for_ip(IP_NEIGH, ip), None);
    }
}
