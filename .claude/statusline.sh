#!/bin/bash
# Global statusline (dir · model · branch) + hub liveness.
#
# Target: the Zenoh router IS the hub's transport, and on the hub's AP the DHCP
# gateway is the hub (the same discovery the robots use), so default to the
# gateway unless HUB_HOST overrides. Off the hub's network the hub is
# unreachable → "hub down", honestly (e.g. on WhiteSky, which isolates this Mac
# from the Pi's subnet — you see the hub only when the Mac is on the hub's AP,
# which is the intended dev spot).
#
# No fleet census: the live robot count came from an MQTT subscribe
# (`mosquitto_sub` on robots/+/sys), retired in the Zenoh cutover. hubd's /fleet
# is now just {uplink, locator} (no per-robot roll-up — the dashboard opens its
# own WS-JSON connection to the ws-adapter to count), and a Zenoh subscribe
# census needs a client this Mac isn't guaranteed to have, so the line degrades
# to liveness-only rather than lie or crash.
#
# Clickable: the dashboard URL is printed as plain text; terminals auto-linkify
# it (cmd/ctrl-click) — more robust than an OSC-8 escape, which some hosts strip.

# Base segments (dir · model · branch · reader) come from the global statusline —
# composed, not re-implemented, so the two never drift. Same stdin JSON both ways.
input="$(cat)"
base="$(printf '%s' "$input" | node "$HOME/.claude/statusline.mjs" 2>/dev/null)"
[ -n "$base" ] && printf '%s · ' "$base"

host="${HUB_HOST:-$(route -n get default 2>/dev/null | awk '/gateway:/{print $2}')}"
[ -z "$host" ] && host="hub.local"

# The Zenoh router (tcp/<gateway>:7447) is the transport — reachable = hub up.
hub_up() {
  python3 -c "import socket;socket.setdefaulttimeout(0.4);socket.create_connection(('$host',7447)).close()" 2>/dev/null
}

if hub_up; then
  printf '🟢 hub · http://%s/' "$host"
else
  printf '⚪ hub down'
fi
