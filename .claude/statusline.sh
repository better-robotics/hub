#!/bin/bash
# Hub liveness — the only thing the built-in footer (dir/model/git) can't show.
# Fast TCP check to the hub's MQTT broker; works for both the Pi and ESP32 hubs.
host="${HUB_HOST:-hub.local}"
if python3 -c "import socket; socket.setdefaulttimeout(0.4); socket.create_connection(('$host', 1883)).close()" 2>/dev/null; then
  printf '🟢 hub up (%s)' "$host"
else
  printf '⚪ hub down'
fi
