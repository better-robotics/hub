#!/usr/bin/env bash
# hub-uplink-watchdog.sh — recover the USB uplink radio when its driver wedges.
#
# The Edimax RTL8188CUS (rtl8192cu) that takes the STA leg loses its scan/assoc
# path when the venue AP goes away, and never gets it back:
#
#   [ 2249.317] rtlwifi: AP off, try to reconnect now
#   [ 2812.887] wlan1: send auth to <bssid> (try 3/3)
#   [ 2813.095] wlan1: authentication with <bssid> timed out
#
# The radio still hears beacons — the Wi-Fi picker lists the network at 100% —
# so NetworkManager reports the join failure as `ssid-not-found`, i.e. "No
# network with SSID 'X' found". That message sent a human hunting a network
# that was sitting right there. The dongle never leaves the USB bus; it is
# driver state, and only a module reload clears it (verified 2026-07-16:
# reload -> connect went straight to an IP where every retry before it timed
# out in auth).
#
# Reloading rtl8192cu does NOT touch the AP: that is wlan0 on brcmfmac, a
# different driver on a different chip. This is why the watchdog exists rather
# than a reboot — the classroom keeps its AP and its rovers while the uplink
# leg is recycled underneath them.
#
# WORKING WHEN: the uplink returns without a reboot. `journalctl -u
# hub-uplink-watchdog` shows `reload -> reconnected` and no human in the loop.
# TEAR DOWN IF: $MAX_FAILS consecutive reloads fail to reconnect. That would
# mean the wedge is not what keeps the uplink down, the premise here is wrong,
# and this is just a script power-cycling a radio at the venue's expense —
# delete it rather than tune it. It goes dormant at that point instead of
# looping, so the failure is visible in the journal rather than as churn.
set -uo pipefail

DRIVER=rtl8192cu       # the wedging driver. Never an interface NAME: wlan0/wlan1
                       # is a per-boot enumeration coin flip between this dongle
                       # and the builtin radio — a boot that lost that toss is
                       # in image/README.md § First hardware boot.
CHECK=${CHECK:-60}     # seconds between polls
NEED=${NEED:-3}        # consecutive down polls before acting (~3 min)
COOLDOWN=${COOLDOWN:-600}   # seconds to wait after a reload before acting again
MAX_FAILS=${MAX_FAILS:-5}   # consecutive failed reloads before giving up

log() { echo "hub-uplink-watchdog: $*"; }

# The netdev bound to $DRIVER, or empty. Driver-based, per the house rule.
uplink_dev() {
  local link n
  for link in /sys/class/net/*/device/driver; do
    [ -e "$link" ] || continue
    if [ "$(basename "$(readlink -f "$link")")" = "$DRIVER" ]; then
      n=${link#/sys/class/net/}
      echo "${n%%/*}"
      return
    fi
  done
}

# The non-AP wifi profile NM would autoconnect, or empty. Without one there is
# no uplink configured and a reload would be pointless radio churn.
sta_profile() {
  local name mode
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    mode=$(nmcli -g 802-11-wireless.mode connection show "$name" 2>/dev/null)
    [ "$mode" = "ap" ] && continue
    echo "$name"
    return
  done < <(nmcli -t -f NAME,TYPE connection show 2>/dev/null \
             | awk -F: '$2=="802-11-wireless" {print $1}')
}

dev_state() { nmcli -t -f DEVICE,STATE device status 2>/dev/null | awk -F: -v d="$1" '$1==d {print $2}'; }

strikes=0
fails=0
cooling=0

log "started (driver=$DRIVER check=${CHECK}s need=$NEED cooldown=${COOLDOWN}s)"

while :; do
  sleep "$CHECK"

  if [ "$cooling" -gt 0 ]; then
    cooling=$((cooling - CHECK))
    continue
  fi

  dev=$(uplink_dev)
  if [ -z "$dev" ]; then
    # No netdev for the driver at all: the dongle is unplugged, or the module
    # is gone. Not our failure mode — a reload can't conjure hardware.
    strikes=0
    continue
  fi

  state=$(dev_state "$dev")
  if [ "$state" = "connected" ]; then
    [ "$strikes" -gt 0 ] && log "$dev recovered on its own"
    strikes=0
    fails=0
    continue
  fi

  prof=$(sta_profile)
  if [ -z "$prof" ]; then
    strikes=0   # no uplink configured; being down is the correct state
    continue
  fi

  strikes=$((strikes + 1))
  [ "$strikes" -ge "$NEED" ] || continue

  log "$dev has been '$state' for $((strikes * CHECK))s with profile '$prof' — reloading $DRIVER"
  modprobe -r "$DRIVER" 2>&1 | sed 's/^/  rmmod: /'
  sleep 3
  modprobe "$DRIVER" 2>&1 | sed 's/^/  modprobe: /'

  # Wait for the netdev to come back, then join IMMEDIATELY. The window is
  # real: reload -> connect worked, while reload -> scan -> wait -> connect hit
  # the same auth timeout. Don't leave it to autoconnect's timing.
  for _ in $(seq 20); do
    sleep 1
    dev=$(uplink_dev)
    [ -n "$dev" ] && [ "$(dev_state "$dev")" != "unavailable" ] && break
  done

  nmcli connection up "$prof" ifname "$dev" 2>&1 | sed 's/^/  up: /'
  sleep 5

  if [ "$(dev_state "$dev")" = "connected" ]; then
    log "reload -> reconnected as $dev ($prof)"
    fails=0
  else
    fails=$((fails + 1))
    log "reload -> still $(dev_state "$dev") (failure $fails/$MAX_FAILS)"
    if [ "$fails" -ge "$MAX_FAILS" ]; then
      log "GIVING UP: $MAX_FAILS reloads in a row did not restore the uplink."
      log "The premise of this watchdog (a wedged $DRIVER is what keeps the"
      log "uplink down) does not hold here. Delete it rather than tune it."
      exit 0   # dormant, not looping: success so systemd won't restart us
    fi
  fi

  strikes=0
  cooling=$COOLDOWN
done
