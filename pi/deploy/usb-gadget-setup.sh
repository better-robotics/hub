#!/bin/bash
# USB composite gadget (ECM ethernet + ACM serial) via ConfigFS. Runs
# independently of pi-robot.service so a crashed firmware still exposes
# `ssh pi@10.55.0.1` (ECM) and a serial console at /dev/ttyGS0 (ACM).

# Diagnostic logging block runs WITHOUT `set -e` — its failure must not
# prevent the gadget setup. The previous version coupled them, so a
# transient /boot/firmware-not-yet-mounted at unit-start time silently
# aborted the entire script before any ConfigFS work happened. Gadget
# setup re-enables -e at the marker below.
set -uo pipefail

# Append every boot's outcome to a log on the boot partition. The whole
# point of USB-CDC is being reachable when SSH/journal aren't — so
# the log goes somewhere recoverable WITHOUT the Pi (pop the SD into any
# host). Append, not overwrite, so multi-boot failure patterns stay
# visible. Only redirect when /boot/firmware looks writable — otherwise
# let stdout/stderr fall through to systemd's journal.
LOG=/boot/firmware/usb-gadget.log
if [ -d "$(dirname "$LOG")" ] && touch "$LOG" 2>/dev/null; then
  exec >> "$LOG" 2>&1
  _log_path="$LOG"
else
  _log_path="(journal only — /boot/firmware not writable)"
fi
echo "=== usb-gadget-setup $(date -Iseconds) ==="
echo "logging to: $_log_path"
echo "kernel: $(uname -r)"
echo "cmdline: $(cat /proc/cmdline 2>/dev/null)"
echo "dtoverlay/dwc2 + modules:"
lsmod 2>/dev/null | grep -E "^(dwc2|libcomposite|usb_f_)" || echo "  (none loaded yet)"
# Force a sync now so the boot-time breadcrumb survives an unclean SD pull.
# FAT32 has no journal — without an explicit sync, writes can sit in the
# OS buffer cache for tens of seconds and disappear on yank.
sync 2>/dev/null || true

set -e  # Real failure path begins here.

GADGET=/sys/kernel/config/usb_gadget/g1
if [ -d "$GADGET" ]; then
  echo "gadget already configured (idempotent exit)"
  exit 0
fi

# Wait for dwc2 to register a UDC on /sys/class/udc. systemd-modules-load
# returns as soon as modprobe finishes, but dwc2's USB-device-controller
# registration is async on top of that — racing past it would leave us
# with an empty /sys/class/udc and a silent skip (ConditionPathExists
# previously masked this; we dropped it so the unit actually runs).
# 10s is generous: dwc2 publishes within ~200 ms on a healthy boot.
for i in $(seq 1 50); do
  if ls /sys/class/udc 2>/dev/null | grep -q .; then
    echo "UDC appeared after $((i * 200))ms: $(ls /sys/class/udc)"
    break
  fi
  sleep 0.2
done
if ! ls /sys/class/udc 2>/dev/null | grep -q .; then
  echo "FAIL: no UDC after 10s — dwc2 not loaded or in host mode"
  echo "Likely fix: ensure config.txt has \`dtoverlay=dwc2,dr_mode=peripheral\`"
  echo "and cmdline.txt has \`modules-load=dwc2,libcomposite\`."
  exit 1
fi

mkdir -p "$GADGET"
cd "$GADGET"

echo 0x1d6b > idVendor    # Linux Foundation
echo 0x0104 > idProduct   # Multifunction Composite Gadget
echo 0x0100 > bcdDevice
echo 0x0200 > bcdUSB

mkdir -p strings/0x409
SN=$(awk '/Serial/ { print $NF; exit }' /proc/cpuinfo 2>/dev/null || echo "0000000000")
# Per-chip product string so two Pis plugged into the same host can be
# told apart in System Information / lsusb. Derivation matches the hub-XXXX
# AP SSID: last 4 of /proc/cpuinfo Serial, uppercased.
SUFFIX=$(echo "$SN" | tail -c 5 | tr '[:lower:]' '[:upper:]')
[ -z "$SUFFIX" ] && SUFFIX="0000"
echo "$SN" > strings/0x409/serialnumber
echo "Sprocket Robotics" > strings/0x409/manufacturer
echo "hub-$SUFFIX" > strings/0x409/product

mkdir -p configs/c.1/strings/0x409
echo "ECM + ACM" > configs/c.1/strings/0x409/configuration
echo 250 > configs/c.1/MaxPower

mkdir -p functions/ecm.usb0
mkdir -p functions/acm.usb0

ln -s functions/ecm.usb0 configs/c.1/
ln -s functions/acm.usb0 configs/c.1/

# Bind to the first available USB Device Controller.
UDC=$(ls /sys/class/udc | head -n 1)
echo "binding gadget to UDC: $UDC"
echo "$UDC" > UDC
echo "bind OK — gadget live at $GADGET, UDC=$UDC"
sync 2>/dev/null || true
