#!/bin/bash -e
# Inside the target rootfs (chroot). network-manager comes from 00-packages;
# binaries, units, and configs were placed by 00-run.sh — this step only
# enables them.

# Wi-Fi regulatory domain — radio stays disabled until this is set.
raspi-config nonint do_wifi_country US || true

# Mosquitto broker: seed PLACEHOLDER creds matching classroom.example.json5
# (change before a real class: mosquitto_passwd -b /etc/mosquitto/hub-passwd …).
# Config + ACL were staged by 00-run.sh; generate the passwd here where
# mosquitto_passwd exists, and lock down the cred/acl files (mosquitto refuses
# world-readable ones).
# `unassigned` = the fresh-board pool identity (firmware MQTT_USER default).
mosquitto_passwd -b -c /etc/mosquitto/hub-passwd unassigned unassigned-secret
mosquitto_passwd -b    /etc/mosquitto/hub-passwd professor  change-me
mosquitto_passwd -b    /etc/mosquitto/hub-passwd team1      change-me-team1
mosquitto_passwd -b    /etc/mosquitto/hub-passwd team2      change-me-team2
chown mosquitto:mosquitto /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf
chmod 0600 /etc/mosquitto/hub-passwd /etc/mosquitto/hub-acl.conf

# Each independently restartable: usb-gadget (recovery, before NM), serial
# console on the gadget, the day-zero hub-XXXX AP, the dashboard chassis +
# Wi-Fi setup (hubd), and the MQTT broker (Mosquitto).
systemctl enable usb-gadget.service
systemctl enable serial-getty@ttyGS0.service
systemctl enable hub-ap.service
systemctl enable hubd.service
systemctl enable mosquitto.service

# --- Appliance diet: a single-purpose broker/AP box, offline by design ---
# dphys-swapfile: no swap — mosquitto+hubd use a few MB on a 1–8 GB Pi, and a
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
