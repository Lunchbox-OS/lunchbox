#!/usr/bin/env bash
# Orchestrates the real Android (Waydroid) end-to-end test.
#
# Stands up a nested headless Sway, starts a Waydroid session attached to it,
# then runs the `waydroid_real` adapter test (which launches an Android app,
# waits for the window, and stops it). Cleans everything up afterwards.
#
# Prerequisites:
#   - Waydroid installed + initialized (see docs/ai/history/2026-06-28 002 ...).
#   - The helper installed + the user in the shepherd-waydroid group:
#       sudo ./scripts/integration-tests/setup-waydroid-dev.sh
#     (re-login is NOT required here: the test binary is launched via `sudo -u`,
#      which re-initialises the group set.)
#   - `sway` available; passwordless `sudo` for the container start.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HELPER="/usr/libexec/shepherd-waydroid-helper"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
USER_NAME="$(id -un)"

skip() { echo "[SKIP] test-waydroid: $*"; exit 0; }
info() { echo "[INFO] $*"; }

command -v waydroid >/dev/null 2>&1 || skip "waydroid not installed"
command -v sway >/dev/null 2>&1 || skip "sway not installed"

cd "$REPO_ROOT"

SWAY_CONF="$(mktemp)"
SWAY_PID=""
SESSION_LOG="$(mktemp)"

cleanup() {
    info "Cleaning up..."
    waydroid session stop >/dev/null 2>&1 || true
    [[ -n "$SWAY_PID" ]] && kill "$SWAY_PID" >/dev/null 2>&1 || true
    rm -f "$SWAY_CONF" "$SESSION_LOG"
}
trap cleanup EXIT

# Build the test binary up front as the current user (toolchain on PATH).
info "Building waydroid_real test..."
cargo test -p shepherd-host-linux --test waydroid_real --no-run >/dev/null 2>&1
TEST_BIN="$(find target/debug/deps -maxdepth 1 -name 'waydroid_real-*' -type f -executable -printf '%T@ %p\n' \
    | sort -rn | head -1 | cut -d' ' -f2-)"
[[ -n "$TEST_BIN" ]] || skip "could not locate the built test binary"

# Ensure the (root) container service is up.
if [[ -x "$HELPER" ]]; then
    info "Starting container via helper..."
    sudo "$HELPER" preboot || true
else
    sudo systemctl start waydroid-container || true
fi

# Nested headless sway with the production fullscreen-match rule.
cat >"$SWAY_CONF" <<'EOF'
default_border none
for_window [app_id="^waydroid\..*"] fullscreen enable
output HEADLESS-1 resolution 1280x800
EOF
info "Starting nested headless sway..."
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 XDG_RUNTIME_DIR="$RUNTIME_DIR" \
    sway -c "$SWAY_CONF" >/dev/null 2>&1 &
SWAY_PID=$!
sleep 3

SWAYSOCK="$(ls -t "$RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null | head -1 || true)"
WAYLAND_DISPLAY="$(ls -t "$RUNTIME_DIR"/wayland-* 2>/dev/null | grep -v '\.lock$' | head -1 | xargs -r basename || true)"
[[ -n "$SWAYSOCK" && -n "$WAYLAND_DISPLAY" ]] || skip "nested sway did not come up"
info "Nested sway: SWAYSOCK=$SWAYSOCK WAYLAND_DISPLAY=$WAYLAND_DISPLAY"

# Start a Waydroid session attached to the nested sway and wait for ready.
info "Starting Waydroid session..."
WAYLAND_DISPLAY="$WAYLAND_DISPLAY" XDG_RUNTIME_DIR="$RUNTIME_DIR" SWAYSOCK="$SWAYSOCK" \
    waydroid session start >"$SESSION_LOG" 2>&1 &
for _ in $(seq 1 90); do
    grep -q "Android with user 0 is ready" "$SESSION_LOG" && break
    sleep 2
done
grep -q "Android with user 0 is ready" "$SESSION_LOG" || skip "Waydroid session did not become ready"
info "Waydroid session ready."

# Run the test binary via `sudo -u` so the shepherd-waydroid group is effective
# (exercising the force_stop -> pkexec -> helper reclaim path too).
info "Running waydroid_real..."
sudo -u "$USER_NAME" env \
    XDG_RUNTIME_DIR="$RUNTIME_DIR" \
    SWAYSOCK="$SWAYSOCK" \
    WAYLAND_DISPLAY="$WAYLAND_DISPLAY" \
    SHEPHERD_WAYDROID_HELPER="$HELPER" \
    "$TEST_BIN" --ignored --nocapture --test-threads=1
