# Running the hub as an always-on appliance

The hub is **three services**: `hubd` (the dashboard/HTTP chassis), `zenohd`
(the Zenoh router the robots talk to), and the **ws-adapter** (the browser edge,
WS-JSON on :9001) — hubd is a client of neither. This directory installs all
three on any systemd Linux box. **A Raspberry Pi is the worked example below;
nothing here is Pi-specific** except the network values, which you supply.

- `hubd.service` — generic systemd unit for hubd (`Restart=always`): serves the
  dashboard, `/fleet`, and device-served Wi-Fi setup (`/wifi/*`, driving nmcli —
  which is why the unit runs as root, not the old `DynamicUser`).
- `zenohd.service` — the Zenoh router (`zenoh-router.example.json5`: listen on
  :7447 + the router ACL). `zenohd` is a downloaded standalone release
  (install.sh fetches it to `/opt/hub/zenoh`), not an apt package.
- `ws-adapter.service` — the browser edge: terminates one WebSocket on :9001 and
  maps the dashboard's WS-JSON ops onto zenohd, applying the operator auth +
  per-owner claiming. Runs from its own venv under `/opt/hub/ws-adapter`.
- `install.sh` — native-builds `hubd` and installs it to `/opt/hub/`, fetches
  `zenohd`, provisions the ws-adapter venv, and installs the units + configs +
  seeded operator credential.
- `payload.tsv` — **the list of what gets installed where**, and the one place
  to add a unit. `install.sh` reads it here; the Pi image installs the same rows
  and CI asserts every one of them landed in the built `.img`. Its `on_host`
  column is the only thing that differs between a running host and the image:
  the AP and uplink watchdog need a Wi-Fi radio, and the USB recovery plane is
  image-only (it needs boot-partition changes this installer doesn't make).

```sh
git clone https://github.com/sprocket-robotics/hub && cd hub/pi
sudo ./deploy/install.sh
```

That builds hubd, installs it, and brings up `hubd` (dashboard on :80), `zenohd`
(the Zenoh router on :7447), and the `ws-adapter` (browser edge on :9001) on boot.

## The one host-varying decision: a stable address

Peers — especially ESP32 robots — dial a **literal IP**, so that IP must not
move:

- Give the host a **static IP or a DHCP reservation** on the classroom router,
  and hand robots/laptops that IP. (A Zenoh client that resolves `hub.local`
  could use the name, but the IP is the safe default for firmware. On the hub's
  own AP this is moot — the DHCP gateway *is* the hub, `tcp/<gateway>:7447`.)

Browsers, by contrast, reach the dashboard at **`http://hub.local`** (avahi/
mDNS — reliable on Apple and modern Android; `10.42.0.1` is the fallback). Bare
`http://hub` was dropped — Apple devices don't resolve single-label names, so
it served only some Android clients for a whole moving part (a dnsmasq drop-in).

Nothing in the repo hardcodes the address; it's the robot/device endpoint config.

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

## The other units: USB recovery plane

`install.sh` installs **hubd + zenohd + the ws-adapter** (the Wi-Fi data plane;
hubd also serves the device-served Wi-Fi setup panel over `/wifi/*` — see
`../src/wifi.rs`). The rest of this directory kits out the appliance's recovery
plane:

- `usb-gadget.service` + `usb-gadget-setup.sh` — USB-C recovery: ECM ethernet
  (`ssh pi@10.55.0.1`) + ACM serial (`/dev/ttyGS0`), independent of hubd.
- `hub-login-banner.sh` — status on every recovery-console login: hub IP, SSID,
  hubd health.

## Operating it

```sh
systemctl status hubd zenohd ws-adapter        # are they up
journalctl -u hubd -u zenohd -u ws-adapter -f  # logs
systemctl restart zenohd ws-adapter            # after editing the router config or rotating the operator pass
```

## Security: classroom vs real deployment

Classroom scoping is enforced by the **ws-adapter** (app-layer operator auth +
per-owner claiming) and the **`zenohd` router ACL** (`zenoh-router.example.json5`):
the hub's own Wi-Fi is the real boundary, not a login — every client, robot or
browser, authenticated or not, gets full read+write on `robots/**` and `pair/**`.
The only login is **operator**, and it only gates one thing: writing
`fleet/estop` (engaging/clearing the room-wide emergency stop). `install.sh`
seeds a **placeholder** operator password into `/etc/hub/operator.env`
— **change it before a real class**, the one credential there is to rotate:

```sh
sudo sed -i 's/^OPERATOR_PASS=.*/OPERATOR_PASS=<newpass>/' /etc/hub/operator.env
sudo systemctl restart ws-adapter
```

The seeded cred is throwaway plaintext (fine behind the AP perimeter). A real
public-facing deployment wants TLS on the Zenoh (:7447) and WS (:9001) listeners
— not in scope for the classroom appliance.
