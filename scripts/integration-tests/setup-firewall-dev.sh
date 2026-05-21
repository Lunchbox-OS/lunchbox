#!/usr/bin/env bash
# One-time dev setup for the firewall helper.
#
# Builds the helper (debug profile) and delegates to the main installer
# (`shepherd install firewall --debug`) so dev and production share the
# same install logic. Adds the invoking user to the shepherd-firewall group.
#
# After running with sudo, log out and back in (or open a new sway session)
# so the supplementary group membership takes effect. Local activities with
# `[entries.firewall]` configured will then be enforced; the wiring is also
# covered in CI by the shepherd-e2e firewall tests.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [[ $EUID -ne 0 ]]; then
    echo "ERROR: re-run with sudo. The main install script writes to /usr/libexec, /usr/share, and /etc." >&2
    exit 1
fi

TARGET_USER="${SUDO_USER:-$(logname 2>/dev/null || true)}"
if [[ -z "$TARGET_USER" || "$TARGET_USER" == "root" ]]; then
    echo "ERROR: cannot determine the non-root user. Run with sudo from a regular login." >&2
    exit 1
fi

# Build the debug helper as the calling user, so the binary in target/debug
# isn't owned by root after this script runs. Use `bash -lc` so the user's
# login profile loads ~/.cargo/bin onto PATH (sudo strips it via
# secure_path).
echo "[setup] Building debug helper..."
sudo -u "$TARGET_USER" bash -lc "cd '$REPO_ROOT' && cargo build --bin shepherd-firewall-helper"

# Delegate to the main installer.
exec "$REPO_ROOT/scripts/shepherd" install firewall --user "$TARGET_USER" --debug
