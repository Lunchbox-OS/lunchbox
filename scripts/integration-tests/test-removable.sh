#!/usr/bin/env bash
# The file routes against a real FAT drive.
#
# Drives `cargo test -p lunchbox-http --test files_removable`, which talks to
# whatever is mounted at /media/shepherd-fat. Every other test in that crate
# runs on a tempdir -- that is, on ext4 -- and three bugs lived in the gap
# between what the protocol assumes and what FAT actually promises:
# two-second timestamps, a capacity under the free-space floor, and a small
# alphabet for filenames.
#
# Prerequisites:
#   sudo ./scripts/integration-tests/setup-removable-dev.sh
#
# If the mounts are missing the underlying tests print `[SKIP]` and pass, which
# is the expected outcome on CI; this orchestrator checks first so you see the
# reason without reading the test output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

MOUNT="${SHEPHERD_TEST_FAT_MOUNT:-/media/shepherd-fat}"
MOUNT_RO="${SHEPHERD_TEST_FAT_MOUNT_RO:-/media/shepherd-fat-ro}"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

command -v cargo >/dev/null 2>&1 || fail "cargo not found on PATH"

echo "[orchestrator] Verifying the drives are mounted..."
for point in "$MOUNT" "$MOUNT_RO"; do
    mountpoint -q "$point" \
        || fail "$point is not a mount point.
       Run sudo ./scripts/integration-tests/setup-removable-dev.sh
       (an unmounted directory there would let the tests pass for the wrong reason)."
    fstype="$(findmnt -n -o FSTYPE "$point")"
    [[ "$fstype" == vfat || "$fstype" == exfat || "$fstype" == msdos ]] \
        || fail "$point is $fstype, not FAT. The point of this test is the filesystem."
done

[[ -w "$MOUNT" ]] || fail "$MOUNT is not writable by $(id -un). Re-run the setup script."
[[ -w "$MOUNT_RO" ]] && fail "$MOUNT_RO is writable; it is supposed to be read-only."

echo "[orchestrator] $MOUNT   $(findmnt -n -o FSTYPE,SIZE "$MOUNT")"
echo "[orchestrator] $MOUNT_RO $(findmnt -n -o FSTYPE,SIZE "$MOUNT_RO") (ro)"
echo

# --test-threads=1 because the tests share one drive, and one of them waits out
# a two-second timestamp tick that the others would otherwise disturb.
exec cargo test -p lunchbox-http --test files_removable -- \
    --include-ignored --test-threads=1 --nocapture
