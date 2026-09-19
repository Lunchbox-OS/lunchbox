#!/usr/bin/env bash
# Mount two loopback FAT images where the file manager looks for removable
# drives, so `cargo test -p shepherd-http --test files_removable` has something
# real to talk to.
#
# Why images rather than a USB stick: the tests need a drive *smaller than the
# 2 GiB free-space floor* and one mounted *read-only*, and they need both to be
# there the same way on every machine. A 512 MiB image gives the first for free
# and the second by mounting the same bytes twice.
#
#   sudo ./scripts/integration-tests/setup-removable-dev.sh
#   sudo ./scripts/integration-tests/setup-removable-dev.sh --teardown
#
# The mounts do not survive a reboot, which is deliberate: nothing here should
# quietly become part of the machine.

set -euo pipefail

MOUNT="${SHEPHERD_TEST_FAT_MOUNT:-/media/shepherd-fat}"
MOUNT_RO="${SHEPHERD_TEST_FAT_MOUNT_RO:-/media/shepherd-fat-ro}"
IMAGE_DIR="${SHEPHERD_TEST_FAT_IMAGE_DIR:-/var/tmp/shepherd-fat}"
IMAGE="$IMAGE_DIR/removable.img"
IMAGE_RO="$IMAGE_DIR/removable-ro.img"
# Small on purpose: under the 2 GiB free_space_floor_bytes default, which is
# what makes the floor test meaningful.
SIZE_MIB=512

if [[ $EUID -ne 0 ]]; then
    echo "ERROR: re-run with sudo. Mounting under /media needs root." >&2
    exit 1
fi

TARGET_USER="${SUDO_USER:-$(logname 2>/dev/null || true)}"
if [[ -z "$TARGET_USER" || "$TARGET_USER" == "root" ]]; then
    echo "ERROR: cannot determine the non-root user. Run with sudo from a regular login." >&2
    exit 1
fi
TARGET_UID="$(id -u "$TARGET_USER")"
TARGET_GID="$(id -g "$TARGET_USER")"

teardown() {
    for point in "$MOUNT" "$MOUNT_RO"; do
        if mountpoint -q "$point"; then
            echo "[teardown] umount $point"
            umount "$point"
        fi
        [[ -d "$point" ]] && rmdir "$point" 2>/dev/null || true
    done
    if [[ -d "$IMAGE_DIR" ]]; then
        echo "[teardown] rm -rf $IMAGE_DIR"
        rm -rf "$IMAGE_DIR"
    fi
    echo "[teardown] done"
}

if [[ "${1:-}" == "--teardown" ]]; then
    teardown
    exit 0
fi

for tool in mkfs.vfat mountpoint; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: '$tool' not found. Install dosfstools (see scripts/deps/test.pkgs)." >&2
        exit 1
    }
done

# Start from nothing, so a half-finished previous run cannot be mistaken for a
# working one — an *unmounted* /media/shepherd-fat is an ordinary ext4
# directory that the tests would otherwise happily pass against.
teardown >/dev/null 2>&1 || true

mkdir -p "$IMAGE_DIR"
echo "[setup] Creating two ${SIZE_MIB} MiB FAT32 images in $IMAGE_DIR..."
for img in "$IMAGE" "$IMAGE_RO"; do
    truncate -s "${SIZE_MIB}M" "$img"
    # -F 32 explicitly: mkfs.vfat picks FAT16 for an image this small, and the
    # point is to look like the USB stick a person would actually plug in.
    # -n gives it a label, so `/media/<label>` reads like a real drive.
    mkfs.vfat -F 32 -n SHEPHERD "$img" >/dev/null
done

mkdir -p "$MOUNT" "$MOUNT_RO"
# uid/gid so the *test process*, which is not root, can write here: a vfat
# filesystem has no ownership of its own, so the mount decides it.
echo "[setup] Mounting $IMAGE at $MOUNT (writable, uid=$TARGET_UID)..."
mount -o "loop,uid=$TARGET_UID,gid=$TARGET_GID,umask=0022" "$IMAGE" "$MOUNT"

echo "[setup] Mounting $IMAGE_RO at $MOUNT_RO (read-only)..."
mount -o "loop,ro,uid=$TARGET_UID,gid=$TARGET_GID,umask=0022" "$IMAGE_RO" "$MOUNT_RO"

echo
echo "[setup] Mounted:"
grep -E " (${MOUNT//\//\\/}|${MOUNT_RO//\//\\/}) " /proc/mounts || true
echo
echo "[setup] The file manager will now offer these as removable roots."
echo "        Run: ./scripts/integration-tests/test-removable.sh"
echo "        Undo: sudo $0 --teardown"
