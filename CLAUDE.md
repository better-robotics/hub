# hub — monorepo context

The classroom Robotics Hub: one shared wire contract, two implementations.
Formed 2026-07-08 by collapsing the per-transport `hub-*` repos — the MQTT
transport won the bake-off, so the meaningful axis is now **host** (Pi vs
MCU), not transport, and the shared contract no longer belongs inside any one
implementation.

## Structure — why a monorepo, not a submodule
- **Top level = the shared contract**: `CONTRACT.md` (topic scheme), `envelopes/`
  (message shapes), `dashboard.html` (browser client), `mcp-bridge/` (LLM client).
- **`pi/`** — Raspberry Pi implementation (Rust `hubd` + Mosquitto + Pi image).
  Was `better-robotics/hub-mqtt`.
- **`esp32/`** — the whole hub on one ESP32 (ESP-IDF firmware). Was
  `better-robotics/hub-esp32`.

Both **build-embed the same top-level `dashboard.html`** (`pi/` via
`include_str!("../../../dashboard.html")`, `esp32/` via
`EMBED_TXTFILES "../../dashboard.html"`) and speak the same `envelopes/`. One
repo, not several, so a breaking contract change lands in the contract *and*
both consumers in **one atomic commit** — submodules would let the two hubs
silently pin different protocol SHAs (the exact drift this structure prevents).

## Where impl-specific context lives
- `pi/CLAUDE.md` — the deep Pi context (broker deploy, AP/NAT, BLE scars, ACL).
  Contract refs point at the monorepo top level (`../CONTRACT.md`,
  `../envelopes/`); remaining `protocol/` mentions there are hub-zenoh's own.
- `esp32/README.md` — the ESP32 firmware (AP+STA+NAT + on-chip broker + WS bridge).

## Building
- Pi: `cd pi && cargo build` (or `sudo ./deploy/install.sh`).
- ESP32: `cd esp32 && idf.py build` (needs ESP-IDF v5.5+; copy
  `main/wifi_creds.example.h` → `main/wifi_creds.h`, gitignored).
- Both build-verified in this layout 2026-07-08.

## Not in this repo (deliberately)
`hub-zenoh` (Zenoh evaluation baseline — **archived 2026-07-09**, MQTT won the
bake-off; kept read-only as the baseline record), `robot` (rover firmware),
`workbench` (browser dev env). Different projects, not implementations of this hub.

## CI
`.github/workflows/` are rehomed for the monorepo (`working-directory: pi`,
`pi/`-prefixed artifact paths). `broker-tests` (on push) and `build-hubd` (via
dispatch) both **verified green** 2026-07-09 — the rehome holds. The other
`build-*`/`release-*` workflows are `workflow_dispatch` (or tag-gated for
`build-image`); trigger on demand with `gh workflow run <name>.yml -R better-robotics/hub`.
