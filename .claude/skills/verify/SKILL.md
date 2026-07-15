---
name: verify
description: Verify dashboard.html changes by driving the page in a browser. Use after editing dashboard.html (or hubd routes it calls) to observe the change at the real surface.
user-invocable: false
---

# Verifying dashboard.html changes

The dashboard is a single self-contained file — no build step. Serve it
statically and drive it with a browser (Playwright MCP works):

```sh
cd hub && python3 -m http.server 8123 --bind 127.0.0.1
# → http://127.0.0.1:8123/dashboard.html
```

Expected environment noise offline: 404s for /fleet, /wifi/status
and ws://…:9001 connection refusals (no hubd/broker behind a static server).
Anything else in the console is real. (The Pi's `/codes` HTTP API is gone —
confirmed 2026-07-13, the hub's own Wi-Fi is the security boundary now, not a
login — so there's no `/codes/*` fetch to see 404 anymore.)

## Handles

- App functions (`openConfig`, `openCamera`, …) are globals — callable from
  `browser_evaluate` when no live MQTT fleet exists to click through. Clicks
  on the resulting DOM are still real gestures.
- Professor-gated sections (the Assign sheet, e-stop Stop-all/Clear) are
  `display:none` offline until a professor session is simulated — walk
  ancestors and unhide before clicking. Everything else (drive, claim,
  telemetry, camera) needs no sign-in at all now — verify it works from a
  cold, anonymous load.
- Fake board endpoint: a tiny HTTP server answering 200 on any path. It MUST
  send `Content-Type: text/html` — `python3 -m http.server` serves an
  extensionless `wifi` file as octet-stream, the iframe becomes a *download*,
  `load` never fires, and reachability tests silently lie.
- Unreachable board: use `127.0.0.1:9` (closed port, instant refusal) instead
  of a blackhole IP — no timeout wait.
- Insecure-context clipboard (hub.local behavior) on localhost: stub
  `Object.defineProperty(navigator,'clipboard',{value:undefined,configurable:true})`.

## Control vocabulary audit

**Run this first — it's cheap, needs no staging, and catches the whole class
of "element skipped the design system".** A 2026-07-16 review found seven
defects in one pass, every one an element that hand-rolled instead of
composing; all of them were mechanically detectable and nothing was looking.
The system is encoded in the CSS now (base `button` = the neutral tile, tiers
as classes, `.stack`, `.list-group`, token-only sizes) — this is what keeps it
that way.

- **No UA-default controls.** The failure mode is invisible in code review: a
  `<button>` with no class renders macOS-grey. `#estop-clear` shipped that way
  and sat in the e-stop banner.

  The reference button MUST come from an iframe — i.e. a document with no
  author styles. Probing with a button in *this* page is what a first pass did,
  and it silently inverted the test: the base rule now styles bare buttons, so
  the probe was in-system, and every correct control "matched the UA default"
  (31 false positives, 0 real). The iframe reads the true UA value.

  ```js
  const f = document.createElement("iframe"); f.style.display = "none"; document.body.appendChild(f);
  const fb = f.contentDocument.createElement("button"); f.contentDocument.body.appendChild(fb);
  const uaBg = f.contentWindow.getComputedStyle(fb).backgroundColor; f.remove();
  [...document.querySelectorAll("button, input, select")]
    .filter(el => getComputedStyle(el).backgroundColor === uaBg)
    .map(el => el.id || el.className || el.outerHTML.slice(0, 60))   // must be []
  ```

- **Sizes come from the token scale.** Reveal every popover/sheet first
  (`#chip-pop`, `.modal-card`, `#codes`, `#drive-store`) or they dodge it.

  ```js
  const OK_FONT = ["11.52px","12.48px","13.6px","16.8px","17.6px"]; // --fs-micro/small/body/title/glyph
  const OK_RADIUS = ["8px","6px","14px","999px","50%","0px"]; // --radius{,-inner,-lg,-pill}/circle/list-row
  [...document.querySelectorAll("button, input, select")].filter(el => {
    const c = getComputedStyle(el);
    return !OK_FONT.includes(c.fontSize) || !OK_RADIUS.includes(c.borderTopLeftRadius);
  }).map(el => [el.id || el.className, getComputedStyle(el).fontSize,
                getComputedStyle(el).borderTopLeftRadius])   // must be []
  ```

  A hit means a magic number crept back (`.segmented`'s thumb was `10px`
  inside its own `8px` track — an inner corner rounder than the container
  clipping it).

- **`hidden` still hides.** `[hidden]` is a UA rule, so any author `display`
  outranks it — adding `.stack` to `#id-menu` silently reopened it.

  ```js
  [...document.querySelectorAll("[hidden]")]
    .filter(el => el.getBoundingClientRect().height > 0)   // must be []
  ```

## Layout regression sweeps

Run in `browser_evaluate` at 320 / 390 / 768 / 1200 — and stage *hostile*
data first (long venue SSID in `#net-chip-label`, `#host-chip` populated,
long robot names): every real-device layout bug so far shipped because the
staged data was too polite.

- **Touching pairs** (spacing floor — anything under ~5px between stacked
  siblings is a defect unless it's the card title→meta pair). **Open the
  popovers/sheets before running it**: the selector list below missed
  `#chip-pop` entirely until 2026-07-16, which is exactly how every
  JS-composed panel came to have a bare wrapper `div` with `gap: 0` between
  its title and its input — the sweep was never pointed at them. Gap belongs
  to `.stack`; a callsite that makes its own wrapper is the bug.

  ```js
  const bad = [];
  for (const p of document.querySelectorAll("main, main section, main div, form, details, .robot, #chip-pop, #chip-pop div, .modal-card, .modal-card div, .stack"))
    { const kids = [...p.children].filter(el => { const r = el.getBoundingClientRect(), cs = getComputedStyle(el);
        return r.height > 8 && r.width > 0 && cs.display !== "none" && cs.position !== "absolute"; });
      for (let i = 0; i < kids.length - 1; i++) { const g = kids[i+1].getBoundingClientRect().top - kids[i].getBoundingClientRect().bottom;
        if (g >= -1 && g < 5 && kids[i].tagName !== "H2" && kids[i].tagName !== "SUMMARY") bad.push([kids[i], kids[i+1], g]); } }
  bad
  ```

- **Horizontal overflow**: `document.documentElement.scrollWidth > innerWidth`
  must be false at every width.

## Gotchas

- The CSP `<meta>` (line ~6) governs cross-host board iframes (`frame-src`),
  camera streams (`img-src`), and probe fetches (`connect-src`) — a policy
  regression breaks every per-board modal while same-host testing still passes.
- Syntax-check the inline scripts before browser time:
  extract each `<script>` block → `node --check`. Beware: a `<script>` mention
  inside the `<style>` comment defeats naive regex extraction; re-grep
  `<script>`/`</script>` for the real line numbers on touch — they move as
  the file grows or shrinks (two blocks: the vendored mqtt.js bundle, then
  the app's own code).
- After any dashboard change: `robot/tools/sync-dashboard.sh` (vendored copy),
  and the Pi only picks it up on the next hubd build (include_str!).
- `robot`'s portal pages are C string literals (`wifi_portal.c`: HEAD, PAGE_*,
  LANDING_*, WELCOME_*). To check them, extract and `node --check` — but
  **strip C comments FIRST**: a non-greedy match to the terminating `;\n` stops
  at any `;` ending a comment line, silently truncating the page and "proving"
  a field is missing that is right there (hit 2026-07-16). Same family as the
  `<script>`-inside-`<style>`-comment trap above. Check each `<script>` block
  SEPARATELY too — PAGE has two, and concatenating them reports a false syntax
  error. And `wifi_portal_start`'s `max_uri_handlers` is a COUNTED budget with
  its arithmetic in the comment above it: adding a route without bumping it
  costs the last handler registered, at runtime, silently.
