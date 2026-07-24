# Robotics Hub — Raspberry Pi

*One routed key-space for the classroom: student devices, operator tools,
and robots over Zenoh — the hub's own Wi-Fi is the permission model, not a
login.*

```sh
zenohd -c zenoh-router.example.json5                                     # the Zenoh router (:7447)
ZENOH_CONNECT=tcp/127.0.0.1:7447 python3 ws-adapter/ws_zenoh_adapter.py  # the browser edge (:9001)
cargo run --bin hubd                                                     # dashboard on http://localhost:8000
```

```
      student devices ×N                  hub appliance                 robots ×M
 ┌──────────────────────────┐    ┌─────────────────────────────┐    ┌────────────────┐
 │ browser                  │    │ hubd (Rust)                 │    │ ESP32 firmware │
 │ dashboard.html (WS-JSON) │◄──►│ HTTP chassis: page, /fleet, │    │ (zenoh-pico)   │
 └──────────────────────────┘    │ Wi-Fi — no transport client │    └────────────────┘
                                 └─────────────────────────────┘
               │                                                             │
      WS-JSON :9001                                                  Zenoh tcp/:7447
    (direct — no relay)                                            (direct — no relay)
               └──────────────────────────▼──────────────────────────────────┘
                     ┌─────────────────────────────────────────┐
                     │ zenohd — the Zenoh router (:7447)       │
                     │ + ws-adapter — the browser edge (:9001) │
                     │ open floor: robots/** + pair/** rw      │
                     │ operator auth gates only fleet/estop    │
                     │ router ACL → adapter is the sole path   │
                     │ robots/<id>/{sys,pwm,cmd/*,imu,led}     │
                     └─────────────────────────────────────────┘
```

**`hubd` is a client of no transport.** It only serves the dashboard page (which
then opens its own WS-JSON connection to the ws-adapter, bypassing hubd
entirely), the uplink/captive-portal probe, and **device-served Wi-Fi setup** —
`GET /wifi/scan`, `GET /wifi/status`, `POST /wifi/connect` (nmcli glue in
`src/wifi.rs`). A phone joins the hub's own `hub-XXXX` AP, opens
`http://hub.local`, and picks the uplink network from the dashboard's "Set up
Wi-Fi" panel — no app, no hosted site, no Web Bluetooth, works on iOS.
Classroom access control — open by default, gated only for the operator's
`fleet/estop` write — is enforced by the ws-adapter (app-layer operator auth +
per-owner claiming) and the `zenohd` router ACL, not by hubd — see
`ws-adapter/README.md` and `zenoh-router.example.json5`.

**The dashboard also works with no hub server at all.** The top-level
[`dashboard.html`](../dashboard.html) (canonical there; hubd embeds it at build
time) is a genuine standalone artifact: download it, open it as `file://`, type
in a hub address, and it connects straight to that hub's ws-adapter over WS-JSON.
That only works as a downloaded/local file, though — hosted live over `https:`
(e.g. GitHub Pages), a browser blocks the plain `ws://` connection as mixed
content; serving the page from the hub's own plain-HTTP origin (or opening it as
`file://`) is what makes the direct connection possible at all.

## Layout

```
pi/
├── src/
│   ├── lib.rs              typed envelopes + key helpers
│   ├── wifi.rs             device-served Wi-Fi setup — nmcli glue for /wifi/*
│   └── bin/
│       └── hubd.rs          dashboard/HTTP chassis + Wi-Fi setup — client of no transport
├── zenoh-router.example.json5   zenohd config: listen :7447 + the router ACL
├── ws-adapter/                  the browser edge (WS-JSON :9001 ↔ zenohd) + per-owner claiming
├── deploy/                  systemd units + install — the appliance's planes
├── image/                   CI-built Pi image: hubd + zenohd + ws-adapter baked into the rootfs
└── tools/                   deploy + Pi-serial helpers
```

The wire contract — topic scheme and message shapes — is canonical at the
monorepo top level: [`CONTRACT.md`](../CONTRACT.md) + [`envelopes/`](../envelopes).

State, roadmap: **[hub#1](../../issues/1)**. The robot firmware the hub
serves is [`sprocket-robotics/robot`](https://github.com/sprocket-robotics/robot);
an ESP32 board can also *be* the hub (a boot role of that same firmware) when
no Pi is present.
