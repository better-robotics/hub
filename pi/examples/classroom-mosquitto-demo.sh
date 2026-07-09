#!/bin/bash
# ACL parity demo — the MQTT answer to hub-zenoh's examples/acl-demo.sh: proves
# Mosquitto's broker-native ACL enforces the same classroom scoping hubd's old
# /ws Rust relay used to (2026-07-08: scoping moved to the broker so browsers
# can be direct mqtt.js clients — no relay in the middle).
#
#   Phase 1  authorized paths work: public sys-read, rover publish, professor
#            full access, team1 scoped to its own robot
#   Phase 2  team1's credentials, team2's topics → denied (no relay to leak
#            through; enforcement is the broker's, not application code)
#   Phase 3  wrong password → connection refused (loud)
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
# so this must be a fresh file each run. Throwaway demo secrets, gitignored,
# matching classroom.example.json5's placeholder values.
rm -f mosquitto-passwd.example
mosquitto_passwd -b -c mosquitto-passwd.example rover rover-secret
mosquitto_passwd -b mosquitto-passwd.example professor change-me
mosquitto_passwd -b mosquitto-passwd.example team1 change-me-team1
mosquitto_passwd -b mosquitto-passwd.example team2 change-me-team2
chmod 0700 mosquitto-passwd.example mosquitto-acl.example.conf
echo "== mosquitto-passwd.example (gitignored, salted) =="; cat mosquitto-passwd.example; echo

# A copy of mosquitto.example.conf on the demo port, backgrounded.
sed "s/^listener 1883/listener $PORT/; /^listener 9001/,+1d" mosquitto.example.conf > "$LOG/mosquitto.conf"
mosquitto -c "$LOG/mosquitto.conf" >"$LOG/broker.log" 2>&1 & BROKER=$!
sleep 1

pub() { mosquitto_pub -h $HOST -p $PORT -u "$1" -P "$2" -t "$3" -m "$4" -q 1 "${@:5}"; }
sub() { mosquitto_sub -h $HOST -p $PORT ${1:+-u "$1"} ${2:+-P "$2"} -t "$3" -C 1 -W 2 2>&1; }

# Assertions so this doubles as a CI gate: each check bumps FAILS on mismatch,
# and the script exits nonzero if any failed. `empty_or_timeout` folds
# mosquitto_sub's -W behaviour (prints "Timed out", or nothing) into one test.
FAILS=0
pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1"; FAILS=$((FAILS + 1)); }
empty_or_timeout() { [[ -z "$1" || "$1" == *"Timed out"* ]]; }

echo "===== PHASE 1 — authorized paths ====="
pub rover rover-secret "robots/team1/sys" '{"hw":"esp32"}' -r
ANON=$(sub '' '' 'robots/+/sys')
[[ "$ANON" == *esp32* ]] && pass "anonymous reads public sys telemetry" || fail "anonymous sys read got: '$ANON'"
if pub professor change-me "robots/team1/pwm" '{"left_motor":50}'; then
  pass "professor → team1/pwm accepted"
else
  fail "professor pwm publish was rejected"
fi
T1=$(sub team1 change-me-team1 'robots/team1/#')
[[ "$T1" == *esp32* ]] && pass "team1 reads its own scope" || fail "team1 own-scope read got: '$T1'"

echo
echo "===== PHASE 2 — cross-team denial ====="
pub rover rover-secret "robots/team2/sys" '{"hw":"esp32"}' -r
# Retained (-r): if the ACL wrongly let this through, it sits on the topic for
# team2 to find later — a non-retained publish would time out either way and
# prove nothing.
pub team1 change-me-team1 "robots/team2/pwm" '{"left_motor":99}' -r 2>/dev/null
LEAK=$(sub team2 change-me-team2 'robots/team2/pwm')
empty_or_timeout "$LEAK" && pass "team1's write to team2 was denied (team2 saw nothing)" \
                          || fail "LEAK — team2 received: $LEAK"
T1T2=$(sub team1 change-me-team1 'robots/team2/#')
empty_or_timeout "$T1T2" && pass "team1 cannot read team2's scope" \
                          || fail "team1 read team2's scope: $T1T2"

echo
echo "===== PHASE 3 — wrong password rejected ====="
if sub team1 WRONG 'robots/team1/#' 2>&1 | grep -qi "not authorised\|refused\|connection error"; then
  pass "wrong password rejected"
else
  fail "wrong password was NOT rejected"
fi

kill $BROKER 2>/dev/null; wait 2>/dev/null
echo
if [[ $FAILS -eq 0 ]]; then echo "ALL CHECKS PASSED"; else echo "$FAILS CHECK(S) FAILED (logs: $LOG)"; fi
[[ $FAILS -eq 0 ]] && rm -rf "$LOG"
exit $(( FAILS > 0 ? 1 : 0 ))
