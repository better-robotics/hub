#!/usr/bin/env bash
# install.sh — build hubd natively on this host and install it as a systemd
# service. Host-agnostic: works on any systemd Linux box (a Pi is one). Run it
# *on the target* (it native-builds for the host arch — no cross toolchain).
#
#   git clone https://github.com/better-robotics/hub-mqtt && cd hub-mqtt
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

# Password file — PLACEHOLDER creds matching classroom.example.json5. CHANGE
# THESE before a real class:
#   sudo mosquitto_passwd -b /etc/mosquitto/hub-passwd <user> <newpass>
# (Only created if absent, so re-running install.sh won't clobber real creds.)
if [[ ! -f /etc/mosquitto/hub-passwd ]]; then
  echo "[install] seeding placeholder MQTT creds — CHANGE THESE before a real class"
  mosquitto_passwd -b -c /etc/mosquitto/hub-passwd rover     rover-secret
  mosquitto_passwd -b    /etc/mosquitto/hub-passwd professor change-me
  mosquitto_passwd -b    /etc/mosquitto/hub-passwd team1     change-me-team1
  mosquitto_passwd -b    /etc/mosquitto/hub-passwd team2     change-me-team2
fi
# mosquitto runs as the `mosquitto` user and refuses world-readable cred/acl files.
chown mosquitto:mosquitto /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf
chmod 0600 /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf

systemctl enable mosquitto.service
systemctl restart mosquitto.service   # pick up the conf.d drop-in

echo "[install] done. status:"
systemctl --no-pager status hubd.service mosquitto.service || true
echo
echo "logs:   journalctl -u hubd -u mosquitto -f"
echo "verify: curl http://<this-host-ip>/fleet                         # dashboard chassis (hubd, :80)"
echo "        mosquitto_sub -h <ip> -u team1 -P change-me-team1 -t 'robots/team1/#'   # broker + ACL"
