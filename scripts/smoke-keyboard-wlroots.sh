#!/usr/bin/env bash
# Headless connectivity smoke test for the wlroots swipe-keyboard backend.
#
# Starts sway with the headless backend, runs the keyboard client against it for a few
# seconds, and asserts the client loads its bundle, binds zwp_input_method_manager_v2, and
# renders without crashing. This validates the Wayland integration; it does NOT yet inject a
# synthesized swipe into a focused text field (see the crate README).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

command -v sway >/dev/null 2>&1 || die "sway is required for the smoke test"

BIN="${KEYBOARD_BIN:-$REPO_ROOT/target/debug/shepherd-keyboard-wlroots}"
[[ -x "$BIN" ]] || die "build first: cargo build -p shepherd-keyboard-wlroots (looked for $BIN)"

BUNDLE_DIR="${SHEPHERD_SWIPE_BUNDLE_DIR:-$REPO_ROOT/dev-runtime/swipe-bundles}"
[[ -d "$BUNDLE_DIR/adult" ]] || die "no bundles; run scripts/fetch-swipe-bundles.sh first"

WORK="$(mktemp -d)"
export XDG_RUNTIME_DIR="$WORK/xdg"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
: > "$WORK/sway.conf"

export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman LIBGL_ALWAYS_SOFTWARE=1

sway -c "$WORK/sway.conf" > "$WORK/sway.log" 2>&1 &
SWAY_PID=$!
cleanup() { kill "$SWAY_PID" 2>/dev/null || true; wait "$SWAY_PID" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT

sock=""
for _ in $(seq 1 40); do
    sock="$(ls "$XDG_RUNTIME_DIR" 2>/dev/null | grep -E '^wayland-[0-9]+$' | head -1 || true)"
    [[ -n "$sock" ]] && break
    sleep 0.25
done
[[ -n "$sock" ]] || { cat "$WORK/sway.log"; die "sway did not create a socket"; }
info "sway up on WAYLAND_DISPLAY=$sock"

WAYLAND_DISPLAY="$sock" SHEPHERD_SWIPE_BUNDLE_DIR="$BUNDLE_DIR" RUST_LOG=info \
    timeout 3 "$BIN" --profile adult > "$WORK/kbd.log" 2>&1 || true

cat "$WORK/kbd.log"
grep -q "loaded signed bundle" "$WORK/kbd.log" || die "client did not load the bundle"
if grep -q "not advertised" "$WORK/kbd.log"; then
    die "client failed to bind zwp_input_method_manager_v2"
fi
grep -qiE "error|panic" "$WORK/kbd.log" && die "client logged an error"
success "smoke test passed: bundle loaded, input-method bound, no crash"
