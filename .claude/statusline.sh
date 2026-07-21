#!/bin/bash
# Global statusline (dir · model · branch) + hub liveness + live fleet count.
#
# Target: the broker IS the hub, and on the hub's AP the DHCP gateway is the hub
# (the same discovery the robots use), so default to the gateway unless HUB_HOST
# overrides. Off the hub's network the hub is unreachable → "hub down", honestly
# (e.g. on WhiteSky, which isolates this Mac from the Pi's subnet — you see the
# fleet only when the Mac is on the hub's AP, which is the intended dev spot).
#
# Count: distinct robot board-ids on robots/+/sys (all teams share the topic, so
# the payload's "board" field is what's distinct). An MQTT census takes ~2s, so
# it runs in the BACKGROUND and caches to /tmp — the status line stays instant,
# showing the last count while a refresh runs.
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
cache="/tmp/hub_fleet_count.$host"
msub="$(command -v mosquitto_sub || echo /opt/homebrew/bin/mosquitto_sub)"

broker_up() {
  python3 -c "import socket;socket.setdefaulttimeout(0.4);socket.create_connection(('$host',1883)).close()" 2>/dev/null
}

if broker_up; then
  # Refresh the fleet count in the background when the cache is missing or >8s old.
  mtime=$(stat -f %m "$cache" 2>/dev/null || echo 0)
  if [ "$(( $(date +%s) - mtime ))" -gt 8 ]; then
    ( n=$("$msub" -h "$host" -t 'robots/+/sys' -W 3 2>/dev/null \
          | grep -o '"board":"[^"]*"' | sort -u | grep -c .); echo "$n" >"$cache" ) >/dev/null 2>&1 &
  fi
  n=$(cat "$cache" 2>/dev/null); [ -z "$n" ] && n="…"
  s="s"; [ "$n" = "1" ] && s=""
  printf '🟢 hub · %s robot%s · http://%s/' "$n" "$s" "$host"
else
  printf '⚪ hub down'
fi
