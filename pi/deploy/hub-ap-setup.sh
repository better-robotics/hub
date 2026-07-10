#!/bin/bash
# Day-zero hub AP: make the Pi advertise `hub-XXXX` on FIRST boot, before any
# human has configured anything — a hub nobody can find isn't a hub. Creates
# the NetworkManager AP profile once (idempotent), then gets out of NM's way.
#
# Topology scars this encodes (pi/CLAUDE.md § Hub-AP mode):
# - wlan0 (built-in brcmfmac) is the reliable AP radio; the USB dongle takes
#   the STA/uplink leg, so this profile is pinned to wlan0.
# - OPEN AP, no WPA2: ESP32-C3 fails the 4-way handshake against this AP
#   (open joins in ~6 s). Classroom auth lives at the MQTT layer (broker ACL).
# - 10.42.0.1 is the constant locator (ipv4.method=shared hands out the rest).
set -uo pipefail

CON=hub-ap
if nmcli -t -f NAME con show 2>/dev/null | grep -qx "$CON"; then
  echo "hub-ap profile already exists (idempotent exit)"
  exit 0
fi

set -e

# brcmfmac registers wlan0 asynchronously at boot; don't race it.
for _ in $(seq 1 20); do
  [ -e /sys/class/net/wlan0 ] && break
  sleep 0.5
done
if [ ! -e /sys/class/net/wlan0 ]; then
  echo "wlan0 never appeared — no built-in Wi-Fi radio?" >&2
  exit 1
fi

# hub-XXXX, suffix from the AP radio's MAC (matches the ESP32 hub's naming:
# last two octets, lowercase hex).
MAC=$(cat /sys/class/net/wlan0/address)
SUFFIX=$(echo "$MAC" | awk -F: '{print $5 $6}')

# powersave 2 = disable, pinned: a power-saving AP misses client frames and
# produces associate-then-drop-before-DHCP loops. Bench scar 2026-07-10 — the
# single-radio AP+STA experiment left wlan0's power save ON and every ESP32
# association flapped until the profile was bounced with powersave disabled.
nmcli con add type wifi ifname wlan0 con-name "$CON" autoconnect yes \
  connection.autoconnect-priority 10 \
  ssid "hub-${SUFFIX}" mode ap 802-11-wireless.band bg \
  802-11-wireless.powersave 2 \
  ipv4.method shared ipv4.addresses 10.42.0.1/24 ipv6.method ignore
nmcli con up "$CON" || true   # NM will also raise it itself via autoconnect

echo "hub-${SUFFIX} AP profile created on wlan0 (10.42.0.1)"
