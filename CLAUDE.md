# hub — monorepo context

The classroom Robotics Hub: the shared wire contract + the Raspberry Pi hub.
Formed 2026-07-08 by collapsing the per-transport `hub-*` repos — the MQTT
transport won the bake-off, so the shared contract no longer belongs inside any
one implementation.

## Structure
- **Top level = the shared contract**: `CONTRACT.md` (topic scheme), `envelopes/`
  (message shapes), `dashboard.html` (browser client), `mcp-bridge/` (LLM client).
- **`pi/`** — Raspberry Pi implementation (Rust `hubd` + Mosquitto + Pi image).
  Was `better-robotics/hub-mqtt`. hubd also serves the workbench IDE bundle
  (workbench `docs/` tree) at `/ide/` from `HUB_IDE_DIR` (default
  `/usr/share/hub/ide`; installed by deploy/install.sh + baked by build-image).

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

## Dashboard UI system
`dashboard.html` is three layers, strictest first: **web platform floor**
(WCAG/ARIA — focus rings, live regions, no color-alone encoding) → **Apple HIG
house layer** (44pt effective tap targets — chip-scale controls carry an
invisible `::after` hit extension; safe areas; reduced motion; the corner
popover adapts toward a sheet on compact widths) → **the file's own
vocabulary**: tokens at the top of `<style>` (radius scale, `--tap`, ink ramp,
ONE warn hue = "act here" — an unassigned board carries it (corner chip +
Assign button); amber recedes to plain ink as boards earn teams), and composable
patterns — `.gate-row` (input+button, stacks on phones), `.btn-tile` /
primary-by-id / `.link-btn` button tiers, `.cchip` corner chips (the card
corner is the rover's topbar), the modal sheet, `#chip-pop` corner popover,
the 0.85rem panel beat.

**Compose, don't hand-roll.** Every shipped spacing/alignment defect so far
was a new element skipping an existing pattern. Before shipping: run the
layout regression sweeps in the `verify` skill (touching-pairs + horizontal
overflow, at 320/390/768/1200, staged with hostile-length data).

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
