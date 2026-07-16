# Robotics Hub — Raspberry Pi

*One routed topic-space for the classroom: student devices, instructor tools,
and rovers over MQTT — the hub's own Wi-Fi is the permission model, not a
login.*

```sh
mosquitto -c mosquitto.example.conf   # the broker (passwd file: examples/classroom-mosquitto-demo.sh)
cargo run --bin hubd                  # dashboard on http://localhost:8000
```

```
      student devices ×N                  hub appliance                 rovers ×M
 ┌──────────────────────────┐    ┌─────────────────────────────┐    ┌────────────────┐
 │ browser                  │    │ hubd (Rust)                 │    │ ESP32 firmware │
 │ dashboard.html + mqtt.js │◄──►│ HTTP chassis: page, /fleet, │    │ (esp-mqtt)     │
 └──────────────────────────┘    │ Wi-Fi — not an MQTT client  │    └────────────────┘
                                 └─────────────────────────────┘
               │                                                             │
      MQTT-over-WS :9001                                              raw MQTT :1883
    (direct — no relay)                                            (direct — no relay)
               └──────────────────────────▼──────────────────────────────────┘
                     ┌─────────────────────────────────────────┐
                     │ Mosquitto — the broker (own process)    │
                     │ open rw on robots/# and pair/# ·        │
                     │ usrpwd auth gates only instructor,       │
                     │ only for fleet/estop                    │
                     │ robots/<id>/{sys,pwm,cmd/*,imu,led}     │
                     └─────────────────────────────────────────┘
```

**`hubd` is not an MQTT client.** It only serves the dashboard page (which
then opens its own MQTT-over-WS connection, bypassing hubd entirely), the
uplink/captive-portal probe, and **device-served Wi-Fi setup** —
`GET /wifi/scan`, `GET /wifi/status`, `POST /wifi/connect` (nmcli glue in
`src/wifi.rs`). A phone joins the hub's own `hub-XXXX` AP, opens
`http://hub.local`, and picks the uplink network from the dashboard's "Set up
Wi-Fi" panel — no app, no hosted site, no Web Bluetooth, works on iOS.
Classroom access control — open by default, gated only for the instructor's
`fleet/estop` write — is enforced by Mosquitto's own ACL, not application
code — see `mosquitto-acl.example.conf`.

**The dashboard also works with no hub server at all.** `mqtt.js` is inlined
directly into the top-level [`dashboard.html`](../dashboard.html) (canonical
there; hubd embeds it at build time), so it's a genuine standalone artifact:
download it, open it as `file://`, type in a hub address, and it connects
straight to Mosquitto. That only works as a downloaded/local file, though —
hosted live over `https:` (e.g. GitHub Pages), a browser blocks the plain
`ws://` connection as mixed content; serving the page from the hub's own
plain-HTTP origin (or opening it as `file://`) is what makes the direct
connection possible at all.

## Layout

```
pi/
├── src/
│   ├── lib.rs              typed envelopes + topic helpers
│   ├── wifi.rs             device-served Wi-Fi setup — nmcli glue for /wifi/*
│   └── bin/
│       └── hubd.rs          dashboard/HTTP chassis + Wi-Fi setup — no MQTT client
├── mosquitto.example.conf       broker config: raw MQTT (1883) + WebSocket (9001)
├── mosquitto-acl.example.conf   classroom scoping, enforced by the broker
├── examples/                    classroom-mosquitto-demo.sh proves the ACL live
├── deploy/                  systemd units + install — the appliance's planes
├── image/                   CI-built Pi image: hubd + Mosquitto baked into the rootfs
└── tools/                   deploy + Pi-serial helpers
```

The wire contract — topic scheme and message shapes — is canonical at the
monorepo top level: [`CONTRACT.md`](../CONTRACT.md) + [`envelopes/`](../envelopes).

State, roadmap: **[hub#1](../../issues/1)**. The rover firmware the hub
serves is [`better-robotics/robot`](https://github.com/better-robotics/robot);
an ESP32 board can also *be* the hub (a boot role of that same firmware) when
no Pi is present.
