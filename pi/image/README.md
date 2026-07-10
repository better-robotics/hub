# hub Pi image — flash-and-go appliance

A CI-built Raspberry Pi 4 (arm64) image with **`hubd` + the Mosquitto broker
baked into the rootfs**. No ethernet, no on-device build: flash it, join the
hub's own `hub-XXXX` Wi-Fi from a phone and set the uplink network in the
dashboard, and the classroom hub is already running.

Why an image instead of editing an SD card: macOS can only write the boot (FAT)
partition, and an offline Pi can't `apt install`. Building in CI sidesteps both —
the build host is Linux with a network and root, so it writes the ext4 rootfs and
installs everything *at build time*. The Pi never installs anything.

## What's baked in
- **`hubd`** — the dashboard/HTTP chassis (serves the page on :80, `/fleet`, and
  device-served Wi-Fi setup at `/wifi/*`; not itself an MQTT client). Static musl
  aarch64 → `/opt/hub/hubd`, enabled via `deploy/hubd.service` (runs as root so it
  can drive NetworkManager for the Wi-Fi panel).
- **Mosquitto** — the MQTT broker every client talks to (`:1883` raw, `:9001`
  WebSocket), config + ACL from `deploy/mosquitto.conf` /
  `mosquitto-acl.example.conf`, placeholder creds seeded in the chroot.
- **USB-gadget recovery** — the USB-C port presents a composite **ECM ethernet +
  ACM serial** gadget: `ssh pi@10.55.0.1` and a serial console at `/dev/ttyGS0`,
  both independent of Wi-Fi/the hub. This is the see-logs / unbrick channel.
- user `pi` (key-only SSH), hostname `hub`, `avahi-daemon` (`hub.local`), US
  Wi-Fi domain, `network-manager`.

## What's deliberately absent
A single-purpose appliance, dieted in `01-run-chroot.sh` (each absence is
CI-asserted): **no swap** (`dphys-swapfile` purged — RAM is ample and a
swapfile only wears the SD), **no Bluetooth** (onboarding is device-served
Wi-Fi, never BLE — the bluez stack is purged *and* `dtoverlay=disable-bt`
turns the radio itself off), no `triggerhappy`, no apt/man-db maintenance
timers (an offline box has no updates to fetch), and **only the contracted
radios' firmware** — `firmware-brcm80211` (built-in AP) + `firmware-realtek`
(the Edimax STA dongle); atheros/libertas are purged. A new dongle model means
adding its firmware package back there — the offline Pi can't `apt install`.

## Provisioning vs recovery — two channels
Wi-Fi setup is device-served (hubd's `/wifi/*`, no app, no Bluetooth); recovery
is the out-of-band cable channel — the two needs split to the right tool:

| Need | Channel |
|---|---|
| Set Wi-Fi from a phone | **hubd `/wifi/*`** — join `hub-XXXX`, open the dashboard, pick the uplink net |
| See logs / recover headless | **USB-gadget** — `ssh pi@10.55.0.1` or serial `/dev/ttyGS0` |

## Build it
- **On demand:** Actions → `build-image` → *Run workflow*.
- **Release:** push a tag `pi-image-vN` → the image is attached to a GitHub
  Release as `hub-pi-image-vN.img.xz`.

Pipeline (`.github/workflows/build-image.yml`): build `hubd` (musl) → stage into
`files/` → `pi-gen` (arm64, last stage only) appends `stage-hub` to the Lite
stages → mount-assert the artifacts → publish `.img.xz`.

## Deploy (every card, cable-free)
1. Flash the released `.img.xz` (Raspberry Pi Imager → "Use custom").
2. Card → Pi 4 → power on (no network needed).
3. Phone → join the Pi's own **`hub-XXXX`** Wi-Fi → open `http://hub.local` (or
   `http://10.42.0.1`) → the dashboard's "Set up Wi-Fi" panel scans, pick the
   classroom SSID + password. The Pi joins on its uplink radio. (The AP's DHCP
   also advertises the dashboard via Captive Portal API `captive:false` —
   phones that honor RFC 8910/8908 surface the page on join, unprompted and
   with nothing blocked; the rest lose nothing.)
4. `hubd` is already running. Pin a **static/reserved IP** so rovers can hardcode
   the broker at `mqtt://<ip>:1883`.

If anything's wrong on first boot, plug a USB-C cable to a laptop and
`ssh pi@10.55.0.1` (or open `/dev/ttyGS0` at 115200) — that channel works even
with Wi-Fi down.

## First hardware boot — verified 2026-07-10
The CI mount-assert proves the artifacts are *present*; the first real-Pi boot
proved they *run*: day-zero AP on the air (`hub-a2f5`), dashboard HTTP 200 on
:80, broker ACL answering on :1883/:9001, gadget serial + ssh live, rootfs
auto-expanded, swap/BT/diet absences confirmed. It also caught two real bugs,
both fixed in `00-run.sh` and CI-asserted since:
- **usb0 sat unmanaged** — NM's udev rules default `g_ether` interfaces to
  `NM_UNMANAGED=1`, so the baked nmconnection never activated
  (`10-usb0-managed.conf` overrides it).
- **the gadget captured the laptop's internet** — a stock `shared` DHCP offer
  advertises the Pi as router+DNS, and macOS prefers wired over Wi-Fi
  (`dnsmasq-shared.d/10-usb0-no-route.conf` suppresses both, usb0 only).

Still to watch on future boots: the Wi-Fi setup panel's actual *join* of an
uplink network (`/wifi/*` scan verified; a join needs a second network in
range), and WPA2 on the hub AP (open for now — the ESP32 join scar, see
`pi/CLAUDE.md`).

Security: the broker ships with the per-team ACL and PLACEHOLDER credentials
baked in — change them with `mosquitto_passwd` before a real class. The serial
console autologs in as `pi`, and `pi` has passwordless sudo (pi-gen's default,
kept deliberately): **cable possession = root**, the same boundary as holding
the removable, unencrypted SD card — and root over the cable is what makes the
recovery channel able to actually *fix* the box (proven on first boot).
