#!/usr/bin/env bash
# install.sh — build hubd natively on this host and install it as a systemd
# service. Host-agnostic: works on any systemd Linux box (a Pi is one). Run it
# *on the target* (it native-builds for the host arch — no cross toolchain).
#
#   git clone https://github.com/better-robotics/hub && cd hub/pi
#   sudo ./deploy/install.sh
#
# Cross-compiling from a dev machine is the later optimization (see README);
# native build is the lowest-risk first deploy.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX=/opt/hub

if [[ $EUID -ne 0 ]]; then
  echo "run with sudo (installs to $PREFIX + enables a systemd unit)" >&2
  exit 1
fi

# Build as the invoking user so cargo's cache isn't root-owned.
BUILD_USER="${SUDO_USER:-root}"
echo "[install] building hubd (release, native arch) as $BUILD_USER…"
sudo -u "$BUILD_USER" bash -c "cd '$REPO_DIR' && cargo build --release --bin hubd"

echo "[install] staging $PREFIX…"
install -d "$PREFIX"
install -m 0755 "$REPO_DIR/target/release/hubd" "$PREFIX/hubd"

# ---- Zenoh: the router (zenohd) + the browser edge (ws-adapter) ----
# The transport. Robots connect to tcp/<gateway>:7447; the browser reaches the
# fabric through the ws-adapter beside zenohd (hubd is a client of neither — it
# only serves the page + /fleet locator). Provisioned before the payload below
# so the binaries/venv exist when payload.tsv enables the two services.
echo "[install] installing dependencies…"
apt-get update -qq
# `iw` is hubd's association check (captive release keys on it, see reap_acks);
# python3-venv + unzip provision the ws-adapter and unpack the zenohd release.
# Mirrored in image/stage-hub/00-hub/00-packages.
apt-get install -y -qq iw python3-venv unzip

# zenohd is a downloaded release, NOT apt (and NOT musl — Raspberry Pi OS is
# glibc; the musl standalone wants a loader the Pi lacks, scar in
# deploy/zenohd.service). Idempotent: an existing binary is kept. Version must
# match the firmware's zenoh-pico pin (robot/platformio.ini → 1.9.0).
ZENOH_VERSION="${ZENOH_VERSION:-1.9.0}"
ZENOH_DIR=/opt/hub/zenoh
install -d "$ZENOH_DIR"
if [[ ! -x "$ZENOH_DIR/zenohd" ]]; then
  case "$(uname -m)" in
    aarch64|arm64) triple=aarch64-unknown-linux-gnu ;;
    x86_64|amd64)  triple=x86_64-unknown-linux-gnu ;;
    *) triple="" ;;
  esac
  if [[ -n "$triple" ]]; then
    url="https://github.com/eclipse-zenoh/zenoh/releases/download/${ZENOH_VERSION}/zenoh-${ZENOH_VERSION}-${triple}-standalone.zip"
    echo "[install] fetching zenohd ${ZENOH_VERSION} (${triple})…"
    tmp="$(mktemp -d)"
    if curl -fsSL "$url" -o "$tmp/zenoh.zip" && unzip -oq "$tmp/zenoh.zip" -d "$tmp"; then
      install -m 0755 "$tmp/zenohd" "$ZENOH_DIR/zenohd"
      install -m 0644 "$tmp"/libzenoh_plugin_storage_manager.so "$ZENOH_DIR/" 2>/dev/null || true
      echo "[install]   $ZENOH_DIR/zenohd"
    else
      echo "[install] WARNING: couldn't fetch zenohd — install it into $ZENOH_DIR by hand before starting the hub (deploy/zenohd.service documents the steps)" >&2
    fi
    rm -rf "$tmp"
  else
    echo "[install] WARNING: unknown arch $(uname -m) — install zenohd ${ZENOH_VERSION} into $ZENOH_DIR manually (deploy/zenohd.service)" >&2
  fi
fi
# The router config (the ACL + fleet/estop storage) and the adapter script are
# payload.tsv rows — the loop below installs them. Only the ws-adapter venv is
# provisioned here (a venv is not a plain file copy): eclipse-zenoh + websockets.
WSA_DIR=/opt/hub/ws-adapter
echo "[install] provisioning the ws-adapter venv…"
install -d "$WSA_DIR"
[[ -d "$WSA_DIR/venv" ]] || python3 -m venv "$WSA_DIR/venv"
"$WSA_DIR/venv/bin/pip" install -q --upgrade pip eclipse-zenoh websockets

# ---- Payload: every file deploy/payload.tsv maps into place ----
# That manifest is the one list — the Pi image installs the same rows and CI
# asserts them in the built .img, so a new unit is one line there rather than
# four edits that can disagree. Its `on_host` column is read here and only here:
#   always  install unconditionally
#   wifi    only on a host with a Wi-Fi radio — the day-zero AP and the uplink
#           watchdog have nothing to do without one. This gates on "is this a
#           Wi-Fi host at all", NOT on any particular radio being plugged in
#           right now; see payload.tsv for why that distinction is load-bearing.
#   image   baked into the Pi image only (it needs boot-partition changes this
#           host-agnostic installer has no business making).
echo "[install] installing payload…"
units=()
while read -r src dest mode enable on_host; do
  if [[ $on_host == image ]]; then
    continue
  fi
  if [[ $on_host == wifi ]] && ! compgen -G "/sys/class/net/wlan*" > /dev/null; then
    echo "[install]   skip $dest — no Wi-Fi radio on this host"
    continue
  fi
  install -D -m "$mode" "$REPO_DIR/$src" "$dest"
  echo "[install]   $dest"
  if [[ $enable == yes ]]; then
    units+=("$(basename "$dest")")
  fi
done < <(grep -Ev '^[[:space:]]*(#|$)' "$REPO_DIR/deploy/payload.tsv")

# Operator credential — the one gated identity: engaging/clearing the fleet
# e-stop (the ws-adapter checks it; the router ACL is defense-in-depth). The
# hub's own Wi-Fi is the classroom's real boundary. CHANGE THIS before a real
# class, then restart the adapter:
#   sudo sed -i 's/^OPERATOR_PASS=.*/OPERATOR_PASS=<newpass>/' /etc/hub/operator.env
#   sudo systemctl restart ws-adapter
# (Only created if absent, so re-running install.sh won't clobber a real one.)
install -d -m 0755 /etc/hub
if [[ ! -f /etc/hub/operator.env ]]; then
  echo "[install] seeding the placeholder operator password — CHANGE IT before a real class"
  printf 'OPERATOR_PASS=change-me\n' > /etc/hub/operator.env
fi
chmod 0600 /etc/hub/operator.env

# Every unit payload.tsv marked `enable` (hubd, zenohd, ws-adapter, the Wi-Fi
# units), in one pass. (Guarded: a zero-arg `systemctl enable` is an error, and
# the Wi-Fi gate above can thin this list.)
if [[ ${#units[@]} -gt 0 ]]; then
  echo "[install] enabling units: ${units[*]}"
  systemctl daemon-reload
  systemctl enable --now "${units[@]}"
fi

# ---- IDE bundle (optional — needs internet at install time) ----
# hubd serves better-robotics/ide's built dist at /ide/ when present
# (HUB_IDE_DIR, default /usr/share/hub/ide). The release asset is the full
# static site INCLUDING its vendored deps — Blockly, Monaco, mqtt.js,
# MicroPython-WASM (ide's vendor/ is gitignored —
# a plain source tarball would be missing them; see ide's release.yml).
# Best-effort: a classroom Pi being (re)installed offline keeps its existing
# bundle, or runs without one.
IDE_DIR=/usr/share/hub/ide
echo "[install] fetching ide bundle…"
if curl -fsSL https://github.com/better-robotics/ide/releases/latest/download/ide-dist.tar.gz \
     -o /tmp/ide-dist.tar.gz 2>/dev/null; then
  rm -rf "$IDE_DIR.new"
  mkdir -p "$IDE_DIR.new"
  tar -xzf /tmp/ide-dist.tar.gz -C "$IDE_DIR.new"
  rm -rf "$IDE_DIR"
  mv "$IDE_DIR.new" "$IDE_DIR"
  rm -f /tmp/ide-dist.tar.gz
  echo "[install] IDE bundle installed → http://<this-host-ip>/ide/"
else
  echo "[install] no internet — skipped the IDE bundle ($([[ -d $IDE_DIR ]] && echo 'existing copy kept' || echo 'not installed'))"
fi

echo "[install] done. status:"
systemctl --no-pager status hubd.service zenohd.service ws-adapter.service || true
echo
echo "logs:   journalctl -u hubd -u zenohd -u ws-adapter -f"
echo "verify: curl http://<this-host-ip>/fleet          # dashboard chassis (hubd, :80)"
echo "        ss -ltn | grep -E '7447|9001'             # zenohd router + ws-adapter listening"
