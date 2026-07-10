# hub — the classroom Robotics Hub

Students join the hub's Wi-Fi, open a dashboard, and drive ESP32 rovers over
MQTT. This repo is the **contract** (topics, envelopes, the dashboard) and the
**Raspberry Pi hub** that hosts it at classroom scale; the rover firmware — and
the ESP32 that can *become* a hub — lives at
[`better-robotics/robot`](https://github.com/better-robotics/robot).

## One contract, three room sizes

The room grows; the wire never changes — `robots/<team>/…` over MQTT, one
`dashboard.html`. The robot firmware picks its shape **per boot**, nothing is
configured:

```
 ISLAND                    ESP32 HUB                  PI HUB
 solo / home               small group · demo         full classroom

 ┌──────────────┐          ┌──────────────┐           ┌──────────────┐
 │  rover-XXXX  │          │   hub-XXXX   │           │  hub-pi-XXXX │
 │ the rover is │          │  any board,  │           │  Mosquitto + │
 │ its own hub: │          │  role = hub: │           │  hubd (pi/): │
 │ AP + broker  │          │  AP + broker │           │  per-team ACL│
 │ + dashboard  │          │  + dashboard │           │  ENFORCED    │
 └──────┬───────┘          └──────┬───────┘           └──────┬───────┘
        ▲                     ▲ ▲ ▲                     ▲ ▲ ▲ ▲ ▲
   one phone —             rovers & phones           the whole room —
   rover.local             join hub-XXXX —           rovers, phones,
                           hub.local                 laptops — hub.local

 isolation: single driver  connect-auth (honor)      broker-enforced ACL
 capacity:  1 + a phone    ~8–10 Wi-Fi clients       room-scale
 ──────────────────────────────────────────────────────────────────────
 a room resizes LIVE: an island yields when any hub-… appears, and every
 board prefers the Pi · a board can be locked to ONE hub (the hub pin),
 so a rogue hub-… can't absorb it
```

## The dashboard

One self-contained `dashboard.html` (mqtt.js inlined; also runs from `file://`
with the hub's address typed once), three tiers — each enforced by the
**broker**, not by page logic:

| tier | credential | can |
|---|---|---|
| public fleet view | none (anonymous read) | watch every robot live: telemetry, cameras, per-board settings |
| team | `teamN:password` | drive **its own** rover — joystick / D-pad, wire log visible (it's a teaching surface) |
| professor | `professor:password` | drive any robot · **Assign**: Blink 💡 a board's LED to find it on the desk, then give it a team, name, hub pin, motor pins |

Fresh boards arrive in an **unassigned** pool only the professor can drive.

## Layout

```
CONTRACT.md         the wire contract — topics, envelopes, identity/ACL model, cmd/* channels
envelopes/          message shapes (imu, pwm, rpc_set_led)
dashboard.html      the browser client — CANONICAL copy (the ESP32 hub vendors it;
                    robot/tools/sync-dashboard.sh --check gates drift)
mcp-bridge/         MCP tool server — drive the fleet from an LLM over the same contract
pi/                 the Raspberry Pi hub
├── src/            hubd — dashboard/HTTP chassis + device-served Wi-Fi setup (nmcli)
├── mosquitto*.conf broker config + per-team ACL
├── deploy/         systemd install: hubd · Mosquitto · day-zero hub AP · USB-gadget recovery
├── image/          pi-gen stage — the CI-baked, flash-and-go Pi image
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

**ESP32 hub:** flash the robot firmware
([browser flasher](https://better-robotics.github.io/)), flip the board's role
to *hub* on its `rover.local` settings page, join its Wi-Fi, open
`http://hub.local`.

Placeholder classroom credentials ship in `pi/classroom.example.json5` —
change them before a real class (`mosquitto_passwd`).

## The other repos

[`robot`](https://github.com/better-robotics/robot) — the unified rover +
ESP32-hub firmware · [`better-robotics.github.io`](https://github.com/better-robotics/better-robotics.github.io)
— the browser flasher · `hub-zenoh` — archived (the Zenoh evaluation baseline
MQTT won against) · `workbench` — a browser dev environment.
