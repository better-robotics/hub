#!/bin/bash -e
# Inside the target rootfs (chroot). network-manager comes from 00-packages;
# binaries, units, and configs were placed by 00-run.sh — this step only
# enables them.

# Wi-Fi regulatory domain — radio stays disabled until this is set.
#
# NOT `|| true`. The line above states the stakes and the `|| true` ignored them:
# if raspi-config misbehaves in the chroot (no systemd, no radio — a plausible
# place for it to), the build stayed green and the flashed Pi never raised
# hub-XXXX. A hub is a hub because students can find its Wi-Fi, so that is not a
# degraded image, it is a brick with a green checkmark. The script runs under
# `bash -e`, so dropping the swallow is the whole fix: a failure here now stops
# the build instead of shipping.
#
# Not also asserted in build-image's verify step, deliberately: that check must
# grep the file the base actually writes, and which one that is (/etc/default/crda
# vs wpa_supplicant.conf) differs by release — it wants confirming against a real
# built image, not writing from memory. An assertion aimed at the wrong path is
# the absence-check trap this repo already knows about: it would read green while
# checking nothing.
raspi-config nonint do_wifi_country US

# Zenoh transport: bake the router binary + the ws-adapter venv, seed the
# operator credential. Mirrors deploy/install.sh (the manual path); the chroot
# is arm64 with build-time network, so the download + pip install run here. The
# router config + adapter script are payload.tsv rows 00-run.sh already placed.
ZENOH_VERSION=1.9.0
install -d /opt/hub/zenoh
tmp="$(mktemp -d)"
curl -fsSL "https://github.com/eclipse-zenoh/zenoh/releases/download/${ZENOH_VERSION}/zenoh-${ZENOH_VERSION}-aarch64-unknown-linux-gnu-standalone.zip" -o "$tmp/z.zip"
unzip -oq "$tmp/z.zip" -d "$tmp"
install -m0755 "$tmp/zenohd" /opt/hub/zenoh/zenohd
install -m0644 "$tmp"/libzenoh_plugin_storage_manager.so /opt/hub/zenoh/ 2>/dev/null || true
rm -rf "$tmp"

# ws-adapter venv (eclipse-zenoh + websockets — native aarch64 wheels).
python3 -m venv /opt/hub/ws-adapter/venv
/opt/hub/ws-adapter/venv/bin/pip install -q --upgrade pip eclipse-zenoh websockets

# Operator credential — the one gated identity, gating just the fleet-wide
# emergency stop. The hub's own Wi-Fi is the classroom's real boundary.
# PLACEHOLDER; change before a real class:
#   sed -i 's/^OPERATOR_PASS=.*/OPERATOR_PASS=<newpass>/' /etc/hub/operator.env
install -d -m0755 /etc/hub
printf 'OPERATOR_PASS=change-me\n' > /etc/hub/operator.env
chmod 0600 /etc/hub/operator.env

# Enable every unit deploy/payload.tsv marks — the same list 00-run.sh just
# installed from, so a unit can't be placed in the rootfs and then forgotten
# here. The manifest is staged to /tmp by ../customize-image.sh: this step runs
# inside the chroot and can't see the stage dir, which is exactly why this list
# used to be retyped. Each is independently restartable: usb-gadget (recovery,
# before NM), the day-zero hub-XXXX AP, the dashboard chassis + Wi-Fi setup
# (hubd), and the uplink watchdog (idles unless the wedging USB radio is
# present).
while read -r src dest mode enable on_host; do
    [ "$enable" = yes ] || continue
    systemctl enable "$(basename "$dest")"
done < <(grep -Ev '^[[:space:]]*(#|$)' /tmp/hub-payload.tsv)

# Not a manifest row: the serial console is a stock template unit instantiated
# on the gadget's tty. (zenohd + ws-adapter ARE manifest rows now — enabled by
# the loop above, like hubd.)
systemctl enable serial-getty@ttyGS0.service

# --- Appliance diet: a single-purpose broker/AP box, offline by design ---
# dphys-swapfile: no swap — zenohd+hubd use a few MB on a 1–8 GB Pi, and a
#   swapfile on the SD card only wears it (also skips the 100 MB first-boot
#   swap allocation).
# triggerhappy: hotkey daemon for input devices the hub doesn't have.
# bluetooth: the product deliberately has none (BLE onboarding was deleted
#   for device-served Wi-Fi setup) — drop the stack and its boot services.
# firmware-atheros/-libertas: the hub's radio contract is explicit — brcmfmac
#   (built-in) is the AP, the Edimax RTL8188CUS the STA leg — so only
#   firmware-brcm80211 + firmware-realtek (+ misc) earn their bytes. An
#   unlisted future dongle needs its firmware added back HERE (the Pi is
#   offline; it can't apt install).
for p in dphys-swapfile triggerhappy bluez bluez-firmware pi-bluetooth \
         modemmanager firmware-atheros firmware-libertas; do
    apt-get -y purge "$p" 2>/dev/null || true
done
apt-get -y autoremove --purge
apt-get clean

# Offline appliance: the apt/man-db maintenance timers have no network and no
# audience — they'd only spin the SD and log failures.
systemctl disable apt-daily.timer apt-daily-upgrade.timer man-db.timer 2>/dev/null || true
