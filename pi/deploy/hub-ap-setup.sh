#!/bin/bash
# Day-zero hub AP: make the Pi advertise `hub-XXXX` on FIRST boot, before any
# human has configured anything — a hub nobody can find isn't a hub. Creates
# the NetworkManager AP profile once (idempotent at the ROLE level: if a
# profile exists but sits on the wrong radio, it is recreated), then gets out
# of NM's way.
#
# Topology scars this encodes (pi/CLAUDE.md § Hub-AP mode):
# - The built-in brcmfmac is the reliable AP radio; the USB dongle takes the
#   STA/uplink leg. Selected BY DRIVER, never by interface name: kernel
#   enumeration order between the SDIO builtin and a USB dongle is a coin
#   flip per boot (hardware-discovered 2026-07-10 — the dongle won wlan0, the
#   AP came up on the scarred radio as hub-e959, and the rovers lost their
#   hub-a2f5). The name is not the identity.
# - OPEN AP, no WPA2: ESP32-C3 fails the 4-way handshake against this AP
#   (open joins in ~6 s). Classroom auth lives at the MQTT layer (broker ACL).
# - 10.42.0.1 is the constant locator (ipv4.method=shared hands out the rest).
set -uo pipefail

CON=hub-ap

# Captive-capture NAT: steer AP clients' DNS (53) and plain HTTP (80) to the
# hub itself, so the OS captive-portal popup lands on the dashboard no matter
# how the client resolves names.
#
# MIRRORED DESIGN — this nft table and the ESP32's src/captive_nat.c are one
# captive policy in two languages (better-robotics/robot; its banner points
# back here). Keep them in step: the capture set (DNS 53 + HTTP 80, never 443),
# the `acked` bypass, and the 90 s presence-reaper grace (hubd.rs reap_acks /
# robot's captive_reap_absent) are mirrored VALUES. Change one, change the
# other, or they drift apart on the two hub kinds a classroom might run.
#
# The dnsmasq hostname overrides
# (30-ap-captive-probes.conf) are the polite fast path, but they only work for
# clients that use the network's DNS — measured 2026-07-13: a Mac with Wi-Fi
# DNS pinned to 8.8.8.8 bypassed them entirely, resolved captive.apple.com to
# Apple's real address, and hung forever on the portal-blocked uplink. The IP
# layer is the one a resolver choice can't route around. HTTPS (443) passes
# through untouched — this redirects, it never impersonates. The usb0
# recovery leg (10.55.0.0/24) is outside the match by construction.
# Runs before the profile idempotency check: nft state dies with every boot,
# so this must apply even when the AP profile already exists.
# The `acked` set is hubd's packet-layer release: a device that tapped Accept
# on /welcome is exempted from the capture (hubd adds its address on ack, the
# presence reaper removes it when the device leaves), so its DNS and HTTP go
# real once there's nothing to hold it for — the lesson of the ESP portal's
# "stop lying once the uplink is real" (robot@f313b57), translated to nft.
nft delete table ip hub-captive 2>/dev/null || true
nft -f - <<'NFTEOF'
table ip hub-captive {
  set acked {
    type ipv4_addr;
  }
  chain capture {
    type nat hook prerouting priority dstnat; policy accept;
    ip saddr @acked accept
    ip saddr 10.42.0.0/24 ip daddr != 10.42.0.0/24 udp dport 53 counter dnat to 10.42.0.1
    ip saddr 10.42.0.0/24 ip daddr != 10.42.0.0/24 tcp dport 53 counter dnat to 10.42.0.1
    ip saddr 10.42.0.0/24 ip daddr != 10.42.0.0/24 tcp dport 80 counter dnat to 10.42.0.1
  }
}
NFTEOF
echo "hub-captive NAT applied (AP-client DNS+HTTP -> 10.42.0.1)"

# Clamp TCP MSS on forwarded (NAT'd) SYNs to the path MTU. A no-op on a normal
# 1500-MTU uplink; on a venue uplink with a smaller MTU (PPPoE, some VPNs) it is
# the difference between "some sites hang forever" — a PMTU black-hole, where the
# client advertises a 1460 MSS the uplink can't carry and the ICMP-too-big that
# would fix it is filtered — and everything loading. A separate table from
# hub-captive on purpose: this is a forward-chain mangle, that one a prerouting
# dnat, and NM's `ipv4.method=shared` owns the masquerade in its own table.
nft delete table ip hub-mss 2>/dev/null || true
nft -f - <<'NFTEOF'
table ip hub-mss {
  chain forward {
    type filter hook forward priority mangle; policy accept;
    tcp flags syn tcp option maxseg size set rt mtu
  }
}
NFTEOF
echo "hub-mss MSS clamp applied (forwarded SYNs -> path MTU)"

# The built-in radio, by driver identity. brcmfmac registers asynchronously
# at boot; don't race it.
ap_dev() {
  local d drv
  for d in /sys/class/net/wlan*; do
    [ -e "$d" ] || continue
    drv=$(basename "$(readlink -f "$d/device/driver" 2>/dev/null)" 2>/dev/null)
    if [ "$drv" = brcmfmac ]; then
      basename "$d"
      return 0
    fi
  done
  return 1
}

AP_DEV=""
for _ in $(seq 1 20); do
  AP_DEV=$(ap_dev) && break
  sleep 0.5
done
if [ -z "$AP_DEV" ]; then
  echo "no brcmfmac radio ever appeared — no built-in Wi-Fi?" >&2
  exit 1
fi

if nmcli -t -f NAME con show 2>/dev/null | grep -qx "$CON"; then
  BOUND=$(nmcli -g connection.interface-name con show "$CON" 2>/dev/null)
  if [ "$BOUND" = "$AP_DEV" ]; then
    echo "hub-ap profile already on $AP_DEV (idempotent exit)"
    exit 0
  fi
  # Self-heal: the profile was created on a boot where the builtin had a
  # different name (or by the pre-2026-07-10 name-pinned version of this
  # script). Recreate on the radio the role demands.
  echo "hub-ap profile bound to '$BOUND' but the builtin radio is $AP_DEV — recreating"
  nmcli con delete "$CON" || true
fi

set -e

# hub-XXXX, suffix from the AP radio's MAC (matches the ESP32 hub's naming:
# last two octets, lowercase hex). Driver-selected device = stable suffix.
MAC=$(cat "/sys/class/net/$AP_DEV/address")
SUFFIX=$(echo "$MAC" | awk -F: '{print $5 $6}')

# powersave 2 = disable, pinned: a power-saving AP misses client frames and
# produces associate-then-drop-before-DHCP loops. Bench scar 2026-07-10 — the
# single-radio AP+STA experiment left the builtin's power save ON and every
# ESP32 association flapped until the profile was bounced with powersave
# disabled.
nmcli con add type wifi ifname "$AP_DEV" con-name "$CON" autoconnect yes \
  connection.autoconnect-priority 10 \
  ssid "hub-${SUFFIX}" mode ap 802-11-wireless.band bg \
  802-11-wireless.powersave 2 \
  ipv4.method shared ipv4.addresses 10.42.0.1/24 ipv6.method ignore
nmcli con up "$CON" || true   # NM will also raise it itself via autoconnect

echo "hub-${SUFFIX} AP profile created on $AP_DEV (10.42.0.1)"
