#!/usr/bin/env bash
# Manual end-to-end firewall enforcement test.
#
# Drives `cargo test -p lunchbox-e2e --test firewall_real`, which boots a
# real lunchboxd, launches an activity through the privileged
# lunchbox-firewall-helper, and verifies the BPF address filter is actually
# enforced (loopback reachable; an external host blocked).
#
# Prerequisites on the host:
#   - lunchbox-firewall-helper installed at /usr/libexec/lunchbox-firewall-helper
#   - The invoking user a member of the lunchbox-firewall group
#   - polkit running and the rule loaded
#   - Outbound connectivity to the deny target (Google DNS, 8.8.8.8:53)
#   - The lunchbox-e2e runtime deps (sway, dbus-daemon, etc.)
#
# If any of those is missing the underlying cargo test prints a clear
# `[SKIP]` line and exits 0; this orchestrator pre-checks the same things
# so you can see the failure without waiting for sway to boot.
#
# First-time setup:
#   sudo ./scripts/integration-tests/setup-firewall-dev.sh
#   # log out + back in for the new group membership to take effect

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

HELPER_PATH="/usr/libexec/lunchbox-firewall-helper"
POLKIT_ACTION="com.lunchbox-os.firewall.apply-process"
DENY_TARGET="${LUNCHBOX_INTEGRATION_DENY_TARGET:-8.8.8.8:53}"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command '$1' not found on PATH"
}

require_command cargo
require_command pkcheck
require_command bash
require_command timeout

echo "[orchestrator] Verifying preconditions..."
[[ -x "$HELPER_PATH" ]] \
    || fail "$HELPER_PATH not installed. Run sudo ./scripts/integration-tests/setup-firewall-dev.sh"

if ! pkcheck --action-id "$POLKIT_ACTION" --process $$ >/dev/null 2>&1; then
    fail "polkit denies $POLKIT_ACTION for this user.
       Run sudo ./scripts/integration-tests/setup-firewall-dev.sh and re-login
       so the lunchbox-firewall group membership takes effect."
fi

echo "[orchestrator] Pre-flight: verifying deny target $DENY_TARGET is reachable from outside the firewall..."
deny_host="${DENY_TARGET%:*}"
deny_port="${DENY_TARGET##*:}"
if ! timeout 3 bash -c "exec 3<>/dev/tcp/$deny_host/$deny_port" 2>/dev/null; then
    fail "deny target $DENY_TARGET is not reachable from outside the firewall.
       The activity's deny check would pass for the wrong reason (no internet
       vs. firewall blocked). Either fix connectivity or set
       LUNCHBOX_INTEGRATION_DENY_TARGET to a host:port that IS reachable here
       but is OUTSIDE the entry's allow list."
fi
exec 3<&- || true

echo "[orchestrator] Building lunchbox binaries..."
./scripts/lunchbox build

echo "[orchestrator] Running cargo test -p lunchbox-e2e --test firewall_real..."
exec cargo test -p lunchbox-e2e --test firewall_real -- \
    --include-ignored --test-threads=1 --nocapture
