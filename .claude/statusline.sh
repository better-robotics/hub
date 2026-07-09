#!/bin/bash
# Project statusLine override: wraps the global statusline (dir/model/branch)
# and appends hubd liveness — hubd's own /fleet endpoint is the liveness
# check the codebase already uses (better-robotics.github.io/landing.js).
input="$(cat)"
base="$(printf '%s' "$input" | node "$HOME/.claude/statusline.mjs")"

if out="$(curl -sf -m 0.3 http://localhost:8000/fleet 2>/dev/null)" \
  && n="$(printf '%s' "$out" | jq -e '.robots | length' 2>/dev/null)"; then
  printf '%s · 🟢 hub localhost:8000 (%s)' "$base" "$n"
else
  printf '%s · ⚪ hub down' "$base"
fi
