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
