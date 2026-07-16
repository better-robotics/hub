# hub — the classroom Robotics Hub

Students join the hub's Wi-Fi, open a dashboard, and drive ESP32 rovers over
MQTT. This repo is the **contract** (topics, envelopes, the dashboard) and the
**Raspberry Pi hub** that hosts it at classroom scale; the rover firmware — and
the ESP32 that can *become* a hub — lives at
[`better-robotics/robot`](https://github.com/better-robotics/robot).

## One contract, three room sizes

The room grows; the wire never changes — `robots/<name>/…` over MQTT, one
`dashboard.html`. The robot firmware picks its shape **per boot**, nothing is
configured:

```
 ISLAND                    ESP32 HUB                  PI HUB
 solo / home               small group · demo         full classroom

 ┌──────────────┐          ┌──────────────┐           ┌──────────────┐
 │  rover-XXXX  │          │   hub-XXXX   │           │  hub-pi-XXXX │
 │ the rover is │          │  any board,  │           │  Mosquitto + │
 │ its own hub: │          │  role = hub: │           │  hubd (pi/): │
 │ AP + broker  │          │  AP + broker │           │  open ACL +  │
 │ + dashboard  │          │  + dashboard │           │  professor   │
 └──────┬───────┘          └──────┬───────┘           └──────┬───────┘
        ▲                     ▲ ▲ ▲                     ▲ ▲ ▲ ▲ ▲
   one phone —             rovers & phones           the whole room —
   rover.local             join hub-XXXX —           rovers, phones,
                           hub.local                 laptops — hub.local

 isolation: single driver  Wi-Fi perimeter (open)    Wi-Fi perimeter (open)
 capacity:  1 + a phone    ~8–10 Wi-Fi clients       room-scale
 ──────────────────────────────────────────────────────────────────────
 a room resizes LIVE: an island yields when any hub-… appears, and every
 board prefers the Pi · a board can be locked to ONE hub (the hub pin),
 so a rogue hub-… can't absorb it · the Wi-Fi perimeter is the boundary on
 every tier — the only gated credential anywhere is professor, for fleet/estop
```

## The dashboard

One self-contained `dashboard.html` (mqtt.js inlined; also runs from `file://`
with the hub's address typed once), two tiers — each enforced by the
**broker**, not by page logic:

| tier | credential | can |
|---|---|---|
| anyone | none | watch every robot live (telemetry, cameras, per-board settings) and drive any of them — joystick / D-pad, wire log visible (it's a teaching surface) |
| professor | `professor:password` | everything anyone can, plus engage/clear the fleet-wide **e-stop** · **Assign**: Blink 💡 a board's LED to find it on the desk, then give it a name, hub pin, motor pins |

The hub's own Wi-Fi is the real boundary, not a login — a robot's name in the
topic is an address, not a credential. Fresh boards arrive in an
**unassigned** pool anyone can drive, same as any other robot.

## Layout

```
CONTRACT.md         the wire contract — topics, envelopes, identity/ACL model, cmd/* channels
envelopes/          message shapes (imu, pwm, rpc_set_led)
dashboard.html      the browser client — CANONICAL copy (the ESP32 hub vendors it;
                    robot/tools/sync-dashboard.sh --check gates drift)
mcp-bridge/         MCP tool server — drive the fleet from an LLM over the same contract
pi/                 the Raspberry Pi hub
├── src/            hubd — dashboard/HTTP chassis + device-served Wi-Fi setup (nmcli)
│                   + serves the ide bundle at /ide/ when installed
├── mosquitto*.conf broker config + the open ACL (professor gated on fleet/estop only)
├── deploy/         systemd install: hubd · Mosquitto · day-zero hub AP · USB-gadget recovery
├── image/          the CI-baked, flash-and-go Pi image (official Lite base + customize-image.sh)
└── examples/       broker ACL + WebSocket transport tests (CI-gated)
```

## Run

**Pi, from the baked image** (recommended): take `hub.img.xz` from Releases
(or run the `build-image` workflow), flash an SD card, boot. The Pi raises an
open `hub-XXXX` network with the dashboard at `http://hub.local`; set its
internet uplink from the dashboard's Wi-Fi panel. A USB-C cable to a laptop is
the headless recovery console.

**Pi, onto an existing OS:**
```sh
cd pi && sudo ./deploy/install.sh    # hubd + Mosquitto (+ the hub AP on a wlan0 host)
```

Both paths also install the [`ide`](https://github.com/better-robotics/ide)
bundle, served at `http://hub.local/ide/` — snap blocks together or write
Python, and it runs in the browser and drives a rover over this repo's own
contract. Reachable from any device on the hub's network, phones included
(plain-http origin: no mixed-content wall between the page, the broker, and
the rovers).

**ESP32 hub:** flash the robot firmware
([browser flasher](https://better-robotics.github.io/)), flip the board's role
to *hub* on its `rover.local` settings page, join its Wi-Fi, open
`http://hub.local`.

`pi/deploy/install.sh` seeds a placeholder `professor` credential into
`/etc/mosquitto/hub-passwd` — the only login the classroom has — change it
before a real class (`mosquitto_passwd`).

## The other repos

[`robot`](https://github.com/better-robotics/robot) — the unified rover +
ESP32-hub firmware · [`better-robotics.github.io`](https://github.com/better-robotics/better-robotics.github.io)
— the browser flasher · [`ide`](https://github.com/better-robotics/ide) — the
blocks-and-Python editor served at `/ide/` · `hub-zenoh` — archived (the Zenoh
evaluation baseline MQTT won against) · `workbench` — a browser dev
environment, drifting from the classroom model.
