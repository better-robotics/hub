# hub — the classroom Robotics Hub

One wire contract, two ways to host it. Students join the hub's Wi-Fi, open a
dashboard, and drive ESP32 rovers over MQTT — from a Raspberry Pi appliance
(this repo), or from a single ESP32 that *is* the whole hub (a boot role of the
rover firmware in [`better-robotics/robot`](https://github.com/better-robotics/robot)).

```
             shared contract (top level of this repo)
   envelopes/  ·  dashboard.html  ·  mcp-bridge/  ·  CONTRACT.md
        │                                    │
        ▼                                    ▼
   ┌──────────┐                        ┌───────────────┐
   │   pi/    │  Raspberry Pi          │  ESP32 hub    │  one ESP32 =
   │          │  Rust hubd (HTTP) +    │  role (in the │  AP + NAT + broker +
   │          │  Mosquitto broker      │  robot repo)  │  WS bridge + dashboard
   └──────────┘                        └───────────────┘
   production target                   small-classroom, no Pi
```

Both hosts serve the **same** `dashboard.html` and speak the **same** `envelopes/`
— a single source of truth at the top of this repo. The Pi embeds it directly;
the ESP32 hub role **vendors** a drift-checked copy into the `robot` firmware. A
breaking contract change lands here, then resyncs there
(`robot/tools/sync-dashboard.sh`).

## Layout

```
hub/
├── CONTRACT.md         the wire contract — topic scheme, envelope table, directions
├── envelopes/          message shapes (imu, pwm, rpc_set_led) — language bindings live in each impl
├── dashboard.html      the browser client (mqtt.js + favicon inlined; also runs standalone from file://)
├── mcp-bridge/         MCP tool server — drive the fleet from an LLM over the same contract
└── pi/                 Raspberry Pi implementation — Rust hubd + Mosquitto + Pi image (was hub-mqtt)
    ├── src/            hubd (dashboard/HTTP chassis) + provisiond (BLE)
    ├── mosquitto*.conf broker config + ACL (classroom scoping)
    ├── deploy/ image/  systemd install + CI-baked Pi image
    └── examples/       broker ACL + WebSocket transport tests (CI-gated)
```

The **ESP32 hub** is not a directory here — it's the hub *role* of the unified
rover firmware in [`better-robotics/robot`](https://github.com/better-robotics/robot)
(one image; a rover that finds no `hub-*` can become one).

## Run

**Pi** (needs a Linux/Pi host):
```sh
cd pi && sudo ./deploy/install.sh          # hubd + Mosquitto as systemd services
```

**ESP32 hub** — flash the `robot` firmware and force the hub role; see that
repo's README. Then join the hub's Wi-Fi and open `http://hub.local/` (or the
printed IP).

## The other repos

`robot` (the rover firmware — and now the ESP32 hub role), `hub-zenoh` (a Zenoh
evaluation baseline, receding), and `workbench` (a browser dev environment) live
separately.
