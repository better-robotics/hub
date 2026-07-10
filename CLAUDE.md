# hub — monorepo context

The classroom Robotics Hub: the shared wire contract + the Raspberry Pi hub.
Formed 2026-07-08 by collapsing the per-transport `hub-*` repos — the MQTT
transport won the bake-off, so the shared contract no longer belongs inside any
one implementation.

## Structure
- **Top level = the shared contract**: `CONTRACT.md` (topic scheme), `envelopes/`
  (message shapes), `dashboard.html` (browser client), `mcp-bridge/` (LLM client).
- **`pi/`** — Raspberry Pi implementation (Rust `hubd` + Mosquitto + Pi image).
  Was `better-robotics/hub-mqtt`.

The Pi build-embeds the top-level `dashboard.html`
(`include_str!("../../../dashboard.html")`) and speaks the `envelopes/` contract.

**The second hub — the whole hub on one ESP32 — moved out 2026-07-09.** It is now
a *boot role* of the unified firmware in `better-robotics/robot` (a rover that
finds no `hub-*` becomes one), not a separate implementation. That repo **vendors**
`dashboard.html` from here (canonical stays in this monorepo) with a drift check
(`robot/tools/sync-dashboard.sh --check`) — the tradeoff for one-image firmware:
the ESP hub no longer rides the same atomic commit as a contract change, so the
drift check is what keeps its embedded dashboard from silently pinning an old
copy. A breaking contract change now means: land it here, then resync in `robot`.

## Where impl-specific context lives
- `pi/CLAUDE.md` — the deep Pi context (broker deploy, AP/NAT, device-served
  Wi-Fi setup, ACL).
  Contract refs point at the monorepo top level (`../CONTRACT.md`,
  `../envelopes/`); remaining `protocol/` mentions there are hub-zenoh's own.
- The ESP32 hub role's context lives in `better-robotics/robot` (`CLAUDE.md`).

## Building
- Pi: `cd pi && cargo build` (or `sudo ./deploy/install.sh`). Build-verified 2026-07-08.
- The ESP32 hub role builds from `better-robotics/robot` (`pio run`), not here.

## Not in this repo (deliberately)
`hub-zenoh` (Zenoh evaluation baseline — **archived 2026-07-09**, MQTT won the
bake-off; kept read-only as the baseline record), `robot` (the rover + ESP32-hub-role
firmware), `workbench` (browser dev env). Different projects.

## CI
`.github/workflows/` are rehomed for the monorepo (`working-directory: pi`,
`pi/`-prefixed artifact paths). `broker-tests` (on push) and `build-hubd` (via
dispatch) both **verified green** 2026-07-09 — the rehome holds. The other
`build-*`/`release-*` workflows are `workflow_dispatch` (or tag-gated for
`build-image`); trigger on demand with `gh workflow run <name>.yml -R better-robotics/hub`.

**`build-image` builds no base OS since 2026-07-10** (verified green same day,
~4.5 min end-to-end): it downloads the pinned official Raspberry Pi OS Lite
release and loop-mount-customizes it (`pi/image/customize-image.sh`) — pi-gen
is gone. `build-hubd` is a reusable workflow (`workflow_call`, static musl)
called as `build-image`'s first job, so the fast-redeploy artifact and the
baked binary are always identical. Bumping the base image (new date, or
Bookworm→Trixie) is a deliberate edit to the three `BASE_*` values in
`build-image.yml`, never a rebuild side effect.
