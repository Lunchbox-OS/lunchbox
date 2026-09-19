#!/usr/bin/env bash
# Manual end-to-end firewall enforcement test for Snap entries.
#
# Builds a tiny "lunchbox-firewall-probe" snap from this repo, installs it
# via `snap try` (classic confinement), then drives
# `cargo test -p lunchbox-e2e --test firewall_real_snap` which boots a real
# lunchboxd, configures an entry with kind=snap, and waits for the snap's
# systemd scope to appear. lunchboxd then invokes
# `lunchbox-firewall-helper apply-cgroup` (via pkexec) which attaches a
# cgroup_skb BPF program to the scope. The probe inside the snap reports
# allow=OPEN deny=BLOCKED.
#
# Prerequisites (same as test-firewall.sh, plus snap):
#   - lunchbox-firewall-helper installed (run setup-firewall-dev.sh)
#   - The user a member of the lunchbox-firewall group (re-login required)
#   - polkit running and the rule loaded
#   - snapd installed and running
#   - Outbound connectivity to the deny target (Google DNS)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

HELPER_PATH="/usr/libexec/lunchbox-firewall-helper"
POLKIT_ACTION="com.lunchbox-os.firewall.apply-process"
DENY_TARGET="${LUNCHBOX_INTEGRATION_DENY_TARGET:-8.8.8.8:53}"
SNAP_NAME="lunchbox-firewall-probe"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}
require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command '$1' not found on PATH"
}

require_command cargo
require_command pkcheck
require_command snap
require_command sudo

echo "[orchestrator] Verifying preconditions..."
[[ -x "$HELPER_PATH" ]] \
    || fail "$HELPER_PATH not installed. Run sudo ./scripts/integration-tests/setup-firewall-dev.sh"

if ! pkcheck --action-id "$POLKIT_ACTION" --process $$ >/dev/null 2>&1; then
    fail "polkit denies $POLKIT_ACTION for this user.
       Run sudo ./scripts/integration-tests/setup-firewall-dev.sh and re-login."
fi

echo "[orchestrator] Pre-flight: $DENY_TARGET reachable from outside firewall..."
deny_host="${DENY_TARGET%:*}"
deny_port="${DENY_TARGET##*:}"
if ! timeout 3 bash -c "exec 3<>/dev/tcp/$deny_host/$deny_port" 2>/dev/null; then
    fail "deny target $DENY_TARGET unreachable. Set LUNCHBOX_INTEGRATION_DENY_TARGET to something reachable but outside the entry's allow list."
fi
exec 3<&- || true

# ----- Stage the snap try directory ----------------------------------------
SNAP_DIR="$(mktemp -d /tmp/lunchbox-fw-snap.XXXXXX)"
PROBE_LOG_DIR="$(mktemp -d /tmp/lunchbox-fw-snap-log.XXXXXX)"
# `snap try` requires the dir tree to be world-readable + executable; mktemp
# defaults to 0700.
chmod 0755 "$SNAP_DIR"
chmod 0777 "$PROBE_LOG_DIR"
export LUNCHBOX_FIREWALL_PROBE_LOG="$PROBE_LOG_DIR/probe.log"
PROBE_LOG_PATH="$LUNCHBOX_FIREWALL_PROBE_LOG"

cleanup() {
    set +e
    sudo snap remove --purge "$SNAP_NAME" >/dev/null 2>&1
    rm -rf "$SNAP_DIR" "$PROBE_LOG_DIR"
}
trap cleanup EXIT

mkdir -p "$SNAP_DIR/meta" "$SNAP_DIR/bin"
cp "$REPO_ROOT/scripts/integration-tests/run-firewall-probe.sh" "$SNAP_DIR/bin/probe.sh"
chmod 0755 "$SNAP_DIR/bin/probe.sh"

cat > "$SNAP_DIR/meta/snap.yaml" <<EOF
name: $SNAP_NAME
version: '1.0'
summary: Test snap for lunchbox-launcher firewall enforcement
description: |
  Probes one allowed and one denied TCP target inside the snap's systemd
  scope. Used by crates/lunchbox-e2e/tests/firewall_real_snap.rs only.
confinement: classic
grade: stable
apps:
  $SNAP_NAME:
    command: bin/probe.sh
EOF

echo "[orchestrator] Installing snap via 'sudo snap try --classic $SNAP_DIR'..."
sudo snap try --classic "$SNAP_DIR"

echo "[orchestrator] Building lunchbox binaries..."
./scripts/lunchbox build

echo "[orchestrator] Running cargo test..."
LUNCHBOX_FIREWALL_PROBE_LOG="$PROBE_LOG_PATH" \
LUNCHBOX_FIREWALL_PROBE_DENY="$DENY_TARGET" \
LUNCHBOX_FIREWALL_PROBE_SNAP="$SNAP_NAME" \
    cargo test -p lunchbox-e2e --test firewall_real_snap -- \
    --include-ignored --test-threads=1 --nocapture
