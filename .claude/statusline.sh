#!/bin/bash
# Hub liveness — the only thing the built-in footer (dir/model/git) can't show.
# Fast TCP check to the hub's MQTT broker; works for both the Pi and ESP32 hubs.
#
# When up, the label is an OSC 8 hyperlink to the dashboard (http://$host/):
# clickable in iTerm2 / VS Code / WezTerm / kitty; harmlessly swallowed by
# terminals that don't support OSC 8 (they show the text, no link, no garbage).
#
# No robot *count* here on purpose: /fleet is {uplink, locator}, not a census,
# and counting via an MQTT subscribe is slow + undercounts (the ESP32 broker
# keeps no retained sys). A live count wants a hub-side /fleet census endpoint
# (the broker already sees every connect) — added with the rover firmware.
host="${HUB_HOST:-hub.local}"
if python3 -c "import socket; socket.setdefaulttimeout(0.4); socket.create_connection(('$host', 1883)).close()" 2>/dev/null; then
  # OSC 8: ESC ] 8 ;; URL ST  <text>  ESC ] 8 ;; ST
  printf '\033]8;;http://%s/\033\\🟢 hub up (%s)\033]8;;\033\\' "$host" "$host"
else
  printf '⚪ hub down'
fi
