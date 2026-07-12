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

Expected environment noise offline: 404s for /fleet, /wifi/status, /codes/*
and ws://…:9001 connection refusals (no hubd/broker behind a static server).
Anything else in the console is real.

## Handles

- App functions (`openConfig`, `openCamera`, `mintShareCard`, …) are globals —
  callable from `browser_evaluate` when no live MQTT fleet exists to click
  through. Clicks on the resulting DOM are still real gestures.
- Professor-gated sections (e.g. `#codes-share`) are `display:none` offline —
  walk ancestors and unhide before clicking.
- Fake board endpoint: a tiny HTTP server answering 200 on any path. It MUST
  send `Content-Type: text/html` — `python3 -m http.server` serves an
  extensionless `wifi` file as octet-stream, the iframe becomes a *download*,
  `load` never fires, and reachability tests silently lie.
- Unreachable board: use `127.0.0.1:9` (closed port, instant refusal) instead
  of a blackhole IP — no timeout wait.
- Insecure-context clipboard (hub.local behavior) on localhost: stub
  `Object.defineProperty(navigator,'clipboard',{value:undefined,configurable:true})`.

## Gotchas

- The CSP `<meta>` (line ~6) governs cross-host board iframes (`frame-src`),
  camera streams (`img-src`), and probe fetches (`connect-src`) — a policy
  regression breaks every per-board modal while same-host testing still passes.
- Syntax-check the inline scripts before browser time:
  extract each `<script>` block → `node --check`. Beware: a `<script>` mention
  inside the `<style>` comment defeats naive regex extraction; real blocks
  start at the `<script>` tags near lines ~658 / ~2958 / ~2980.
- After any dashboard change: `robot/tools/sync-dashboard.sh` (vendored copy),
  and the Pi only picks it up on the next hubd build (include_str!).
