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
- Instructor-gated sections (the Assign sheet, e-stop Stop-all/Clear) are
  `display:none` offline until a instructor session is simulated — walk
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

## Contrast audit

**A token comment that states a ratio is a claim, not a measurement — measure
it.** `--ink-faint` carried `/* ≥4.5:1 on --inset (placeholders live there) */`
and shipped at **4.24** (2026-07-16), so every placeholder and `.log-payload` in
the file was under AA behind a comment asserting it wasn't. Three more went with
it: `.dir-out` 3.45, `.dir-in` 3.85 (the tree quietly took dimmer tokens than
the log's, under a comment claiming the arrows were the same), and — worst —
`.notice.danger`'s **"Emergency stop engaged" at 4.11**. Dark-committed UI is
where this hides: everything *looks* high-contrast on near-black.

Walk every rendered text node + every `::placeholder`, compare against the first
opaque ancestor background, assert `>= 4.5` (`>= 3` for large text: `>= 24px`, or
`>= 18.66px` bold). Standard WCAG 2.x relative-luminance math — write it fresh;
the four things below are what make it *right for this file*, and each one
turned a wrong answer into a real defect on 2026-07-16.

- **Resolve colours through a 1×1 canvas** (`ctx.fillStyle = v` →
  `getImageData`), never by parsing `getComputedStyle`. A `color-mix()` computes
  to `color(srgb 0.31 0.62 0.78)` and an `oklch()` to `oklch(0.55 0.17 27)` —
  taking the first three numbers reads those as RGB triples and reports
  confident nonsense instead of throwing. Both variants were hit in one session.
  Sanity-check the resolver before trusting it: white on black must be exactly 21.
- **Reveal before measuring.** The first pass read *clean* and was wrong: the log
  drawer was closed, so 19 elements had 0×0 boxes and were silently skipped —
  the same species as the touching-pairs sweep never being pointed at
  `#chip-pop`. Open `#log-body`, the popovers and the sheets, and force
  `#estop-banner` engaged; that banner's string is the one that must never miss.
  Skip `.sr-only` (never rendered, never seen) and filter on
  `getClientRects().length`.
- **`::placeholder` is not a text node** — a text-node walk cannot see it, and
  it is where the real failure lived.
- **A glyph in a `<span>` is TEXT.** `.dir-in`/`.dir-out` are `↓`/`↑` characters,
  so SC 1.4.3's 4.5 applies — not SC 1.4.11's 3:1 for graphical objects. Check
  for an actual `<svg>` before granting anything the 3:1 bar; that distinction
  decided whether two of the four failures were real.
- **Non-text still has a floor.** UI component boundaries and state need 3:1
  (SC 1.4.11). An empty `input` is the case that bites: its fill is 1.18:1 on a
  card, so the *border* is the only thing making the field perceivable —
  `--border-input` exists for that and nothing else.
- **A shared token's fix must be re-checked at every dependent pairing.** Moving
  `--danger-text` changes `.notice.danger`'s text, its border mix, AND the
  `.tchip.danger` fill that white sits on. Compute all of them before choosing.
- **`color-mix(in srgb, …)` is not perceptually uniform.** Mixing toward black
  in sRGB shifts lightness non-linearly: the e-stop chip's fill gives white
  5.95:1 in srgb and **7.19:1** in `oklab` for the same 72%. When a mix exists
  *to reach* a ratio, `in oklab` is the cheaper way to get there. (WCAG 2.x's
  own math is known-shaky on dark backgrounds — APCA is the honest cross-check,
  not a gate. Ship against 2.x; sanity-read with APCA.)

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

  **Skip the dense-by-design containers, or the sweep cries wolf.** A log's
  lines and a `.list-group`'s rows are SUPPOSED to touch — that's the
  grouped-inset idiom, hairline separators and no gap. This never mattered
  while `#wire-log` and the tree lived outside `<main>` (in the log drawer and
  the rail); the Messages merge moved both inside it, so `main div` now reaches
  them and reported ~40 "defects" in one run, every one correct behaviour.
  A sweep that fires on healthy markup gets skimmed, and then it stops working.

  ```js
  // Rows that must touch. Extend this, never the threshold.
  const DENSE = '#wire-log, .tw-tree, [id^="twg-"], .list-group, dl, .rail, .segmented, #topic-tree';
  const bad = [];
  for (const p of document.querySelectorAll("main, main section, main div, form, details, .robot, #chip-pop, #chip-pop div, .modal-card, .modal-card div, .stack, .panel, .panel-head, .wire-head, #topic-detail"))
    { if (p.closest(DENSE) || p.matches(DENSE)) continue;
      const kids = [...p.children].filter(el => { const r = el.getBoundingClientRect(), cs = getComputedStyle(el);
        return r.height > 8 && r.width > 0 && cs.display !== "none" && cs.position !== "absolute"; });
      for (let i = 0; i < kids.length - 1; i++) { if (kids[i].matches(DENSE) || kids[i+1].matches(DENSE)) continue;
        const g = kids[i+1].getBoundingClientRect().top - kids[i].getBoundingClientRect().bottom;
        if (g >= -1 && g < 5 && kids[i].tagName !== "H2" && kids[i].tagName !== "SUMMARY") bad.push([kids[i], kids[i+1], g]); } }
  bad
  ```

- **The phone tab bar floats OVER the content** (`.rail` is `position: fixed`
  under 720px), so it is invisible to both sweeps above — neither an overflow
  nor a touching pair can see a control lying on top of another. Two things to
  check by hand at 320/390, and `elementFromPoint` at a control's own centre is
  the only honest test (a rect that merely *exists* proves nothing):
  **`#estop-chip` must stay hittable** — it is the room's kill switch, it lives
  in `.topbar`, and "an always-reachable Stop must survive any IA change" is
  `dashboard-redesign.md`'s safety floor, the one it says to carry into every
  layout (it was written because an exploration's sidebar dropped it). Reveal it with
  `.classList.add("show")`: the real gate is `prof && !engaged`, so **engaging
  the e-stop hides it on purpose** (the banner's Clear takes over) — a test that
  engages first proves nothing and looks like a pass.
  **The log's tail must clear the bar** — `.content`'s `padding-bottom` is what
  buys that. Scroll `.content` to its end and assert
  `#wire-log`'s `bottom <= .rail`'s `top`. A tail-following log pins the newest
  line to its bottom edge, which is exactly where the glass floats; Instagram's
  feed can scroll past its bar, this cannot.

- **Horizontal overflow**: `document.documentElement.scrollWidth > innerWidth`
  must be false at every width.

- **An `<iframe>` IS a viewport** — media queries, `dvh` and layout all resolve
  against it, and it's the way to sweep widths when the harness can't resize the
  window (`resize_window` reported success while `innerWidth` stayed 1512 and
  `outerWidth` read 0). Same-origin, so `contentWindow.ingestSys` reaches the
  app inside it. Stage through the app's real ingestion path, never by poking
  the store: `window.robots` is the `<div id="robots">` element (id-globals),
  NOT the `robots` Map — the Map is script-scoped and unreachable. Assigning to
  it "works" silently and renders nothing.

## Delivery audit

**Every pass above asks "is the page correct?" — none asks "what does it cost
to deliver?", which is why this category hid for months in a file audited many
times over.** It is not a general web-perf concern here: the Pi is one AP radio
that `pi/CLAUDE.md` already measured into starvation, and a class opens the
same page at once, so bytes are multiplied by ~30 against the known bottleneck.
A correctness pass will never surface any of it.

Measure a real open with `performance.getEntriesByType('resource')`, cold and
again on reload; `transferSize ≈ decodedBodySize` ⇒ nothing is compressed. The
findings that aren't re-derivable (all measured 2026-07-16):

- **The file tree lies.** `du` calls the IDE bundle 17 MB; a real open transfers
  **5.4 MB** — Monaco lazy-loads 8.9 MB of language workers that never arrive.
  Ranking the work off file sizes got the priorities wrong.
- **`no-cache` with no `ETag`/`Last-Modified` is not a caching policy, it's
  "download it again"** — and it reads like caching in review. `/ide/` refetched
  27 requests / 5.4 MB *every load*; with a validator, **15 KB**, the 3.5 MB
  Monaco chunk down to 300 bytes. Same species as an exit code printed but never
  propagated: a mechanism that looks like it's checking and isn't.
- **gzip, not minification** — dashboard.html raw 646 KB → gzip **204 KB**
  (**31.6%**); minifying *first* buys only ~23 KB more, because **62% of the
  file is already-minified vendor blob**. Minification does ~5% of the work and
  charges the comments, a build step in two places, and an undebuggable page.
  The ratio is the durable part — the byte counts drift every time the page
  grows, and did within a day of first being written down here.
- **Not `immutable`**, though the bundle ships content-hashed names for exactly
  that: it needs a filename heuristic, and a false positive pins a mutable asset
  in a student's cache forever. Revalidation costs one LAN round-trip and cannot
  go stale — the bytes were the problem, not the round-trips.
- **Payload vs purpose** — 3.58 MB of editor core ships for a *read-only* Python
  preview. Ask it of any vendored asset.

## Gotchas

- **Clear `localStorage` between probe runs.** `hubRailCollapsed`,
  `hubLogOpen`, `hubPanel` and `hubDriveMode` persist per origin, so one
  `setRail(true)` in an earlier probe silently collapsed the rail in *every*
  later iframe load — a sweep then reports `.navitem` at 16px wide and it looks
  like a real narrow-width defect. The measurement lied, not the page
  (2026-07-16).
- **`const` helpers are not on `window`.** Only `function` declarations become
  globals in a classic script — `ingestSys`, `ingestEstop`, `renderEstop`,
  `renderRobots`, `openConfig` are reachable; `announce`, `alertNow`,
  `sheetStatus`, `panelHead`, `esc` are not. Drive behaviour through the
  function-declaration entry points; that's better verification anyway (it
  exercises the real path instead of the helper in isolation).
- **A dead `python3 -m http.server` looks like a security bug.** When the server
  dies mid-session the page keeps rendering from memory while storage and
  same-origin frame access start throwing `SecurityError` / "Access is denied
  for this document" — which reads exactly like a CSP `sandbox` regression. Curl
  the URL before debugging the policy (2026-07-16).
- **Don't hand-roll tag balance with regex.** Counting `<section>` was defeated
  twice in one pass: once by an HTML comment that *contained* `<section id="codes"
  hidden>`, once by a **CSS** comment containing `<h2#details-label>` (stripping
  `<!-- -->` doesn't touch `/* */`). Both produced a confident "balanced" over a
  real stray `</section>`. Use `html.parser` and let it tell you. Same family as
  the `<script>`-inside-`<style>`-comment trap below.
- **Live regions must be verified by BEHAVIOUR, not presence.** `role="status"`
  on a rendered empty node is necessary, not sufficient. The test that matters:
  fire the state change through its real path, then re-render N times and assert
  the region did **not** mutate again — the 2 s clock calls `renderEstop()`
  unconditionally, and an unguarded identical write is a no-op that several AT
  re-announce. Watch it with a `MutationObserver`; repeat renders must produce
  zero records.

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
