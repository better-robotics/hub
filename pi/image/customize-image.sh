#!/bin/bash -eu
# Bake the hub appliance from the official Raspberry Pi OS Lite image — no
# pi-gen. The golden base is retrieved, not rebuilt: this script grows the
# rootfs, applies the base config pi-gen's stages used to own (user, ssh,
# hostname, locale), then runs the stage-hub payload (00-packages, 00-run.sh,
# 01-run-chroot.sh) — the same scripts, same order, same host/chroot contexts
# they had as a pi-gen stage.
#
# Runs in CI on a native arm64 Linux runner as root: chroot into the arm64
# rootfs needs no qemu/binfmt. Not runnable on macOS (loop devices, chroot) —
# which is why the image build lives in CI in the first place.
#
# usage: sudo --preserve-env=HUB_SSH_PUBKEY ./customize-image.sh <image.img>
#   HUB_SSH_PUBKEY   authorized_keys line for the first user (required)
#   stage-hub/00-hub/files/ must be staged by the caller (hubd + deploy/ files)

IMG="${1:?usage: customize-image.sh <image.img>}"
STEP_DIR="$(cd "$(dirname "$0")/stage-hub/00-hub" && pwd)"

FIRST_USER=pi
TARGET_HOSTNAME=hub
TIMEZONE=America/New_York
LOCALE=en_US.UTF-8
KEYMAP=us

[ -n "${HUB_SSH_PUBKEY:-}" ] || { echo "HUB_SSH_PUBKEY not set" >&2; exit 1; }
[ -f "$STEP_DIR/files/hubd" ]  || { echo "stage files not staged (no $STEP_DIR/files/hubd)" >&2; exit 1; }
[ "$(id -u)" = 0 ]             || { echo "must run as root" >&2; exit 1; }

# --- grow the image: Lite ships sized-to-content, apt needs headroom. The
# stock first-boot init_resize.sh still expands to the full card, so the exact
# slack size is irrelevant to the flashed Pi; zerofree below keeps it nearly
# free in the .xz.
truncate -s +1G "$IMG"
LOOP=$(losetup -fP --show "$IMG")
parted -s "$LOOP" resizepart 2 100%
partprobe "$LOOP"
e2fsck -pf "${LOOP}p2" || [ $? -le 1 ]   # 1 = fixed something, still healthy
resize2fs "${LOOP}p2"

ROOTFS=$(mktemp -d)

restore_chroot_files() {
    rm -f "$ROOTFS/usr/sbin/policy-rc.d" "$ROOTFS/etc/resolv.conf"
    if [ -e "$ROOTFS/etc/resolv.conf.orig" ] || [ -L "$ROOTFS/etc/resolv.conf.orig" ]; then
        mv "$ROOTFS/etc/resolv.conf.orig" "$ROOTFS/etc/resolv.conf"
    fi
}
on_fail() {
    set +e
    restore_chroot_files
    umount -R "$ROOTFS" 2>/dev/null
    losetup -d "$LOOP" 2>/dev/null
}
trap on_fail EXIT

mount "${LOOP}p2" "$ROOTFS"
mount "${LOOP}p1" "$ROOTFS/boot/firmware"
mount --bind /dev      "$ROOTFS/dev"
mount --bind /dev/pts  "$ROOTFS/dev/pts"
mount -t proc  proc "$ROOTFS/proc"
mount -t sysfs sys  "$ROOTFS/sys"

# The chroot needs DNS (apt) and must not start services (no systemd running).
if [ -e "$ROOTFS/etc/resolv.conf" ] || [ -L "$ROOTFS/etc/resolv.conf" ]; then
    mv "$ROOTFS/etc/resolv.conf" "$ROOTFS/etc/resolv.conf.orig"
fi
cp /etc/resolv.conf "$ROOTFS/etc/resolv.conf"
printf '#!/bin/sh\nexit 101\n' > "$ROOTFS/usr/sbin/policy-rc.d"
chmod +x "$ROOTFS/usr/sbin/policy-rc.d"

in_chroot() { chroot "$ROOTFS" /usr/bin/env DEBIAN_FRONTEND=noninteractive "$@"; }

# ---- base config (what pi-gen's stage1/stage2 settings used to provide) ----
echo "$TARGET_HOSTNAME" > "$ROOTFS/etc/hostname"
sed -i "s/^127\.0\.1\.1.*/127.0.1.1\t$TARGET_HOSTNAME/" "$ROOTFS/etc/hosts"

sed -i "s/^# *$LOCALE /$LOCALE /" "$ROOTFS/etc/locale.gen"
in_chroot locale-gen
in_chroot update-locale "LANG=$LOCALE"
echo "$TIMEZONE" > "$ROOTFS/etc/timezone"
ln -sf "/usr/share/zoneinfo/$TIMEZONE" "$ROOTFS/etc/localtime"
sed -i "s/^XKBLAYOUT=.*/XKBLAYOUT=\"$KEYMAP\"/" "$ROOTFS/etc/default/keyboard"

# First user. The base image ships a default user pending the first-boot
# rename wizard (disabled below) — create only if missing, and lock the
# password either way ('!' in shadow — password auth impossible everywhere):
# ssh is pubkey-only, the serial console autologs in, sudo is NOPASSWD.
# Cable/card possession is the auth boundary (image/README.md).
in_chroot getent passwd "$FIRST_USER" >/dev/null || \
    in_chroot adduser --disabled-password --gecos "" "$FIRST_USER"
in_chroot usermod -p '!' "$FIRST_USER"
for grp in adm dialout cdrom sudo audio video plugdev games users input render netdev spi i2c gpio; do
    in_chroot adduser "$FIRST_USER" "$grp" || true
done
echo "$FIRST_USER ALL=(ALL) NOPASSWD: ALL" > "$ROOTFS/etc/sudoers.d/010_$FIRST_USER-nopasswd"
chmod 0440 "$ROOTFS/etc/sudoers.d/010_$FIRST_USER-nopasswd"
install -d -m 0700 "$ROOTFS/home/$FIRST_USER/.ssh"
printf '%s\n' "$HUB_SSH_PUBKEY" > "$ROOTFS/home/$FIRST_USER/.ssh/authorized_keys"
chmod 0600 "$ROOTFS/home/$FIRST_USER/.ssh/authorized_keys"
chroot "$ROOTFS" chown -R "$FIRST_USER:$FIRST_USER" "/home/$FIRST_USER/.ssh"

in_chroot systemctl enable ssh
cat > "$ROOTFS/etc/ssh/sshd_config.d/10-hub-pubkey-only.conf" <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
EOF
# The stock image gates first boot on the user-creation wizard — and
# rename_user.conf refuses ALL ssh logins until it has run. Our user is baked,
# so the gate and the wizard both go (getty@tty1 comes back in its place).
rm -f "$ROOTFS/etc/ssh/sshd_config.d/rename_user.conf"
in_chroot systemctl disable userconfig.service 2>/dev/null || true
in_chroot systemctl enable getty@tty1 2>/dev/null || true

# ---- stage-hub payload — the same three pieces pi-gen ran, same order ----
in_chroot apt-get update
# shellcheck disable=SC2046  # word splitting is the point
in_chroot apt-get install -y $(cat "$STEP_DIR/00-packages")
(cd "$STEP_DIR" && ROOTFS_DIR="$ROOTFS" bash 00-run.sh)
install -m 0755 "$STEP_DIR/01-run-chroot.sh" "$ROOTFS/tmp/01-run-chroot.sh"
in_chroot bash /tmp/01-run-chroot.sh
rm -f "$ROOTFS/tmp/01-run-chroot.sh"

# ---- ide bundle — hubd serves it at /ide/ (see hubd.rs) ----
# Fetched on the CI host (the chroot has DNS but this needs no chroot). The
# release asset is the full built site INCLUDING its vendored deps (Blockly,
# Monaco, mqtt.js, MicroPython-WASM) —
# better-robotics/ide's vendor/ is gitignored (fetched by vendor.sh, not
# committed), so a plain source tarball would ship a broken page.
curl -fsSL https://github.com/better-robotics/ide/releases/latest/download/ide-dist.tar.gz \
  -o /tmp/ide-dist.tar.gz
install -d "$ROOTFS/usr/share/hub/ide"
tar -xzf /tmp/ide-dist.tar.gz -C "$ROOTFS/usr/share/hub/ide"
rm -f /tmp/ide-dist.tar.gz

# Offline appliance: the apt lists our `apt-get update` fetched are dead
# weight in the shipped image (the Pi can't apt install anyway).
rm -rf "$ROOTFS"/var/lib/apt/lists/*

restore_chroot_files
umount -R "$ROOTFS"
# Zero the freed blocks so the grow-slack and purged packages compress away.
zerofree "${LOOP}p2"
losetup -d "$LOOP"
trap - EXIT
echo "✓ customized: $IMG"
