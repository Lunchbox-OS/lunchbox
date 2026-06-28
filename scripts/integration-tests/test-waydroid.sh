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

wait_session_ready() {
    for _ in $(seq 1 90); do
        grep -q "Android with user 0 is ready" "$SESSION_LOG" && return 0
        sleep 2
    done
    return 1
}

# Optionally force multi_windows=false up front so the preboot test exercises
# its set-and-restart branch (the prop is persistent, so it's otherwise already
# true on a host that's run this before). This adds an extra Android boot, so
# it's opt-in via WAYDROID_TEST_FORCE_RESTART=1 — heavy/slow on constrained VMs.
if [[ "${WAYDROID_TEST_FORCE_RESTART:-0}" == "1" ]]; then
    info "Forcing multi_windows=false (to exercise the preboot restart branch)..."
    WAYLAND_DISPLAY="$WAYLAND_DISPLAY" XDG_RUNTIME_DIR="$RUNTIME_DIR" SWAYSOCK="$SWAYSOCK" \
        waydroid session start >"$SESSION_LOG" 2>&1 &
    wait_session_ready || skip "Waydroid session did not become ready"
    waydroid prop set persist.waydroid.multi_windows false
    waydroid session stop >/dev/null 2>&1 || true
    sleep 2
fi

run_test() {
    local name="$1"
    info "Running $name..."
    sudo -u "$USER_NAME" env \
        XDG_RUNTIME_DIR="$RUNTIME_DIR" \
        SWAYSOCK="$SWAYSOCK" \
        WAYLAND_DISPLAY="$WAYLAND_DISPLAY" \
        SHEPHERD_WAYDROID_HELPER="$HELPER" \
        "$TEST_BIN" "$name" --ignored --exact --nocapture --test-threads=1
}

# 1. preboot test: verifies preboot_waydroid brings the container + session up
#    and flips multi_windows (set + restart). The session it starts is held by a
#    child of this short-lived test process, so it dies when the test exits —
#    fine here (in production shepherdd is long-lived and the session persists).
run_test waydroid_preboot_enables_multi_window
waydroid session stop >/dev/null 2>&1 || true
sleep 2

# 2. launch/stop test against an orchestrator-managed long-lived session
#    (multi_windows is now persistently true, set by preboot above).
info "Starting a long-lived session for the launch test..."
WAYLAND_DISPLAY="$WAYLAND_DISPLAY" XDG_RUNTIME_DIR="$RUNTIME_DIR" SWAYSOCK="$SWAYSOCK" \
    waydroid session start >"$SESSION_LOG" 2>&1 &
wait_session_ready || skip "Waydroid session did not become ready"
run_test waydroid_launch_and_stop
