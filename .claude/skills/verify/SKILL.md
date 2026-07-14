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

## Layout regression sweeps

Run in `browser_evaluate` at 320 / 390 / 768 / 1200 — and stage *hostile*
data first (long venue SSID in `#net-chip-label`, `#host-chip` populated,
long robot names): every real-device layout bug so far shipped because the
staged data was too polite.

- **Touching pairs** (spacing floor — anything under ~5px between stacked
  siblings is a defect unless it's the card title→meta pair):

  ```js
  const bad = [];
  for (const p of document.querySelectorAll("main, main section, main div, form, details, .robot"))
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
