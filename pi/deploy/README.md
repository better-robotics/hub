# Running the hub as an always-on appliance

The hub is **two services**: `hubd` (the dashboard/HTTP chassis) and
**Mosquitto** (the MQTT broker every client actually talks to — hubd is not an
MQTT client). This directory installs both on any systemd Linux box. **A
Raspberry Pi is the worked example below; nothing here is Pi-specific** except
the network values, which you supply.

- `hubd.service` — generic systemd unit for hubd (`Restart=always`).
- `mosquitto.conf` — broker config, dropped into `/etc/mosquitto/conf.d/`
  (Debian's packaged mosquitto includes it and runs it under its own unit).
- `install.sh` — native-builds `hubd`, installs it to `/opt/hub/`, and
  installs + configures Mosquitto (config, ACL, seeded creds).

```sh
git clone https://github.com/better-robotics/hub-mqtt && cd hub-mqtt
sudo ./deploy/install.sh
```

That builds hubd, installs it, and brings up both `hubd` (dashboard on :80) and
`mosquitto` (broker on :1883 raw + :9001 WebSocket) on boot.

## The one host-varying decision: a stable address

Peers — especially ESP32 rovers — dial a **literal IP**, so that IP must not
move:

- Give the host a **static IP or a DHCP reservation** on the classroom router,
  and hand rovers/laptops that IP. (An MQTT client library that resolves
  `hub.local` could use the name, but the IP is the safe default for firmware.)

Browsers, by contrast, reach the dashboard at **`http://hub.local`** (avahi/
mDNS — reliable on Apple and modern Android; `10.42.0.1` is the fallback). Bare
`http://hub` was dropped — Apple devices don't resolve single-label names, so
it served only some Android clients for a whole moving part (a dnsmasq drop-in).

Nothing in the repo hardcodes the address; it's the rover/device endpoint config.

## Worked example: Raspberry Pi

1. **Flash** Raspberry Pi OS **Lite (64-bit)** with Raspberry Pi Imager. Pre-seed
   hostname, SSH (your public key), and Wi-Fi/ethernet in the imager settings —
   it's a headless appliance, no monitor needed.
2. **Reserve its IP** on the router (see above).
3. **Toolchain:** `sudo apt install build-essential` then install rustup
   (`curl https://sh.rustup.rs -sSf | sh`).
4. `sudo ./deploy/install.sh` — native arm64 build, ~minutes on a Pi 4/5.

Rebuilds are rare (flash once, runs a semester), so native build beats setting up
a cross toolchain. If rebuild friction ever shows up, add a cross path
(`cross build --target aarch64-unknown-linux-gnu` from a dev machine, then ship
just the binary) — that's a later optimization.

## The other units: BLE + USB planes

`install.sh` installs **hubd + Mosquitto** (the Wi-Fi data plane). The rest of
this directory kits out the appliance's other two planes — the three-plane
topology is diagrammed in the [main README](../README.md#deployed-the-appliances-three-planes):

- `provisiond.service` — Wi-Fi provisioning over Improv BLE (runs `/opt/hub/provisiond`).
- `usb-gadget.service` + `usb-gadget-setup.sh` — USB-C recovery: ECM ethernet
  (`ssh pi@10.55.0.1`) + ACM serial (`/dev/ttyGS0`), independent of hubd/provisiond.
- `hub-login-banner.sh` — status on every recovery-console login: hub IP, SSID,
  hubd health. Improv carries provisioning only; status lives here.

## Operating it

```sh
systemctl status hubd mosquitto     # are they up
journalctl -u hubd -u mosquitto -f  # logs
systemctl restart mosquitto         # after editing /etc/mosquitto/conf.d/hub.conf or the ACL
```

## Security: classroom vs real deployment

Classroom scoping is enforced by **Mosquitto's broker-native ACL**
(`/etc/mosquitto/hub-acl.conf`, from `mosquitto-acl.example.conf`): anonymous
clients get read-only fleet telemetry (`robots/+/sys`); professor and per-team
logins get their scoped write access. `install.sh` seeds **placeholder**
credentials matching `classroom.example.json5` — **change them before a real
class**:

```sh
sudo mosquitto_passwd -b /etc/mosquitto/hub-passwd professor <newpass>
sudo systemctl restart mosquitto
```

The seeded creds are throwaway plaintext (fine behind the AP perimeter). A real
public-facing deployment wants TLS/mTLS on the listeners — not in scope for the
classroom appliance. The whole model is demonstrated end-to-end by
`examples/classroom-mosquitto-demo.sh`.
