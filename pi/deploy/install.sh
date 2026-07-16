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

echo "[install] installing hubd systemd unit…"
install -m 0644 "$REPO_DIR/deploy/hubd.service" /etc/systemd/system/hubd.service
systemctl daemon-reload
systemctl enable --now hubd.service

# ---- Mosquitto: the actual MQTT broker (hubd is not an MQTT client) ----
# Debian's packaged mosquitto ships its own systemd unit and includes
# /etc/mosquitto/conf.d/*.conf — so we drop our config there rather than write
# a custom unit. Broker-native ACL enforces classroom scoping (professor/team);
# see mosquitto-acl.example.conf.
echo "[install] installing Mosquitto broker…"
apt-get update -qq
apt-get install -y -qq mosquitto mosquitto-clients

install -m 0644 "$REPO_DIR/deploy/mosquitto.conf"           /etc/mosquitto/conf.d/hub.conf
install -m 0644 "$REPO_DIR/mosquitto-acl.example.conf"      /etc/mosquitto/hub-acl.conf

# Password file — one identity. The hub's own Wi-Fi is the classroom's real
# boundary (mosquitto-acl.example.conf); professor is the one credential
# that ACL can't give away for free (fleet/estop write). CHANGE THIS before
# a real class:
#   sudo mosquitto_passwd -b /etc/mosquitto/hub-passwd professor <newpass>
# (Only created if absent, so re-running install.sh won't clobber a real one.)
if [[ ! -f /etc/mosquitto/hub-passwd ]]; then
  echo "[install] seeding the placeholder professor password — CHANGE IT before a real class"
  mosquitto_passwd -b -c /etc/mosquitto/hub-passwd professor change-me
fi
# mosquitto runs as the `mosquitto` user and refuses world-readable cred/acl files.
chown mosquitto:mosquitto /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf
chmod 0600 /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf

systemctl enable mosquitto.service
systemctl restart mosquitto.service   # pick up the conf.d drop-in

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

# ---- Day-zero hub-XXXX AP (Pi-radio-specific: needs a Wi-Fi radio; the
# setup script selects the builtin by driver, never by interface name) ----
if compgen -G "/sys/class/net/wlan*" > /dev/null; then
  echo "[install] installing day-zero hub AP unit…"
  install -m 0755 "$REPO_DIR/deploy/hub-ap-setup.sh" /usr/local/bin/hub-ap-setup.sh
  install -m 0644 "$REPO_DIR/deploy/hub-ap.service"  /etc/systemd/system/hub-ap.service
  systemctl daemon-reload
  systemctl enable --now hub-ap.service
else
  echo "[install] no Wi-Fi radio — skipping the hub-XXXX AP unit (not a Pi/Wi-Fi host)"
fi

# ---- Uplink radio watchdog (only where the wedging driver is actually bound;
# the watchdog is specific to rtl8192cu, not to "a second radio") ----
if grep -qs rtl8192cu /sys/class/net/*/device/uevent 2>/dev/null \
   || readlink -f /sys/class/net/*/device/driver 2>/dev/null | grep -q rtl8192cu; then
  echo "[install] installing uplink watchdog unit…"
  install -m 0755 "$REPO_DIR/deploy/hub-uplink-watchdog.sh" /usr/local/bin/hub-uplink-watchdog.sh
  install -m 0644 "$REPO_DIR/deploy/hub-uplink-watchdog.service" /etc/systemd/system/hub-uplink-watchdog.service
  systemctl daemon-reload
  systemctl enable --now hub-uplink-watchdog.service
else
  echo "[install] no rtl8192cu radio — skipping the uplink watchdog (nothing to recover)"
fi

echo "[install] done. status:"
systemctl --no-pager status hubd.service mosquitto.service || true
echo
echo "logs:   journalctl -u hubd -u mosquitto -f"
echo "verify: curl http://<this-host-ip>/fleet                         # dashboard chassis (hubd, :80)"
echo "        mosquitto_sub -h <ip> -t 'robots/#'                      # broker + ACL — no credential needed"
