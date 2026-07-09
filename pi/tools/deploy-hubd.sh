#!/usr/bin/env bash
# deploy-hubd.sh — build hubd in CI and deploy it to the live Pi over the USB
# serial console, one command. The repo is private and the workstation↔Pi
# network path is usually client-isolated, so everything rides the serial
# console: the artifact's pre-signed URL (so the Pi can download with no
# credentials) and the ops tools (base64-staged) both go over the wire.
#
#   ./tools/deploy-hubd.sh
#
# Steps: verify the tree is pushed → dispatch build-hubd.yml → wait, checking
# the run's headSha matches HEAD → fetch the artifact's pre-signed URL →
# install + restart on the Pi → sync tools/ to /opt/hub/tools → probe /fleet.
set -euo pipefail

REPO=better-robotics/hub   # was hub-mqtt — merged into the monorepo 2026-07-08
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$DIR"

# pi-serial.py autodetects the Pi's console (ports renumber on every USB
# re-enumeration); pin the discovery once so we don't re-probe per command.
PORT="${PI_SERIAL_PORT:-$(python3 -c "import sys; sys.path.insert(0, 'tools'); import importlib; print(importlib.import_module('pi-serial').find_pi())")}"
pi() { PI_SERIAL_PORT="$PORT" python3 tools/pi-serial.py "$@"; }
echo "[deploy] Pi console: $PORT"

# --- the binary deployed must be the binary reviewed: refuse a dirty or
# unpushed tree, and later match the CI run's headSha against this.
[[ -z "$(git status --porcelain)" ]] || { echo "working tree dirty — commit first" >&2; exit 1; }
git fetch -q origin main
SHA=$(git rev-parse HEAD)
[[ "$SHA" == "$(git rev-parse origin/main)" ]] || { echo "HEAD not pushed to origin/main" >&2; exit 1; }

echo "[deploy] dispatching build-hubd.yml @ ${SHA:0:7}…"
gh workflow run build-hubd.yml --repo "$REPO"

RUN=""
for _ in $(seq 60); do
  sleep 5
  read -r RUN STATUS CONCLUSION < <(gh run list --repo "$REPO" --workflow build-hubd.yml -L1 \
    --json databaseId,headSha,status,conclusion \
    -q ".[] | select(.headSha==\"$SHA\") | \"\(.databaseId) \(.status) \(.conclusion)\"") || true
  [[ -n "${RUN:-}" && "${STATUS:-}" == "completed" ]] && break
  RUN=""
done
[[ -n "$RUN" ]] || { echo "build did not complete in time" >&2; exit 1; }
[[ "$CONCLUSION" == "success" ]] || { echo "build $RUN concluded: $CONCLUSION" >&2; exit 1; }
echo "[deploy] build $RUN succeeded"

ART=$(gh api "repos/$REPO/actions/runs/$RUN/artifacts" -q '.artifacts[] | select(.name=="hubd-arm64") | .id')
URL=$(curl -s -w '%{redirect_url}' -H "Authorization: token $(gh auth token)" \
  "https://api.github.com/repos/$REPO/actions/artifacts/$ART/zip" -o /dev/null)
B64=$(printf '%s' "$URL" | base64 | tr -d '\n')

echo "[deploy] installing on the Pi…"
pi "echo $B64 | base64 -d > /tmp/hubd-url && cd /tmp && curl -sL \$(cat hubd-url) -o hubd.zip && unzip -o -q hubd.zip && sudo install -m755 /tmp/hubd /opt/hub/hubd && sudo systemctl restart hubd && sleep 3 && systemctl is-active hubd" 90

# --- ops tools live in /opt/hub/tools (not /tmp: reboots erase /tmp, and the
# private repo means the Pi can't fetch them itself).
for f in reprovision.py; do
  T=$(base64 < "tools/$f" | tr -d '\n')
  pi "sudo mkdir -p /opt/hub/tools && echo $T | base64 -d | sudo tee /opt/hub/tools/$f >/dev/null && sudo chmod 755 /opt/hub/tools/$f && echo synced-$f" 15
done

echo "[deploy] verifying /fleet…"
# trailing echo: pi-serial.py drops output that shares a line with its
# done-marker, and curl emits no final newline.
pi "(curl -s http://127.0.0.1/fleet || curl -s http://127.0.0.1:8000/fleet); echo" 15
echo
echo "[deploy] done — hubd ${SHA:0:7} live"
