#!/bin/bash -e
# Host context (has ROOTFS_DIR): drop hub artifacts into the target rootfs. CI
# stages files/ before pi-gen runs — binaries cross-built, units + scripts
# copied from deploy/ (the single source of truth, also used off-Pi by
# deploy/install.sh).

# --- hub dashboard chassis (serves the dashboard + device-served Wi-Fi setup;
# the old BLE provisiond was deleted 2026-07-09, provisioning is /wifi/* now) ---
install -d "${ROOTFS_DIR}/opt/hub"
install -m 0755 files/hubd            "${ROOTFS_DIR}/opt/hub/hubd"
install -m 0644 files/hubd.service       "${ROOTFS_DIR}/etc/systemd/system/hubd.service"

# --- Mosquitto broker config (the package itself comes from 00-packages;
# the passwd file is generated in the chroot, 01-run-chroot.sh) ---
install -m 0644 files/mosquitto.conf     "${ROOTFS_DIR}/etc/mosquitto/conf.d/hub.conf"
install -m 0644 files/mosquitto-acl.conf "${ROOTFS_DIR}/etc/mosquitto/hub-acl.conf"

# --- Day-zero hub-XXXX AP (wlan0, open, 10.42.0.1) — a hub nobody can find
# isn't a hub; the profile is created once on first boot, NM owns it after ---
install -m 0755 files/hub-ap-setup.sh "${ROOTFS_DIR}/usr/local/bin/hub-ap-setup.sh"
install -m 0644 files/hub-ap.service  "${ROOTFS_DIR}/etc/systemd/system/hub-ap.service"

# --- USB-gadget recovery channel (ECM ssh + ACM serial) ---
install -m 0755 files/usb-gadget-setup.sh "${ROOTFS_DIR}/usr/local/bin/usb-gadget-setup.sh"
install -m 0644 files/usb-gadget.service  "${ROOTFS_DIR}/etc/systemd/system/usb-gadget.service"

# dwc2 in peripheral mode + libcomposite so the USB-C port presents the gadget.
CONFIG="${ROOTFS_DIR}/boot/firmware/config.txt"
CMDLINE="${ROOTFS_DIR}/boot/firmware/cmdline.txt"
# Guard on dr_mode=peripheral specifically, NOT bare dwc2: stock Bookworm config
# already ships `[cm5] dtoverlay=dwc2,dr_mode=host`, so a `^dtoverlay=dwc2` guard
# false-matches and skips our line — leaving the Pi 4's USB-C in host mode (no
# UDC, gadget never enumerates). Append under a fresh [all] so it applies to the
# Pi 4 Model B (which matches none of the stock model-scoped dwc2 sections).
grep -q 'dr_mode=peripheral' "$CONFIG" || \
  printf '\n[all]\ndtoverlay=dwc2,dr_mode=peripheral\n' >> "$CONFIG"
# No Bluetooth hardware either: the bluez stack is purged in 01-run-chroot.sh,
# and this stops the kernel's own hci0 firmware probes (journal noise on a
# radio nothing uses). Must stay below the [all] header above.
grep -q 'dtoverlay=disable-bt' "$CONFIG" || \
  printf 'dtoverlay=disable-bt\n' >> "$CONFIG"
# modules-load must ride on the single cmdline line, after rootwait.
grep -q 'modules-load=dwc2' "$CMDLINE" || \
  sed -i 's/\brootwait\b/rootwait modules-load=dwc2,libcomposite/' "$CMDLINE"

# usb0 = 10.55.0.1/24 shared: the plugged-in laptop gets an address from the
# Pi, so `ssh pi@10.55.0.1` works with no router in sight.
install -d -m 0700 "${ROOTFS_DIR}/etc/NetworkManager/system-connections"
cat > "${ROOTFS_DIR}/etc/NetworkManager/system-connections/usb-gadget.nmconnection" <<'NMEOF'
[connection]
id=usb-gadget
type=ethernet
interface-name=usb0
autoconnect=true

[ethernet]

[ipv4]
method=shared
address1=10.55.0.1/24

[ipv6]
method=ignore
NMEOF
chmod 600 "${ROOTFS_DIR}/etc/NetworkManager/system-connections/usb-gadget.nmconnection"

# NM's udev rules mark g_ether interfaces NM_UNMANAGED=1 by default (gadget
# interfaces usually tether INTO a device, not serve from one) — without this
# override NM ignores usb0 entirely and the nmconnection above never activates.
# Hardware-discovered 2026-07-10, first real-Pi boot: gadget enumerated on the
# laptop, but usb0 sat DOWN/unmanaged and 10.55.0.1 never came up.
install -d "${ROOTFS_DIR}/etc/NetworkManager/conf.d"
cat > "${ROOTFS_DIR}/etc/NetworkManager/conf.d/10-usb0-managed.conf" <<'MANEOF'
[device-usb-gadget]
match-device=interface-name:usb0
managed=1
MANEOF

# usb0 hands out addresses but must NOT advertise itself as the laptop's
# default route or DNS: macOS ranks wired above Wi-Fi, so a stock `shared`
# DHCP offer silently captures the whole laptop's traffic into an uplink-less
# Pi (hardware-discovered 2026-07-10 — the laptop's DNS died on plug-in).
# dnsmasq auto-tags requests with the arriving interface, so this scopes to
# usb0 only; the wlan0 AP keeps advertising router+DNS (rovers need the NAT).
install -d "${ROOTFS_DIR}/etc/NetworkManager/dnsmasq-shared.d"
cat > "${ROOTFS_DIR}/etc/NetworkManager/dnsmasq-shared.d/10-usb0-no-route.conf" <<'NOROUTEEOF'
dhcp-option=tag:usb0,3
dhcp-option=tag:usb0,6
NOROUTEEOF

# Captive Portal API pointer (RFC 8910 option 114 → hubd's RFC 8908 /captive,
# which answers captive:false + the dashboard as venue-info-url): joining
# phones can surface the dashboard unprompted, with NOTHING blocked — this is
# an advertisement, not a portal (captive portals were chosen against;
# robot/CLAUDE.md § Status). AP leg only (tag:wlan0) — the usb0 recovery link
# must stay claim-free. Progressive nicety: RFC 8908 prefers TLS we can't
# validly present offline, so clients that insist will just ignore it.
cat > "${ROOTFS_DIR}/etc/NetworkManager/dnsmasq-shared.d/20-ap-capport.conf" <<'CAPPORTEOF'
dhcp-option=tag:wlan0,114,http://10.42.0.1/captive
CAPPORTEOF

# Login banner: print the hub's IP + router status on every interactive login
# (serial autologin and ssh both source /etc/profile.d/*.sh). This is where
# "what's my hub's address / is it up" is answered.
install -m 0644 files/hub-login-banner.sh "${ROOTFS_DIR}/etc/profile.d/hub-status.sh"

# Autologin on the USB-ACM serial console (physical-cable possession is the auth
# boundary, same as holding the SD card): drops straight to a `pi` shell. Note
# this IS root: pi-gen bakes NOPASSWD sudo for the first user, and we keep it —
# the SD card is removable and unencrypted, so physical access was always root,
# and the recovery channel being able to *fix* the box is its purpose
# (2026-07-10: the usb0-unmanaged bug was diagnosed and repaired live over
# exactly this console).
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/system/serial-getty@ttyGS0.service.d"
cat > "${ROOTFS_DIR}/etc/systemd/system/serial-getty@ttyGS0.service.d/autologin.conf" <<'AUTOEOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin pi --keep-baud 115200,57600,38400,9600 %I $TERM
AUTOEOF

# The dashboard's name is http://hub.local, served by avahi-daemon (mDNS,
# hostname `hub`) — no dnsmasq name mapping. Bare `http://hub` was dropped
# 2026-07-08: Apple devices don't resolve single-label names anyway (verified
# on iPhone), so the dnsmasq-hub-name drop-in served only some Android clients
# for a whole moving part. `10.42.0.1` is the always-works fallback.

# Connectivity checking: gives NM a real HTTP probe so `nmcli -g CONNECTIVITY
# general` can say `portal` (venue sign-in page blocking internet) instead of
# guessing `full` from the default route. hubd renders the verdict from its own
# HTTP probe (probe_uplink, self-testing the real path); this NM config just
# keeps NM's own connectivity state honest alongside it.
install -d "${ROOTFS_DIR}/etc/NetworkManager/conf.d"
cat > "${ROOTFS_DIR}/etc/NetworkManager/conf.d/20-connectivity.conf" <<'CONNEOF'
[connectivity]
uri=http://nmcheck.gnome.org/check_network_status.txt
CONNEOF
