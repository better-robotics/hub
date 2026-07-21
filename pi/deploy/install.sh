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

# ---- Mosquitto: the actual MQTT broker (hubd is not an MQTT client) ----
# Debian's packaged mosquitto ships its own systemd unit and includes
# /etc/mosquitto/conf.d/*.conf — so we drop our config there rather than write
# a custom unit (deploy/payload.tsv puts it there). Broker-native ACL enforces
# classroom scoping (operator/team); see mosquitto-acl.example.conf.
# Installed before the payload below so /etc/mosquitto and the `mosquitto` user
# exist when the config, ACL, and their ownership land.
echo "[install] installing Mosquitto broker…"
apt-get update -qq
# `iw` is hubd's association check (the captive release keys on it, see
# reap_acks). Raspberry Pi OS ships it, so this is a no-op today — declared
# anyway because "the base image happens to carry it" is not a dependency, and
# a base bump that dropped it would silently strand every ack. Mirrored in
# image/stage-hub/00-hub/00-packages: an image installs from its own list, and
# a fresh card that came up without this would have nothing to say so (2fd8fdf).
apt-get install -y -qq mosquitto mosquitto-clients iw

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

# Password file — one identity. The hub's own Wi-Fi is the classroom's real
# boundary (mosquitto-acl.example.conf); operator is the one credential
# that ACL can't give away for free (fleet/estop write). CHANGE THIS before
# a real class:
#   sudo mosquitto_passwd -b /etc/mosquitto/hub-passwd operator <newpass>
# (Only created if absent, so re-running install.sh won't clobber a real one.)
if [[ ! -f /etc/mosquitto/hub-passwd ]]; then
  echo "[install] seeding the placeholder operator password — CHANGE IT before a real class"
  mosquitto_passwd -b -c /etc/mosquitto/hub-passwd operator change-me
fi
# mosquitto runs as the `mosquitto` user and refuses world-readable cred/acl files.
chown mosquitto:mosquitto /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf
chmod 0600 /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf

systemctl enable mosquitto.service
systemctl restart mosquitto.service   # pick up the conf.d drop-in

# Every unit payload.tsv marked `enable`, in one pass — mosquitto's own unit is
# above because the package, not the manifest, ships it. (Guarded: a zero-arg
# `systemctl enable` is an error, and the Wi-Fi gate above can thin this list.)
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
systemctl --no-pager status hubd.service mosquitto.service || true
echo
echo "logs:   journalctl -u hubd -u mosquitto -f"
echo "verify: curl http://<this-host-ip>/fleet                         # dashboard chassis (hubd, :80)"
echo "        mosquitto_sub -h <ip> -t 'robots/#'                      # broker + ACL — no credential needed"
