//! Team-code management — the dashboard's "Team codes" panel backend.
//!
//! The professor's whole credential workflow used to be ssh + mosquitto_passwd;
//! these endpoints make it dashboard-native: list identities, set/rotate a
//! team's code, delete a team. Changes go through `mosquitto_passwd` on the
//! live passwd file and a broker reload (SIGHUP re-reads it), so the panel and
//! the CLI stay two views of the same file.
//!
//! Auth: unlike `/wifi/*` (device-served setup, physical-proximity boundary),
//! codes are the classroom's auth ROOT — every mutating request carries the
//! professor's *current* password, verified against the broker itself by a
//! 2-packet MQTT CONNECT/CONNACK probe on 127.0.0.1:1883. That is not an MQTT
//! client (no session, no pub/sub — the no-relay rule stands); it is the only
//! verifier that can never disagree with the file mosquitto actually loaded.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PASSWD: &str = "/etc/mosquitto/hub-passwd";
/// Seeded by the image build; deleted on the first code change. Its presence
/// is the "this class still runs PLACEHOLDER codes" signal the dashboard nags
/// the professor about.
const PLACEHOLDER_MARKER: &str = "/etc/mosquitto/.placeholder-creds";

/// Identities the panel must not touch: `unassigned` is the fresh-board pool
/// secret compiled into the rover firmware's default config — rotating it
/// strands every unflashed board; `professor` can be *rotated* but not deleted.
const POOL_USER: &str = "unassigned";

/// Minimal MQTT 3.1.1 CONNECT → CONNACK auth probe. Return code 0 = accepted.
async fn broker_accepts(user: &str, pass: &str) -> bool {
    let Ok(mut sock) = tokio::net::TcpStream::connect("127.0.0.1:1883").await else {
        return false;
    };
    let (u, p) = (user.as_bytes(), pass.as_bytes());
    let client_id = b"hubd-auth-probe";
    // Variable header: protocol name "MQTT", level 4, flags (user+pass+clean), keepalive.
    let mut vh: Vec<u8> = vec![0, 4, b'M', b'Q', b'T', b'T', 4, 0xC2, 0, 30];
    for field in [&client_id[..], u, p] {
        vh.extend_from_slice(&(field.len() as u16).to_be_bytes());
        vh.extend_from_slice(field);
    }
    let mut pkt: Vec<u8> = vec![0x10];
    // Remaining-length varint (payloads here are far under 128, but encode properly).
    let mut len = vh.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        pkt.push(byte);
        if len == 0 {
            break;
        }
    }
    pkt.extend_from_slice(&vh);
    if sock.write_all(&pkt).await.is_err() {
        return false;
    }
    let mut resp = [0u8; 4];
    let ok = tokio::time::timeout(std::time::Duration::from_secs(3), sock.read_exact(&mut resp))
        .await
        .map(|r| r.is_ok() && resp[0] == 0x20 && resp[3] == 0)
        .unwrap_or(false);
    // Fixed-header DISCONNECT so the broker sees a clean close, not a drop.
    let _ = sock.write_all(&[0xE0, 0]).await;
    ok
}

fn list_users() -> Vec<String> {
    std::fs::read_to_string(PASSWD)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(':').map(|(u, _)| u.to_string()))
        .collect()
}

/// Both fields land in `mosquitto_passwd`'s argv — a leading '-' would be
/// parsed as a flag (`-D` deletes, `-c` truncates the file), the same argv
/// misparse `wifi.rs` guards against. First char alphanumeric kills the class.
fn valid_name(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && user.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && user.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn valid_pass(pass: &str) -> bool {
    !pass.is_empty() && pass.len() <= 64 && !pass.starts_with('-')
}

async fn reload_broker() {
    // Debian's mosquitto.service reload = SIGHUP = re-read passwd/acl.
    let _ = tokio::process::Command::new("systemctl").args(["reload", "mosquitto"]).status().await;
}

fn err(msg: &str) -> (&'static str, &'static str, String) {
    ("200 OK", "application/json", serde_json::json!({ "ok": false, "error": msg }).to_string())
}

/// `GET /codes/list` → `{users, placeholders}`. Read-only, unauthenticated by
/// design: usernames are already public knowledge (they are the topic ids the
/// anonymous fleet view renders), and `placeholders` tells the professor's
/// banner whether the class still runs seeded codes.
pub fn list_json() -> String {
    serde_json::json!({
        "users": list_users(),
        "placeholders": std::path::Path::new(PLACEHOLDER_MARKER).exists(),
    })
    .to_string()
}

/// `POST /codes/set` body `{auth, user, pass}` — create or rotate an identity.
pub async fn set_json(body: &str) -> (&'static str, &'static str, String) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let auth = v.get("auth").and_then(|s| s.as_str()).unwrap_or("");
    let user = v.get("user").and_then(|s| s.as_str()).unwrap_or("");
    let pass = v.get("pass").and_then(|s| s.as_str()).unwrap_or("");
    if !broker_accepts("professor", auth).await {
        return err("professor code rejected");
    }
    if !valid_name(user) {
        return err("names are 1-32 chars: a-z 0-9 - _ (starting with a letter or digit)");
    }
    if user == POOL_USER {
        return err("the pool identity is fixed — it matches the firmware default");
    }
    if !valid_pass(pass) {
        return err("codes are 1-64 chars and can't start with '-'");
    }
    let ok = tokio::process::Command::new("mosquitto_passwd")
        .args(["-b", PASSWD, user, pass])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return err("mosquitto_passwd failed");
    }
    let _ = std::fs::remove_file(PLACEHOLDER_MARKER);
    reload_broker().await;
    ("200 OK", "application/json", r#"{"ok":true}"#.into())
}

/// `POST /codes/del` body `{auth, user}` — remove a team identity.
pub async fn del_json(body: &str) -> (&'static str, &'static str, String) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let auth = v.get("auth").and_then(|s| s.as_str()).unwrap_or("");
    let user = v.get("user").and_then(|s| s.as_str()).unwrap_or("");
    if !broker_accepts("professor", auth).await {
        return err("professor code rejected");
    }
    if user == "professor" || user == POOL_USER {
        return err("professor and the pool identity cannot be deleted");
    }
    if !valid_name(user) || !list_users().iter().any(|u| u == user) {
        return err("no such team");
    }
    let ok = tokio::process::Command::new("mosquitto_passwd")
        .args(["-D", PASSWD, user])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return err("mosquitto_passwd failed");
    }
    reload_broker().await;
    ("200 OK", "application/json", r#"{"ok":true}"#.into())
}

// ---- Access requests: a team with no code yet asks from the login gate; the
// professor approves from the codes panel and the requester's poll delivers
// the minted code straight into their browser — nothing typed on either side.
//
// The queue is in-memory on purpose: requests are ephemeral (a hubd restart
// resets them to "ask again"), and the token is a claim ticket — the approved
// code is handed to whoever holds it, exactly once, then forgotten. The
// public request list carries names only, never tokens. The professor
// approves a NAME, not a phone: on a name dispute the code can be rotated,
// same trust boundary as reading a QR card off a neighbour's desk.

enum ReqState {
    Waiting,
    Approved(String), // the minted code, awaiting one-shot pickup
    Denied,
}

struct Pending {
    name: String,
    token: String,
    created: Instant,
    state: ReqState,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
const REQUEST_TTL: Duration = Duration::from_secs(30 * 60);
const REQUEST_CAP: usize = 20;

fn rand_hex(bytes: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf));
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Classroom-typeable code: 8 chars, no 0/O/1/l/i lookalikes.
fn gen_code() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    use std::io::Read;
    let mut buf = [0u8; 8];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf));
    buf.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

fn prune(q: &mut Vec<Pending>) {
    q.retain(|p| p.created.elapsed() < REQUEST_TTL);
}

/// `POST /codes/request` body `{name}` → `{ok, token}`. Unauthenticated — it
/// is the door someone with no credential knocks on. A name already waiting
/// is REJECTED, never token-replaced: the waiting list is public, so
/// replacement would let anyone enumerate a pending name and silently
/// redirect its approval to their own poll (caught by security review,
/// 2026-07-10). A requester who lost their token asks the professor to
/// dismiss the stale row, then knocks again — visible, not silent.
pub fn request_json(body: &str) -> (&'static str, &'static str, String) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let name = v.get("name").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
    if !valid_name(&name) {
        return err("names are 1-32 chars: a-z 0-9 - _ (starting with a letter or digit)");
    }
    if name == "professor" || name == POOL_USER {
        return err("that name is reserved");
    }
    if list_users().iter().any(|u| *u == name) {
        return err("that team already has a code — ask the professor for it");
    }
    let token = rand_hex(16);
    let mut q = PENDING.lock().unwrap();
    prune(&mut q);
    if q.iter().any(|p| p.name == name) {
        return err("that name is already requested — if that wasn't you, tell the professor");
    }
    if q.len() >= REQUEST_CAP {
        return err("too many open requests — ask the professor to clear some");
    }
    q.push(Pending { name, token: token.clone(), created: Instant::now(), state: ReqState::Waiting });
    ("200 OK", "application/json", serde_json::json!({ "ok": true, "token": token }).to_string())
}

/// `POST /codes/poll` body `{token}` → `{status}` and, once approved,
/// `{status:"approved", user, pass}` — delivered exactly once; the entry is
/// consumed. `unknown` covers expired/restarted/never-existed alike.
pub fn poll_json(body: &str) -> (&'static str, &'static str, String) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let token = v.get("token").and_then(|s| s.as_str()).unwrap_or("");
    let mut q = PENDING.lock().unwrap();
    prune(&mut q);
    let Some(i) = q.iter().position(|p| p.token == token && !token.is_empty()) else {
        return ("200 OK", "application/json", r#"{"status":"unknown"}"#.into());
    };
    let out = match &q[i].state {
        ReqState::Waiting => r#"{"status":"waiting"}"#.to_string(),
        ReqState::Approved(pass) => {
            let s = serde_json::json!({ "status": "approved", "user": q[i].name, "pass": pass })
                .to_string();
            q.remove(i);
            s
        }
        ReqState::Denied => {
            q.remove(i);
            r#"{"status":"denied"}"#.to_string()
        }
    };
    ("200 OK", "application/json", out)
}

/// `GET /codes/requests` → `{requests: [names]}`. Public read like
/// `/codes/list` — a pending name is about to become a public topic id anyway.
pub fn requests_json() -> String {
    let mut q = PENDING.lock().unwrap();
    prune(&mut q);
    let names: Vec<&str> = q
        .iter()
        .filter(|p| matches!(p.state, ReqState::Waiting))
        .map(|p| p.name.as_str())
        .collect();
    serde_json::json!({ "requests": names }).to_string()
}

/// `POST /codes/approve` body `{auth, name}` — professor-gated. Mints the
/// code server-side, creates the broker credential immediately (the rover can
/// be assigned before the requester even polls), parks the code for pickup.
/// Returns `{ok, user, pass}` so the panel can mint the QR card too.
pub async fn approve_json(body: &str) -> (&'static str, &'static str, String) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let auth = v.get("auth").and_then(|s| s.as_str()).unwrap_or("");
    let name = v.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if !broker_accepts("professor", auth).await {
        return err("professor code rejected");
    }
    // Validate against the queue, but never hold the lock across the awaits.
    {
        let mut q = PENDING.lock().unwrap();
        prune(&mut q);
        if !q.iter().any(|p| p.name == name && matches!(p.state, ReqState::Waiting)) {
            return err("no such request — it may have expired");
        }
    }
    let pass = gen_code();
    let ok = tokio::process::Command::new("mosquitto_passwd")
        .args(["-b", PASSWD, &name, &pass])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return err("mosquitto_passwd failed");
    }
    let _ = std::fs::remove_file(PLACEHOLDER_MARKER);
    reload_broker().await;
    let mut q = PENDING.lock().unwrap();
    if let Some(p) = q.iter_mut().find(|p| p.name == name) {
        p.state = ReqState::Approved(pass.clone());
        p.created = Instant::now(); // pickup window restarts at approval
    }
    ("200 OK", "application/json", serde_json::json!({ "ok": true, "user": name, "pass": pass }).to_string())
}

/// `POST /codes/deny` body `{auth, name}` — professor-gated; the requester's
/// next poll sees `denied` and stops.
pub async fn deny_json(body: &str) -> (&'static str, &'static str, String) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let auth = v.get("auth").and_then(|s| s.as_str()).unwrap_or("");
    let name = v.get("name").and_then(|s| s.as_str()).unwrap_or("");
    if !broker_accepts("professor", auth).await {
        return err("professor code rejected");
    }
    let mut q = PENDING.lock().unwrap();
    match q.iter_mut().find(|p| p.name == name && matches!(p.state, ReqState::Waiting)) {
        Some(p) => p.state = ReqState::Denied,
        None => return err("no such request"),
    }
    ("200 OK", "application/json", r#"{"ok":true}"#.into())
}
