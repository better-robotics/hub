# hub — monorepo context

The classroom Robotics Hub: the shared wire contract + the Raspberry Pi hub.
Formed 2026-07-08 by collapsing the per-transport `hub-*` repos — the MQTT
transport won the bake-off, so the shared contract no longer belongs inside any
one implementation.

## Structure
- **Top level = the shared contract**: `CONTRACT.md` (topic scheme), `envelopes/`
  (message shapes), `dashboard.html` (browser client), `mcp-bridge/` (LLM client).
- **`pi/`** — Raspberry Pi implementation (Rust `hubd` + Mosquitto + Pi image).
  Was `better-robotics/hub-mqtt`. hubd also serves the
  [`better-robotics/ide`](https://github.com/better-robotics/ide) bundle
  (its built dist — source + vendored Blockly/Monaco/mqtt.js/MicroPython-WASM,
  fetched as a release asset since `ide`'s `vendor/` is gitignored) at `/ide/`
  from `HUB_IDE_DIR` (default `/usr/share/hub/ide`; installed by
  deploy/install.sh + baked by build-image). It is **blocks-first Python**, not
  a code editor: students land in a Blockly workspace and the MicroPython it
  generates renders live beneath it (Monaco is that read-only preview, not the
  authoring surface), running as WASM in the tab. `ide` is a browser-only
  client of this monorepo's own MQTT/WS contract — no firmware or hubd changes
  needed when it updates.

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
house layer** (44pt tap targets *under `pointer:coarse` only* — a mouse never
needed them, and forcing 44px into the base rule is what made the corner
popover tower over its own chips; chip-scale controls stay small and reach
44pt via an invisible `::after` hit extension; safe areas; reduced motion; the
corner popover adapts toward a sheet on compact widths) → **the file's own
vocabulary**, which is *encoded, not described* (2026-07-16):

- **Tokens are the only source of sizes.** Radius scale (`--radius-inner`
  exists so a nested corner can't out-round its container), `--ctrl-h` (the
  one control height), the `--fs-*` type scale, ink ramp. A literal px/rem in
  a control is a bug — the audit greps for it. **Spacing is the exception, and
  knowingly so**: 29 hand-picked values, no `--space-*` scale. Retrofitting one
  measured out at 104/173 declarations moving up to 3.2px — a visual redesign,
  not a mechanical fix, so it wants its own pass with eyes on every breakpoint
  rather than a blind sweep. Don't half-do it: a scale nothing uses is worse
  than none. (`.stack`/`.list-group` are the real answer to most of it —
  containers own spacing; the primitives are Every Layout's Stack and Sidebar,
  which this file arrived at independently.)
- **A token comment that states a contrast ratio is a claim, not a
  measurement.** `--ink-faint` said `≥4.5:1` and shipped at 4.24; the tree's
  arrows sat at 3.45/3.85 under a comment claiming they were the log's; and
  `.notice.danger`'s "Emergency stop engaged" was 4.11. Dark-committed UI hides
  this — everything looks high-contrast on near-black. `--danger-text` is
  lifted off Apple's `#ff453a` for exactly this reason (conformance beats the
  vendor value when they collide), and `--border-input` exists because an empty
  field's fill is 1.18:1 on a card, so its border is the only thing making it
  perceivable (SC 1.4.11). Measure with the `verify` skill's contrast audit —
  it is the only rung that catches this, and a shared token's fix must be
  re-checked at every dependent pairing.
- **The base `<button>` IS the neutral tile**, and tiers are classes on top
  (`.btn-primary` / `.btn-accent` / `.btn-danger` / `.link-btn`). So a
  classless button is in-system by construction. It was previously
  primary-by-id-list, which no new button could join — and `#estop-clear`,
  joining nothing, shipped as a raw macOS button.
- **Containers own spacing, not callsites**: `.stack` (vertical rhythm — the
  JS-composed popovers each used to wrap contents in a bare div, which eats
  the parent's `gap` because `* { margin: 0 }` means nothing else supplies
  it), `#modal-body > *` (padding; full-bleed opts out by name),
  `.list-group` (the iOS grouped-inset list — ONE filled panel with hairline
  separators, which the telemetry `dl` always was and the Wi-Fi picker
  reimplemented as a wall of pills).
- **ONE warn hue = "act here"**, carried by the identity chip alone: the chip
  IS the Assign affordance in every state (tap who-it-is to change it). Amber
  recedes to plain ink once a board is named. Amber is never the *only*
  channel — `.tchip.warn` carries a `!` mark, because "online" vs "needs venue
  sign-in" on the network chip differed by hue alone and read as fine all class.
- **`announce()` / `alertNow()` / `sheetStatus()` are the announcement
  vocabulary** — the AT counterpart to `.notice`, and the same shape as the
  button tier above: status used to be announced-by-whichever-sheet-is-open,
  which no new status could join (four live regions existed; three sat inside
  `hidden` storage). `#app-status` and `#app-alert` are `.sr-only`, **always
  rendered, empty at rest** — that is the whole contract, and it's why they
  can't live on the visual banners, which are display-toggled by design. A
  region that isn't in the tree when its text arrives never fires; populate-
  then-reveal is a coin flip across AT. Never write them directly, and never
  put `role=alert` on a `display:none` banner (`#estop-banner` had one, with its
  text pre-filled, containing a focusable button). Writes are change-guarded
  because the 2 s clock re-renders unconditionally — verify with a
  MutationObserver, not by eye.
- `.gate-row`, `.cchip` corner chips (the card corner is the rover's topbar),
  the modal sheet, `#chip-pop` corner popover, the `--fs-body` panel beat.

**Compose, don't hand-roll.** Every shipped spacing/alignment defect so far
was a new element skipping an existing pattern — a review on 2026-07-16 found
seven at once, all that same species, which is why the vocabulary moved from
this paragraph into the CSS itself. Prose can't stop a classless button.
Before shipping: run the `verify` skill's **control-vocabulary audit** (no
UA-default controls, token-only sizes, `[hidden]` still hides) and the layout
regression sweeps (touching-pairs + horizontal overflow, at 320/390/768/1200,
staged with hostile-length data, popovers *open* — the sweep was pointed away
from `#chip-pop` for months, which is exactly how they all got `gap: 0`).

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
firmware), `workbench` (browser dev env, drifting from the classroom model),
`ide` (the blocks-and-Python editor `pi/` fetches and serves at `/ide/` — a
client of this repo's contract, not an implementation of it). Different projects.

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
