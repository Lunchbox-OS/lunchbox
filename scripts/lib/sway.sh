#!/usr/bin/env bash
# Sway compositor helpers for Lunchbox
# Handles nested sway execution for development and production

# Get the directory containing this script
SWAY_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source common utilities
# shellcheck source=common.sh
source "$SWAY_LIB_DIR/common.sh"

# Source build utilities for binary paths
# shellcheck source=build.sh
source "$SWAY_LIB_DIR/build.sh"

# PID of the running sway process (for cleanup)
SWAY_PID=""

# Default directories
DEFAULT_DEV_RUNTIME="./dev-runtime"
DEFAULT_DATA_DIR="$DEFAULT_DEV_RUNTIME/data"
DEFAULT_SOCKET_PATH="$DEFAULT_DEV_RUNTIME/lunchbox.sock"

# Remove leftover wayland-N / sway-ipc.<uid>.<pid>.sock files in
# $XDG_RUNTIME_DIR whose owner is no longer running. A new nested sway picks
# the lowest free wayland-N slot via wl_display_add_socket_auto, but a leaked
# socket *file* with no listener will still trip up clients (and any shell
# whose WAYLAND_DISPLAY happens to be set to that name will see "Connection
# refused" on the next launch). This is what cleans up after a previous
# nested sway that was SIGKILL'd or whose parent died before the trap fired.
sway_purge_stale_sockets() {
    local rt="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    [[ -d "$rt" ]] || return 0

    # sway IPC sockets encode the PID -- check the PID and unlink if dead.
    local sock pid
    for sock in "$rt"/sway-ipc.*.sock; do
        [[ -e "$sock" ]] || continue
        pid="${sock##*ipc.*([0-9]).}"; pid="${pid%.sock}"
        if [[ -n "$pid" && ! -d "/proc/$pid" ]]; then
            info "Removing stale sway IPC socket: $sock (pid $pid gone)"
            rm -f "$sock"
        fi
    done

    # Wayland sockets don't encode PID. Probe each wayland-N (N>=1) by
    # connecting; if no listener, treat as stale and unlink the socket and
    # its lock file. Skip wayland-0 -- that's usually the host compositor.
    local n path
    for n in $(seq 1 32); do
        path="$rt/wayland-$n"
        [[ -e "$path" ]] || continue
        if ! python3 -c "import socket,sys; s=socket.socket(socket.AF_UNIX); s.settimeout(0.2)
try: s.connect(sys.argv[1])
except OSError: sys.exit(1)" "$path" 2>/dev/null; then
            info "Removing stale wayland socket: $path (no listener)"
            rm -f "$path" "$path.lock"
        fi
    done
}

# Cleanup function for sway processes. Runs from the EXIT/TERM/INT/HUP trap;
# `extglob` is needed for the IPC-socket pattern in sway_purge_stale_sockets.
sway_cleanup() {
    info "Cleaning up sway session..."

    # Kill the nested sway - this will clean up everything inside it
    if [[ -n "${SWAY_PID:-}" ]]; then
        kill "$SWAY_PID" 2>/dev/null || true
        # Give libwayland's atexit handler a moment to remove the socket
        # files cleanly before we sweep them ourselves.
        for _ in $(seq 1 20); do
            kill -0 "$SWAY_PID" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$SWAY_PID" 2>/dev/null || true
    fi

    # Explicitly kill any lunchbox processes that might have escaped
    pkill -x "lunchboxd" 2>/dev/null || true
    pkill -x "lunchbox-launcher" 2>/dev/null || true
    pkill -x "lunchbox-hud" 2>/dev/null || true
    pkill -x "lunchbox-media" 2>/dev/null || true

    # Remove socket
    if [[ -n "${LUNCHBOX_SOCKET:-}" ]]; then
        rm -f "$LUNCHBOX_SOCKET"
    fi

    # Sweep any wayland/sway-ipc sockets the kill above didn't unlink.
    shopt -s extglob
    sway_purge_stale_sockets
}

# Kill any existing dev instances
sway_kill_existing() {
    info "Cleaning up any existing dev instances..."
    kill_matching "sway -c.*sway.conf"
    pkill -x "lunchboxd" 2>/dev/null || true
    pkill -x "lunchbox-launcher" 2>/dev/null || true
    pkill -x "lunchbox-hud" 2>/dev/null || true
    pkill -x "lunchbox-media" 2>/dev/null || true

    # Remove stale lunchboxd IPC socket
    if [[ -n "${LUNCHBOX_SOCKET:-}" ]] && [[ -e "$LUNCHBOX_SOCKET" ]]; then
        rm -f "$LUNCHBOX_SOCKET"
    fi

    # Self-heal from prior unclean exits (orphaned wayland-N / sway-ipc
    # socket files left when sway or its parent shell were SIGKILL'd).
    shopt -s extglob
    sway_purge_stale_sockets

    # Brief pause to allow cleanup
    sleep 0.5
}

# Set up environment for lunchbox binaries
sway_setup_env() {
    local data_dir="${1:-$DEFAULT_DATA_DIR}"
    local socket_path="${2:-$DEFAULT_SOCKET_PATH}"

    # Create directories
    mkdir -p "$data_dir"

    # Export environment variables
    export LUNCHBOX_SOCKET="$socket_path"
    export LUNCHBOX_DATA_DIR="$data_dir"

    # Make the debug binaries findable on PATH so lunchboxd can spawn
    # activities that reference them by name (e.g. `lunchbox-media`).
    # `config.example.toml` uses bare command names so the same config works
    # both in dev and after `lunchbox install bins`. Without this, lunchboxd
    # exec()s lunchbox-media and gets ENOENT.
    local repo_root
    repo_root="$(get_repo_root)"
    local debug_bin_dir="$repo_root/target/debug"
    case ":${PATH:-}:" in
        *":$debug_bin_dir:"*) ;;
        *) export PATH="$debug_bin_dir:${PATH:-}" ;;
    esac
}

# Generate a sway config for development
# Uses debug binaries and development paths
sway_generate_dev_config() {
    local repo_root
    repo_root="$(get_repo_root)"
    
    # Use the existing sway.conf as template
    local sway_config="$repo_root/sway.conf"
    
    if [[ ! -f "$sway_config" ]]; then
        die "sway.conf not found at $sway_config"
    fi
    
    # Return path to the sway config (we use the existing one for dev)
    echo "$sway_config"
}

# Start a nested sway session for development
sway_start_nested() {
    local sway_config="$1"
    
    require_command sway

    info "Starting nested sway session..."

    # Cleanup trap. Trapping SIGTERM/SIGINT/SIGHUP in addition to EXIT is
    # essential: bash's default action on those signals is to terminate
    # *without* running the EXIT trap, which is exactly what was leaking
    # nested-sway wayland sockets when the integration test (or anything
    # else) sent SIGTERM to ./run-dev.
    # shellcheck disable=SC2317,SC2329  # invoked indirectly via `trap` below
    sway_handle_signal() {
        # Re-trap to no-op so we don't recurse if the cleanup itself takes
        # a signal, then exit -- this triggers the EXIT trap.
        trap - EXIT TERM INT HUP
        sway_cleanup
        exit 130
    }
    trap sway_cleanup EXIT
    trap sway_handle_signal TERM INT HUP

    # Start sway with wayland backend (nested in current session).
    # WLR_SCENE_DISABLE_DIRECT_SCANOUT=1 forces the compositor to always
    # composite, even for fullscreen surfaces. A fullscreen activity would
    # otherwise direct-scan-out and starve wl-mirror's screencopy of frames,
    # blacking the mirrored external display (issue #87).
    WLR_BACKENDS=wayland WLR_LIBINPUT_NO_DEVICES=1 WLR_SCENE_DISABLE_DIRECT_SCANOUT=1 \
        sway -c "$sway_config" --unsupported-gpu &
    SWAY_PID=$!

    info "Sway started with PID $SWAY_PID"

    # Wait for sway to exit
    wait "$SWAY_PID"
}

# Ensure the example lunchbox-media library is in place at the path
# `config.example.toml` references (`~/.config/lunchbox/movies.toml`).
#
# `./run-dev` boots lunchboxd against the in-repo `config.example.toml`, which
# uses `~/.config/lunchbox/movies.toml` for its media entries. lunchboxd
# expands `~` to the dev user's $HOME at exec time, so without setup that
# path is missing and the media activities fail to launch. We mirror what
# `install_config` does on the install path: drop the example library on
# first run, then leave it alone so the dev's edits stick.
sway_ensure_dev_media_library() {
    local repo_root
    repo_root="$(get_repo_root)"

    local source_library="$repo_root/movies-library.example.toml"
    if [[ ! -f "$source_library" ]]; then
        return 0
    fi

    local user_config_dir="$HOME/.config/lunchbox"
    local dst_library="$user_config_dir/movies.toml"

    if [[ -f "$dst_library" ]]; then
        return 0
    fi

    mkdir -p "$user_config_dir"
    cp "$source_library" "$dst_library"
    info "Installed example media library at $dst_library (edit URIs before use)"
}

# Run a development session (build + nested sway)
sway_dev_run() {
    local repo_root
    repo_root="$(get_repo_root)"

    verify_repo
    cd "$repo_root" || die "Failed to change directory to $repo_root"

    # Set up environment
    sway_setup_env "$DEFAULT_DATA_DIR" "$DEFAULT_SOCKET_PATH"

    # Kill any existing instances
    sway_kill_existing

    # Build debug binaries
    info "Building lunchbox binaries..."
    build_cargo false

    # Make sure the library file referenced from config.example.toml exists.
    sway_ensure_dev_media_library

    # Get sway config
    local sway_config
    sway_config="$(sway_generate_dev_config)"

    # Start nested sway (blocking)
    sway_start_nested "$sway_config"
}

# Main sway command dispatcher (internal use)
sway_main() {
    local subcmd="${1:-}"
    shift || true
    
    case "$subcmd" in
        run)
            sway_dev_run "$@"
            ;;
        *)
            # Default to run for backwards compatibility
            sway_dev_run "$@"
            ;;
    esac
}
