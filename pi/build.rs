use std::process::Command;

// Stamp the build's git description into HUBD_VERSION so `hubd --version`
// identifies exactly which commit is deployed — the check a footgun probe was
// reaching for on 2026-07-19 (see hubd.rs main()). CI checks out fresh, so the
// release artifact's stamp is always current; `--always` yields the short SHA
// when no tags are fetched (checkout is shallow), which is what identifies a
// deploy. Falls back to the crate version if git isn't available at all.
fn main() {
    let desc = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=HUBD_VERSION={desc}");
    // Restamp a local rebuild when HEAD moves (CI is fresh regardless).
    println!("cargo:rerun-if-changed=../.git/HEAD");
}
