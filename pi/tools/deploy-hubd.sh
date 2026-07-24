#!/usr/bin/env bash
# deploy-hubd.sh — build hubd in CI and deploy it to the live Pi over the USB
# serial console, one command.
#
#   ./tools/deploy-hubd.sh
#
# Steps: verify the tree is pushed → dispatch build-hubd.yml → wait, checking
# the run's headSha matches HEAD → fetch the artifact's pre-signed URL →
# install + restart on the Pi → sync tools/ to /opt/hub/tools → probe /fleet.
#
# WHY SERIAL — and read this before trusting it. The original reasons were "the
# repo is private" (so the Pi needs a pre-signed URL to fetch the artifact with
# no credentials) and "the workstation↔Pi network path is usually
# client-isolated". BOTH ARE NOW FALSE: the repo went public, and on the bench
# LAN `ssh pi@hub.local` works. On 2026-07-16 find_pi() also matched no console
# at all — the attached USB serials were robots — so this path did not run,
# while the network path it rules out deployed fine. The pre-signed URL still
# earns its keep for a Pi that genuinely has no route to this workstation (a
# classroom with client isolation on), which is the case this was built for and
# is worth keeping; it is no longer the DEFAULT case.
#
# The equivalent over ssh, when the Pi is reachable and no console is attached
# — same guarantee, the deployed binary is the reviewed one (match the artifact
# checksum, and `install.sh` cannot run on the Pi: it native-builds and there is
# no toolchain there by design):
#
#   gh run download <run> -R sprocket-robotics/hub -n hubd-arm64
#   scp hubd pi@hub.local:/tmp/hubd-new
#   ssh pi@hub.local 'sha256sum /tmp/hubd-new | grep -q <sha> \
#     && sudo cp -a /opt/hub/hubd /opt/hub/hubd.prev \
#     && sudo install -m755 /tmp/hubd-new /opt/hub/hubd \
#     && sudo systemctl restart hubd && sleep 3 && systemctl is-active hubd \
#     && curl -sf http://127.0.0.1/fleet >/dev/null && echo "fleet OK"'
#
# Not wired in as a --ssh flag on purpose: an untested branch in the script that
# installs binaries on a live classroom hub is worth less than a command a human
# runs and watches. Add the flag when someone can exercise both paths.
set -euo pipefail

REPO=sprocket-robotics/hub   # was hub-mqtt — merged into the monorepo 2026-07-08
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
# `&& echo`, never `; echo` — the trailing echo exists because pi-serial.py
# drops output sharing a line with its done-marker and curl emits no final
# newline, but as `;` it also RESET the status, so this "verification" reported
# success even when both curls failed. And -sf, not -s: without -f, curl exits 0
# on a 500. Two ways for a check to pass while the thing it checks is down.
pi "(curl -sf http://127.0.0.1/fleet || curl -sf http://127.0.0.1:8000/fleet) && echo" 15
echo
# Reached only if every `pi` above exited 0 (set -e + pi-serial's real status).
# This line used to print unconditionally — including the run where the remote
# curl couldn't resolve github.com, nothing was installed, and the Pi went on
# serving the previous binary while the deploy announced the new one.
echo "[deploy] done — hubd ${SHA:0:7} live"
