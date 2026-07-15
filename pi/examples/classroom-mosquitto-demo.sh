#!/bin/bash
# ACL demo — proves Mosquitto's broker-native ACL enforces the classroom's
# Wi-Fi-perimeter model (confirmed 2026-07-13, CONTRACT.md § Discovery &
# isolation): every client, robot or browser, authenticated or not, gets
# full read+write on robots/# and pair/#. The one gated identity is
# professor, and it protects exactly one thing: fleet/estop.
#
#   Phase 1  open access: an anonymous client can publish AND read any
#            robot's subtree, no credential at all
#   Phase 2  fleet/estop: anonymous can read the latch but not write it;
#            professor can write it
#   Phase 3  wrong professor password → connection refused (loud)
#
# Run from the repo root. Needs mosquitto + mosquitto_pub/sub/passwd on PATH
# (brew install mosquitto). macOS ships no `timeout`(1), so every read uses
# mosquitto_sub's own -W wait-timeout instead. sys is published retained
# (-r) — a late subscriber must still see the last known value, same as the
# dashboard's "last seen" fleet view; that also sidesteps subscribe-after-
# publish ordering in this script.
set -u
cd "$(dirname "$0")/.." || exit 1
LOG=$(mktemp -d)
HOST=127.0.0.1
PORT=18830  # non-default: don't collide with a real broker on this machine

# 0. passwd file — mosquitto_passwd salts+hashes; -c on the first call only,
# so this must be a fresh file each run. Throwaway demo secret, gitignored,
# matching install.sh's placeholder seed.
rm -f mosquitto-passwd.example
mosquitto_passwd -b -c mosquitto-passwd.example professor change-me
chmod 0700 mosquitto-passwd.example mosquitto-acl.example.conf
echo "== mosquitto-passwd.example (gitignored, salted) =="; cat mosquitto-passwd.example; echo

# A copy of mosquitto.example.conf on the demo port, backgrounded.
sed "s/^listener 1883/listener $PORT/; /^listener 9001/,+1d" mosquitto.example.conf > "$LOG/mosquitto.conf"
mosquitto -c "$LOG/mosquitto.conf" >"$LOG/broker.log" 2>&1 & BROKER=$!
sleep 1

pub() { mosquitto_pub -h $HOST -p $PORT ${1:+-u "$1"} ${2:+-P "$2"} -t "$3" -m "$4" -q 1 "${@:5}"; }
sub() { mosquitto_sub -h $HOST -p $PORT ${1:+-u "$1"} ${2:+-P "$2"} -t "$3" -C 1 -W 2 2>&1; }

# Assertions so this doubles as a CI gate: each check bumps FAILS on mismatch,
# and the script exits nonzero if any failed. `empty_or_timeout` folds
# mosquitto_sub's -W behaviour (prints "Timed out", or nothing) into one test.
FAILS=0
pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1"; FAILS=$((FAILS + 1)); }
empty_or_timeout() { [[ -z "$1" || "$1" == *"Timed out"* ]]; }

echo "===== PHASE 1 — open access, no credential ====="
pub '' '' "robots/scout/sys" '{"hw":"esp32"}' -r          # anonymous device publish
ANON=$(sub '' '' 'robots/+/sys')
[[ "$ANON" == *esp32* ]] && pass "anonymous reads any robot's telemetry" || fail "anonymous sys read got: '$ANON'"
if pub '' '' "robots/scout/pwm" '{"left_motor":50}' -r; then
  pass "anonymous → any robot's pwm accepted (a name is an address, not a credential)"
else
  fail "anonymous pwm publish was rejected"
fi
CROSS=$(sub '' '' 'robots/scout/pwm')
[[ "$CROSS" == *left_motor* ]] && pass "anonymous reads it straight back — no per-robot scoping left" \
                                || fail "expected the pwm publish to be readable, got: '$CROSS'"

echo
echo "===== PHASE 2 — fleet/estop stays professor-gated ====="
# Baseline write needs the professor credential — anonymous has read-only on
# fleet/estop, so an anonymous publish here would be silently dropped and
# never establish the retained value the rest of this phase depends on.
pub professor change-me "fleet/estop" '{"engaged":false}' -r
STATE=$(sub '' '' 'fleet/estop')
[[ "$STATE" == *engaged* ]] && pass "anonymous reads the estop latch" || fail "anonymous estop read got: '$STATE'"
pub '' '' "fleet/estop" '{"engaged":true,"reason":"student"}' -r 2>/dev/null
# A rejected publish still exits 0 in mosquitto_pub; the ACL denial shows up
# as the retained value staying unchanged, checked next.
AFTER_ANON=$(sub '' '' 'fleet/estop')
[[ "$AFTER_ANON" == *'"engaged":false'* ]] && pass "anonymous write to fleet/estop was denied (latch unchanged)" \
                                            || fail "LEAK — anonymous engaged the room-wide estop: $AFTER_ANON"
if pub professor change-me "fleet/estop" '{"engaged":true,"reason":"professor"}' -r; then
  pass "professor → fleet/estop accepted"
else
  fail "professor estop publish was rejected"
fi
AFTER_PROF=$(sub '' '' 'fleet/estop')
[[ "$AFTER_PROF" == *'"engaged":true'* ]] && pass "the latch reflects the professor's write" \
                                           || fail "professor's write didn't land: $AFTER_PROF"

echo
echo "===== PHASE 3 — wrong professor password rejected ====="
if sub professor WRONG 'fleet/estop' 2>&1 | grep -qi "not authorised\|refused\|connection error"; then
  pass "wrong professor password rejected"
else
  fail "wrong professor password was NOT rejected"
fi

kill $BROKER 2>/dev/null; wait 2>/dev/null
echo
if [[ $FAILS -eq 0 ]]; then echo "ALL CHECKS PASSED"; else echo "$FAILS CHECK(S) FAILED (logs: $LOG)"; fi
[[ $FAILS -eq 0 ]] && rm -rf "$LOG"
exit $(( FAILS > 0 ? 1 : 0 ))
