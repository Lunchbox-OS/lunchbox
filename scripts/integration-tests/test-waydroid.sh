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

# Nested headless sway with the production window rules for Waydroid surfaces
# (per-app `waydroid.<pkg>` toplevels for windowed mode, plus the single
# `Waydroid` full-UI surface for the locktask path).
#
# The rules are LIFTED FROM sway.conf rather than restated here. They were a
# hand-written copy once, and it silently drifted: when the locktask path started
# parking the full-UI surface on `__shepherd_parked` before pinning, production
# gained `move container to workspace __shepherd_parked` and this copy did not —
# so `wait_for_parked_full_ui` never saw the surface and the locktask test failed
# with "Waydroid full UI did not appear within 45s", looking exactly like a
# product regression. Extracting them keeps the harness honest for free.
{
    echo "default_border none"
    grep -E '^for_window \[app_id="(\^waydroid|Waydroid)' "$REPO_ROOT/sway.conf"
    echo "output HEADLESS-1 resolution 1280x800"
} >"$SWAY_CONF"
info "Waydroid window rules under test:"
grep '^for_window' "$SWAY_CONF" | sed 's/^/  /'
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

# 1b. cold-container prop correction. Runs here because it needs the running
#     session test 1 just left (to poison the prop through) and then goes cold
#     itself. Leaves multi_windows=true and a running session again.
run_test waydroid_preboot_fixes_stale_props_on_a_cold_container

waydroid session stop >/dev/null 2>&1 || true
sleep 2

# 2. launch/stop test against an orchestrator-managed long-lived session
#    (multi_windows is now persistently true, set by preboot above).
info "Starting a long-lived session for the launch test..."
WAYLAND_DISPLAY="$WAYLAND_DISPLAY" XDG_RUNTIME_DIR="$RUNTIME_DIR" SWAYSOCK="$SWAYSOCK" \
    waydroid session start >"$SESSION_LOG" 2>&1 &
wait_session_ready || skip "Waydroid session did not become ready"
run_test waydroid_launch_and_stop

# The launch above applied lock-down via the adapter (default on) ->
# waydroid::lock_down -> pkexec helper -> `cmd statusbar send-disable-flag`.
# The flags are system-wide and persist after the app stopped, so check them.
info "Verifying kiosk lock-down (statusbar disable flags)..."
DIS="$(sudo waydroid --details-to-stdout shell dumpsys statusbar 2>/dev/null \
    | grep -o 'mDisabled1=0x[0-9a-fA-F]*' | head -1)"
if [[ -n "$DIS" && "$DIS" != "mDisabled1=0x0" ]]; then
    info "[OK] lock-down active: $DIS (shade/nav disabled)"
else
    echo "[WARN] lock-down flags not detected ($DIS)"
fi

# 3. locktask test: the DPC device-owner Lock Task path (`lock_mode =
#    "locktask"`). It presents the single full-UI `Waydroid` surface, so restart
#    the session with multi_windows OFF first. The test skips if the DPC isn't
#    set as device owner (`shepherd-admin apps install android`).
info "Starting a full-UI session for the locktask test (multi_windows off)..."
# Set the prop BEFORE stopping the session. `waydroid prop set` reaches the
# property service *through the session* and silently exits 0 when none is
# running, so setting it after the stop below did nothing: multi_windows stayed
# true, the test's own preboot then had to restart the session to turn it off,
# and a session started from the test binary's minimal environment can't resolve
# the third-party DPC activity ("Activity class does not exist"). The pin never
# landed and the failure surfaced three layers away as "Lock Task never LOCKED".
waydroid prop set persist.waydroid.multi_windows false
if [[ "$(waydroid prop get persist.waydroid.multi_windows | tr -d '[:space:]\r')" != "false" ]]; then
    skip "could not set multi_windows=false — is a session running? (prop set is a silent no-op without one)"
fi
waydroid session stop >/dev/null 2>&1 || true
sleep 2
: >"$SESSION_LOG"  # truncate so wait_session_ready sees the NEW ready line
WAYLAND_DISPLAY="$WAYLAND_DISPLAY" XDG_RUNTIME_DIR="$RUNTIME_DIR" SWAYSOCK="$SWAYSOCK" \
    waydroid session start >"$SESSION_LOG" 2>&1 &
wait_session_ready || skip "Waydroid session did not become ready"
run_test waydroid_locktask_launch_and_stop

# 4. shutdown test: stop_waydroid() must leave a session it didn't start alone,
#    and stop one it prebooted. Runs last — it deliberately ends with the
#    session STOPPED, which is also what cleanup would do anyway. Reuses the
#    locktask session above as the "started by someone else" case.
run_test waydroid_stop_on_shutdown

# The suite ends with the session stopped and `persist.waydroid.multi_windows`
# left FALSE (the locktask step set it). Nothing here can put it back: the prop
# service needs a running session. Say so, because the next run starts with
# test 1 asserting preboot leaves multi_windows TRUE — and preboot can't set it
# either on a cold container, so that run fails after a 4-minute poll for
# reasons that have nothing to do with the code under test.
warn "Leaving persist.waydroid.multi_windows=false (no session left to set it through)."
info "Before the next cold run, restore it with a session up:"
info "  waydroid session start &   # in a Wayland session"
info "  waydroid prop set persist.waydroid.multi_windows true"
