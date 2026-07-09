#!/bin/bash
# Project status line — committed (team-shared), self-contained (no personal
# ~/.claude deps, so it works for everyone), and hubd-free.
#
# Line 1: dir · model · branch (rebuilt from the status-line stdin JSON).
# Line 2: hub liveness — a fast TCP check to the BROKER (hub.local:1883), the
#         real source of truth, so it reflects both the Pi and ESP32 hubs and
#         needs nothing auto-started. (Robot *count* would need hubd's /fleet
#         aggregator or a slow MQTT subscribe — deliberately not done here.)
input="$(cat)"
dir=$(printf '%s' "$input" | jq -r '.workspace.current_dir // .cwd // ""')
model=$(printf '%s' "$input" | jq -r '.model.display_name // ""')
branch=$(git -C "$dir" rev-parse --abbrev-ref HEAD 2>/dev/null)

line1="$(basename "$dir" 2>/dev/null)"
[ -n "$model" ]  && line1="$line1 · $model"
[ -n "$branch" ] && line1="$line1 · $branch"

host="${HUB_HOST:-hub.local}"
if python3 -c "import socket; socket.setdefaulttimeout(0.4); socket.create_connection(('$host', 1883)).close()" 2>/dev/null; then
  line2="🟢 hub broker up ($host)"
else
  line2="⚪ hub down"
fi

printf '%s\n%s' "$line1" "$line2"
