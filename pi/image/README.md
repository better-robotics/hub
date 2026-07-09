# hub Pi image — flash-and-go appliance

A CI-built Raspberry Pi 4 (arm64) image with **`hubd` + `provisiond` + the
Mosquitto broker baked into the rootfs**. No ethernet, no on-device build: flash
it, set Wi-Fi from a phone over Bluetooth, and the classroom hub is already
running.

Why an image instead of editing an SD card: macOS can only write the boot (FAT)
partition, and an offline Pi can't `apt install`. Building in CI sidesteps both —
the build host is Linux with a network and root, so it writes the ext4 rootfs and
installs everything *at build time*. The Pi never installs anything.

## What's baked in
- **`hubd`** — the dashboard/HTTP chassis (serves the page on :80, `/fleet`;
  not itself an MQTT client). Static musl aarch64 → `/opt/hub/hubd`, enabled via
  `deploy/hubd.service`.
- **Mosquitto** — the MQTT broker every client talks to (`:1883` raw, `:9001`
  WebSocket), config + ACL from `deploy/mosquitto.conf` /
  `mosquitto-acl.example.conf`, placeholder creds seeded in the chroot.
- **`provisiond`** — Wi-Fi provisioning over the **Improv BLE standard**
  (improv-wifi.com). A phone uses Improv's hosted Web-Bluetooth client (no app)
  to scan + send credentials; we join via NetworkManager. Native glibc aarch64
  (BlueZ needs libdbus) → `/opt/hub/provisiond`, `deploy/provisiond.service`.
- **USB-gadget recovery** — the USB-C port presents a composite **ECM ethernet +
  ACM serial** gadget: `ssh pi@10.55.0.1` and a serial console at `/dev/ttyGS0`,
  both independent of Wi-Fi/BLE/the hub. This is the see-logs / unbrick channel.
- user `pi` (key-only SSH), hostname `hub`, `avahi-daemon` (`hub.local`), US
  Wi-Fi domain, `bluez` + `network-manager`.

## Provisioning vs recovery — two channels
Improv is provisioning-only (no log/reboot in the spec), so the two needs split
to the right tool:

| Need | Channel |
|---|---|
| Set Wi-Fi from a phone | **Improv BLE** — Improv web client, no app, no cable |
| See logs / recover headless | **USB-gadget** — `ssh pi@10.55.0.1` or serial `/dev/ttyGS0` |

## Build it
- **On demand:** Actions → `build-image` → *Run workflow*.
- **Release:** push a tag `image-vN` → the `.img.xz` is attached to a GitHub Release.

Pipeline (`.github/workflows/build-image.yml`): build `hubd` (musl) + `provisiond`
(glibc) → stage into `files/` → `pi-gen` (arm64, last stage only) appends
`stage-hub` to the Lite stages → mount-assert the artifacts → publish `.img.xz`.

## Deploy (every card, cable-free)
1. Flash the released `.img.xz` (Raspberry Pi Imager → "Use custom").
2. Card → Pi 4 → power on (no network needed).
3. Phone → open the Improv web client → it finds `hub-XXXX` over Bluetooth →
   scan, pick the classroom SSID + password. The Pi joins.
4. `hubd` is already running. Pin a **static/reserved IP** so rovers can hardcode
   `tcp/<ip>:7447`.

If anything's wrong on first boot, plug a USB-C cable to a laptop and
`ssh pi@10.55.0.1` (or open `/dev/ttyGS0` at 115200) — that channel works even
with Wi-Fi and BLE both down.

## Known checks on first hardware boot
The CI mount-assert proves the artifacts are *present*; only a real Pi proves
they *run*. Watch:
- **BLE bring-up** — the Pi 4's BCM4345C0 controller reports LE Extended
  Advertising it can't honor, so `provisiond` advertises via BlueZ's *legacy*
  mgmt path (`btmgmt`), not `bluer`'s `advertise()` (which crash-loops on
  `Invalid Parameters`). GATT still rides bluetoothd. If BLE is missing, check
  `journalctl -u provisiond` and that the advert is up (`btmgmt info`).
- **USB gadget** — `dwc2` UDC must appear; the setup script waits 10s and logs to
  `/boot/firmware/usb-gadget.log` (readable by popping the SD into any host).
- **usb0 addressing** — NetworkManager must bring `usb0` up as `10.55.0.1`
  (`shared`) so the laptop gets a lease.

Security: no transport auth/ACL exists yet (hub#1); hub-zenoh's shape
(throwaway-plaintext usrpwd for classroom, TLS/mTLS for real deployments — see
its `deploy/README.md`) is the reference. The serial console
autologs in as `pi` (physical-cable = the auth boundary) but grants **no
passwordless root** — it reads journals via the `adm` group; Wi-Fi (re)config is
via BLE or the SD card. SSID/password from BLE are validated (length, no control
chars, no flag-smuggling) before reaching `nmcli`.
