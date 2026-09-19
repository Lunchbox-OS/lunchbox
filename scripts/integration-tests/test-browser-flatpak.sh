#!/usr/bin/env bash
# Manual real-Chrome test for the supervised-browser activity.
#
# Verifies the two assumptions only real Chrome can confirm:
#   1. Flatpak Chrome reads the managed-policy JSON at the path lunchbox writes
#      it to (`~/.var/app/com.google.Chrome/config/chromium/policies/managed/`),
#      so the URL allow/blocklist is actually enforced.
#   2. `--user-data-dir` lands where lunchbox later wipes the profile.
#
# It drives `cargo test -p lunchbox-host-linux` against the real
# `com.google.Chrome` flatpak. HOME is redirected to a tempdir inside the test
# so the user's real Chrome config is never touched; the test self-skips if the
# flatpak isn't installed.
#
# Prerequisites:
#   - flatpak + a working flathub remote
#   - com.google.Chrome installed for the current user:
#       flatpak install -y flathub com.google.Chrome
#   - `timeout` (coreutils) on PATH

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

APP_ID="${LUNCHBOX_CHROME_FLATPAK:-com.google.Chrome}"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}
require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command '$1' not found on PATH"
}

require_command cargo
require_command flatpak
require_command timeout

echo "[orchestrator] Verifying preconditions..."
if ! flatpak info "$APP_ID" >/dev/null 2>&1; then
    fail "flatpak '$APP_ID' is not installed.
       Run: flatpak install -y flathub $APP_ID
       (or set LUNCHBOX_CHROME_FLATPAK to another Chromium-based flatpak id)"
fi

echo "[orchestrator] Running cargo test (real Chrome, $APP_ID)..."
LUNCHBOX_CHROME_FLATPAK="$APP_ID" \
    cargo test -p lunchbox-host-linux --lib -- \
    --ignored --nocapture --test-threads=1 \
    browser::tests::real_flatpak_chrome_enforces_policy_and_user_data_dir

echo "[orchestrator] Done."
