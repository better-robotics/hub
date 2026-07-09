# hub-esp32

*The whole classroom hub on a single ESP32 — Wi-Fi AP, internet-sharing router,
MQTT broker, and the dashboard, one chip, no Raspberry Pi.*

> Experimental hardware variant of the classroom Robotics Hub. The production
> target is [`better-robotics/hub-mqtt`](https://github.com/better-robotics/hub-mqtt)
> (a Raspberry Pi running Mosquitto); this repo asks whether a bare ESP32 can
> stand in for it at small-classroom scale. **Feasibility is proven** (see the
> validation trail in hub-mqtt#2); what's here is a working prototype, not a
> hardened appliance.

On one plain ESP32, simultaneously:

```
        student phone/laptop                    ESP32 (this firmware)                 venue Wi-Fi
   ┌────────────────────────────┐        ┌──────────────────────────────────┐      ┌────────────┐
   │ browser                    │  AP    │ SoftAP  brobo-hub-test             │ STA  │ venue Wi-Fi│
   │ http://192.168.4.1/        │◄──────►│ NAPT ───────────────────────────────────►│ (internet) │
   │ dashboard.html + mqtt.js   │        │ :80  dashboard   :9001  WS bridge  │      └────────────┘
   └────────────────────────────┘        │ :1883 Mosquitto broker (on-chip)   │
                 │ ws://…:9001                └──────────────────────────────────┘
                 └── MQTT-over-WS ──► WS↔TCP bridge ──► 127.0.0.1:1883 broker
```

- **AP + STA + NAPT** — students join the ESP32's AP; its STA leg uplinks to the
  venue Wi-Fi; NAT forwards their traffic out, so joining the hub keeps internet.
- **On-chip MQTT broker** — Espressif's Mosquitto port (`espressif/mosquitto`),
  with a connect-callback enforcing per-team credentials.
- **WS↔TCP bridge** — the Mosquitto port has no WebSocket listener, so a
  ~180-line bridge (`ws_mqtt_bridge.c`) lets browsers reach the broker over
  MQTT-over-WebSocket. This is the piece that unblocked the browser dashboard.
- **Self-served dashboard** — the real `dashboard.html` (shared with hub-mqtt)
  is embedded and served on :80, so the ESP32 needs no separate web host.

Port layout mirrors the Pi (page :80, broker-WS :9001) so the identical
`dashboard.html` runs on both.

## Build & flash

Needs ESP-IDF v5.5+. Target: `esp32` (original; 4 MB flash).

```sh
cp main/wifi_creds.example.h main/wifi_creds.h   # then edit: your venue Wi-Fi
idf.py set-target esp32
idf.py -p /dev/cu.YOURPORT flash monitor
```

Then join Wi-Fi **`brobo-hub-test`** / **`brobotics`** and open
**`http://192.168.4.1/`**. Fleet view is public; drive with `professor` /
`change-me` (or a per-team login — see `connect_cb` in `hub_broker_main.c`).

## Known limits (why this is a prototype, not the appliance)

- **~8 Wi-Fi clients** — single-radio SoftAP ceiling; small classrooms only.
- **Single radio → same channel** — the AP follows the venue Wi-Fi's channel.
- **Broker has no per-topic ACL** — session-level auth only. The intended
  multi-team model runs a broker *per team* (isolation by instance, not topic
  rules); this single-broker build allows anonymous read for the demo fleet view.
- **Bridge lifetime is validation-grade** — fixed slot pool, edge-case cleanup
  can leak a slot until keepalive traffic reclaims it. Fine for a demo, needs
  hardening for an unattended semester.
- **All C on a chip with no memory protection** — unlike the Pi's Rust-on-Linux,
  a broker/bridge fault isn't isolated.

## Layout

```
main/
├── hub_broker_main.c   AP+STA+NAPT, broker, per-team connect-auth
├── ws_mqtt_bridge.c    WS↔TCP bridge + :80 dashboard/​/fleet handlers
├── dashboard.html      embedded, served on :80 (copy of hub-mqtt's)
├── wifi_creds.example.h → copy to wifi_creds.h (gitignored)
└── CMakeLists.txt
sdkconfig.defaults      NAPT, WS support, 4MB large-app partition
```
