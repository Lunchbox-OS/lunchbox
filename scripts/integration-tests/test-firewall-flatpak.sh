#!/usr/bin/env bash
# Manual end-to-end firewall enforcement test for Flatpak entries.
#
# Builds a tiny "com.lunchbox-os.firewall.Probe" flatpak from this repo with
# flatpak-builder, installs it user-scoped, then drives `cargo test
# -p lunchbox-e2e --test firewall_real_flatpak`. Same shape as
# test-firewall-snap.sh but for the flatpak path of
# `apply_firewall_to_existing_scope`.
#
# Prerequisites:
#   - lunchbox-firewall-helper installed (run setup-firewall-dev.sh)
#   - The user a member of the lunchbox-firewall group
#   - polkit rule loaded
#   - flatpak + flatpak-builder + a working flathub remote
#   - org.freedesktop.{Platform,Sdk}//24.08 installed:
#       flatpak --user install -y flathub \
#           org.freedesktop.Platform//24.08 org.freedesktop.Sdk//24.08
#   - Outbound to the deny target

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

HELPER_PATH="/usr/libexec/lunchbox-firewall-helper"
POLKIT_ACTION="com.lunchbox-os.firewall.apply-process"
DENY_TARGET="${LUNCHBOX_INTEGRATION_DENY_TARGET:-8.8.8.8:53}"
APP_ID="com.lunchbox-os.firewall.Probe"
RUNTIME_VERSION="24.08"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}
require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command '$1' not found on PATH"
}

require_command cargo
require_command pkcheck
require_command flatpak
require_command flatpak-builder

echo "[orchestrator] Verifying preconditions..."
[[ -x "$HELPER_PATH" ]] \
    || fail "$HELPER_PATH not installed. Run sudo ./scripts/integration-tests/setup-firewall-dev.sh"

if ! pkcheck --action-id "$POLKIT_ACTION" --process $$ >/dev/null 2>&1; then
    fail "polkit denies $POLKIT_ACTION; run setup-firewall-dev.sh and re-login."
fi

if ! flatpak --user info "org.freedesktop.Platform//$RUNTIME_VERSION" >/dev/null 2>&1; then
    fail "flatpak runtime org.freedesktop.Platform//$RUNTIME_VERSION not installed.
       Run: flatpak --user install -y flathub org.freedesktop.Platform//$RUNTIME_VERSION org.freedesktop.Sdk//$RUNTIME_VERSION"
fi
if ! flatpak --user info "org.freedesktop.Sdk//$RUNTIME_VERSION" >/dev/null 2>&1; then
    fail "flatpak SDK org.freedesktop.Sdk//$RUNTIME_VERSION not installed (needed by flatpak-builder)."
fi

echo "[orchestrator] Pre-flight: $DENY_TARGET reachable from outside firewall..."
deny_host="${DENY_TARGET%:*}"
deny_port="${DENY_TARGET##*:}"
if ! timeout 3 bash -c "exec 3<>/dev/tcp/$deny_host/$deny_port" 2>/dev/null; then
    fail "deny target $DENY_TARGET unreachable. Set LUNCHBOX_INTEGRATION_DENY_TARGET to something reachable but outside the entry's allow list."
fi
exec 3<&- || true

# ----- Stage the flatpak build dir -----------------------------------------
BUILD_DIR="$(mktemp -d /tmp/lunchbox-fw-flatpak.XXXXXX)"
PROBE_LOG_DIR="$(mktemp -d /tmp/lunchbox-fw-flatpak-log.XXXXXX)"
chmod 0777 "$PROBE_LOG_DIR"

cleanup() {
    set +e
    flatpak --user uninstall -y "$APP_ID" >/dev/null 2>&1
    rm -rf "$BUILD_DIR" "$PROBE_LOG_DIR"
}
trap cleanup EXIT

cp "$REPO_ROOT/scripts/integration-tests/run-firewall-probe.sh" "$BUILD_DIR/probe.sh"
chmod 0755 "$BUILD_DIR/probe.sh"

cat > "$BUILD_DIR/$APP_ID.yaml" <<EOF
app-id: $APP_ID
runtime: org.freedesktop.Platform
runtime-version: '$RUNTIME_VERSION'
sdk: org.freedesktop.Sdk
command: probe.sh
finish-args:
  - --share=network
  # /tmp inside the sandbox is private by default even with --filesystem=host.
  # The probe writes its result log under /tmp; expose it explicitly.
  - --filesystem=/tmp
modules:
  - name: probe
    buildsystem: simple
    build-commands:
      - install -D -m 755 probe.sh /app/bin/probe.sh
    sources:
      - type: file
        path: probe.sh
EOF

echo "[orchestrator] Building flatpak via flatpak-builder..."
( cd "$BUILD_DIR" && flatpak-builder --user --install --force-clean build-tree "$APP_ID.yaml" ) 2>&1 | tail -3

echo "[orchestrator] Building lunchbox binaries..."
./scripts/lunchbox build

echo "[orchestrator] Running cargo test..."
LUNCHBOX_FIREWALL_PROBE_LOG="$PROBE_LOG_DIR/probe.log" \
LUNCHBOX_FIREWALL_PROBE_DENY="$DENY_TARGET" \
LUNCHBOX_FIREWALL_PROBE_FLATPAK="$APP_ID" \
    cargo test -p lunchbox-e2e --test firewall_real_flatpak -- \
    --include-ignored --test-threads=1 --nocapture
