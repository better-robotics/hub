# hub-mqtt

*One routed topic-space for the classroom: student devices, professor tools,
and rovers over MQTT, under a directional per-topic permission model.*

> MQTT-transport variant of the classroom Robotics Hub — the preferred
> implementation, measured against
> [`better-robotics/hub-zenoh`](https://github.com/better-robotics/hub-zenoh)
> as an evaluation baseline
> ([hub-zenoh#4](https://github.com/better-robotics/hub-zenoh/issues/4)).

```sh
mosquitto -c mosquitto.example.conf   # the broker (passwd file: examples/classroom-mosquitto-demo.sh)
cargo run --bin hubd                  # dashboard on http://localhost:8000
```

```
      student devices ×N                  hub appliance                 rovers ×M
 ┌──────────────────────────┐    ┌─────────────────────────────┐    ┌────────────────┐
 │ browser                  │    │ hubd (Rust)                 │    │ ESP32 firmware │
 │ dashboard.html + mqtt.js │◄──►│ HTTP chassis: page, /fleet, │    │ (esp-mqtt)     │
 └──────────────────────────┘    │ BLE — not an MQTT client    │    └────────────────┘
                                 └─────────────────────────────┘
               │                                                             │
      MQTT-over-WS :9001                                              raw MQTT :1883
    (direct — no relay)                                            (direct — no relay)
               └──────────────────────────▼──────────────────────────────────┘
                     ┌─────────────────────────────────────────┐
                     │ Mosquitto — the broker (own process)    │
                     │ usrpwd auth · directional per-topic ACL │
                     │ robots/<id>/{sys,imu,pwm,led,led/reply} │
                     └─────────────────────────────────────────┘
```

**`hubd` is not an MQTT client** — the inversion from hub-zenoh, where hubd
*is* the router in the data path. Here it only serves the dashboard page
(which then opens its own MQTT-over-WS connection, bypassing hubd entirely),
the uplink/captive-portal probe, and (in `provisiond`) BLE day-zero Wi-Fi
provisioning. Classroom access control (professor vs. team scoping) is
enforced by Mosquitto's own ACL, not application code — see
`mosquitto-acl.example.conf`.

**The dashboard also works with no hub server at all.** `mqtt.js` is inlined
directly into `public/dashboard.html` (no separate file, no CDN), so it's a
genuine standalone artifact: download it, open it as `file://`, type in a
hub address, and it connects straight to Mosquitto. That only works as a
downloaded/local file, though — hosted live over `https:` (e.g. GitHub
Pages), a browser blocks the plain `ws://` connection as mixed content;
serving the page from the hub's own plain-HTTP origin (or opening it as
`file://`) is what makes the direct connection possible at all.

**Still open — see [hub#1](../../issues/1):** the RPC binding for `set_led`
(topic scheme is settled, the MQTT5 properties aren't wired into a client
yet), loopback sim clients, and the ESP32 rover firmware itself
(`esp-mqtt`, not yet added to `better-robotics/robot`).

## Layout

```
hub-mqtt/
├── src/
│   ├── lib.rs              typed envelopes + topic helpers (transport-agnostic)
│   └── bin/
│       ├── hubd.rs          dashboard/HTTP chassis — no MQTT client
│       └── provisiond.rs    BLE Wi-Fi provisioning (Improv)
├── protocol/                the envelope contract, canonical here (see protocol/README.md)
├── public/
│   └── dashboard.html       mqtt.js inlined — direct client, also standalone (file://)
├── mosquitto.example.conf       broker config: raw MQTT (1883) + WebSocket (9001)
├── mosquitto-acl.example.conf   classroom scoping, enforced by the broker
├── classroom.example.json5      the scoping intent these files implement
├── examples/                    classroom-mosquitto-demo.sh proves the ACL live
├── deploy/                  systemd units + install — the appliance's planes
└── image/                   CI-built Pi image: hubd + provisiond baked into the rootfs
```

State, roadmap: **[hub#1](../../issues/1)**. The rover firmware the hub
serves is [`better-robotics/robot`](https://github.com/better-robotics/robot).
