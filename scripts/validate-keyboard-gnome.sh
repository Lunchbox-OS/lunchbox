#!/usr/bin/env bash
# Validate the GNOME swipe-keyboard extension on a real GNOME Shell, headless and nested
# (the GNOME analog of scripts/smoke-keyboard-wlroots.sh).
#
# Starts the decode daemon and a nested headless gnome-shell on a private bus, installs and
# enables the extension with its self-test, and asserts every self-test line PASSes: the
# extension loads/enables cleanly, the keyboard actor renders, the commit + virtual-key APIs
# exist, the safety gate forces tap-only for password/sensitive fields, and a gesture decodes
# through the daemon (top candidate "hello") — i.e. parity with the wlroots path.
#
# Usage: scripts/validate-keyboard-gnome.sh [user|gdm]   (default: user)
#
# Not covered (needs an interactive session with input devices + a focused app): a real
# touch-driven swipe committing into an app field, OSK-suppression behavior, focus-driven
# show/hide. See the extension README's manual checklist.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

MODE="${1:-user}"
[[ "$MODE" == user || "$MODE" == gdm ]] || die "mode must be 'user' or 'gdm'"

command -v gnome-shell >/dev/null 2>&1 || die "gnome-shell is required"
command -v dbus-run-session >/dev/null 2>&1 || die "dbus-run-session is required"
command -v gdbus >/dev/null 2>&1 || die "gdbus is required"

DAEMON="${KEYBOARD_DAEMON:-$REPO_ROOT/target/debug/shepherd-keyboard-gnome-daemon}"
[[ -x "$DAEMON" ]] || die "build first: cargo build -p shepherd-keyboard-gnome-daemon"
BUNDLE_DIR="${SHEPHERD_SWIPE_BUNDLE_DIR:-$REPO_ROOT/dev-runtime/swipe-bundles}"
[[ -d "$BUNDLE_DIR/adult" ]] || die "no bundles; run scripts/fetch-swipe-bundles.sh first"
HELLO="$REPO_ROOT/crates/shepherd-keyboard-core/tests/fixtures/hello.gesture.json"

export REPO_ROOT DAEMON BUNDLE_DIR HELLO MODE
dbus-run-session -- bash -euo pipefail -c '
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    export SHEPHERD_SWIPE_BUNDLE_DIR="$BUNDLE_DIR"
    export SHEPHERD_SWIPE_SELFTEST=1
    export SHEPHERD_HELLO="$(cat "$HELLO")"

    ext="$HOME/.local/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com"
    work="$(mktemp -d)"
    rm -rf "$ext"; mkdir -p "$(dirname "$ext")"
    cp -r "$REPO_ROOT/crates/shepherd-keyboard-gnome-daemon/extension" "$ext"
    cleanup() { kill "${DPID:-}" "${SPID:-}" 2>/dev/null || true; wait 2>/dev/null || true; rm -rf "$work" "$ext"; }
    trap cleanup EXIT

    "$DAEMON" --profile adult >"$work/daemon.log" 2>&1 &
    DPID=$!
    # Wait for the daemon to own its bus name before starting the shell.
    for _ in $(seq 1 40); do
        gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
            --method org.freedesktop.DBus.NameHasOwner com.armeafamily.ShepherdSwipe 2>/dev/null | grep -q true && break
        sleep 0.5
    done

    modeflag=""; [ "$MODE" = gdm ] && modeflag="--mode=gdm"
    gnome-shell --headless $modeflag --virtual-monitor 1080x600 \
        --wayland-display="wayland-validate-$MODE" >"$work/shell.log" 2>&1 &
    SPID=$!
    up=""
    for _ in $(seq 1 30); do
        gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
            --method org.freedesktop.DBus.Properties.Get org.gnome.Shell ShellVersion >/dev/null 2>&1 && { up=1; break; }
        sleep 1
    done
    [ -n "$up" ] || { tail -10 "$work/shell.log"; echo "nested gnome-shell did not start" >&2; exit 1; }
    sleep 2
    gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
        --method org.gnome.Shell.Extensions.EnableExtension shepherd-swipe@armeafamily.com >/dev/null 2>&1
    sleep 3

    info_out="$(gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
        --method org.gnome.Shell.Extensions.GetExtensionInfo shepherd-swipe@armeafamily.com 2>&1)"
    echo "$info_out" | tr "," "\n" | grep -iE "'"'"'state'"'"'|'"'"'error'"'"'" || true

    results="$(grep -oE "SHEPHERD-SELFTEST.*" "$work/shell.log" || true)"
    echo "$results"
    [ -n "$results" ] || { echo "no self-test output — extension did not run ($MODE mode)" >&2; exit 1; }
    if echo "$results" | grep -q "FAIL"; then echo "a self-test FAILED ($MODE mode)" >&2; exit 1; fi
'
success "GNOME extension validated on Shell $(gnome-shell --version | grep -oE "[0-9]+" | head -1) ($MODE mode): all self-tests passed"
