# hub — the classroom Robotics Hub

One wire contract, two implementations. Students join the hub's Wi-Fi, open a
dashboard, and drive ESP32 rovers over MQTT — from a Raspberry Pi appliance, or
from a single ESP32 that *is* the whole hub.

```
             shared contract (top level)
   envelopes/  ·  dashboard.html  ·  mcp-bridge/  ·  CONTRACT.md
        │                                    │
        ▼                                    ▼
   ┌──────────┐                        ┌──────────┐
   │   pi/    │  Raspberry Pi          │  esp32/  │  one ESP32 =
   │          │  Rust hubd (HTTP) +    │          │  AP + NAT + broker +
   │          │  Mosquitto broker      │          │  WS bridge + dashboard
   └──────────┘                        └──────────┘
   production target                   small-classroom, no Pi
```

Both implementations embed the **same** `dashboard.html` and speak the **same**
`envelopes/` — a single source of truth at the top of this repo, so the two can
never silently drift on the wire format (the reason this is one repo, not
several: a breaking contract change lands in the contract *and* both consumers
in one atomic commit).

## Layout

```
hub/
├── CONTRACT.md         the wire contract — topic scheme, envelope table, directions
├── envelopes/          message shapes (imu, pwm, rpc_set_led) — language bindings live in each impl
├── dashboard.html      the browser client (mqtt.js + favicon inlined; also runs standalone from file://)
├── mcp-bridge/         MCP tool server — drive the fleet from an LLM over the same contract
├── pi/                 Raspberry Pi implementation — Rust hubd + Mosquitto + Pi image (was hub-mqtt)
│   ├── src/            hubd (dashboard/HTTP chassis) + provisiond (BLE)
│   ├── mosquitto*.conf broker config + ACL (classroom scoping)
│   ├── deploy/ image/  systemd install + CI-baked Pi image
│   └── examples/       broker ACL + WebSocket transport tests (CI-gated)
└── esp32/              ESP32 implementation — ESP-IDF firmware (was hub-esp32)
    └── main/           AP+STA+NAT + on-chip Mosquitto + WS↔TCP bridge + served dashboard
```

## Run

**Pi** (needs a Linux/Pi host):
```sh
cd pi && sudo ./deploy/install.sh          # hubd + Mosquitto as systemd services
```

**ESP32** (needs ESP-IDF v5.5+ and a board):
```sh
cd esp32
cp main/wifi_creds.example.h main/wifi_creds.h    # then set your venue Wi-Fi
idf.py -p /dev/YOURPORT flash monitor
```
Then join the hub's Wi-Fi and open `http://hub.local/` (or the printed IP).

## The other repos

`robot` (rover firmware), `hub-zenoh` (a Zenoh evaluation baseline, receding),
and `workbench` (a browser dev environment) live separately — they're different
projects, not implementations of this hub.
