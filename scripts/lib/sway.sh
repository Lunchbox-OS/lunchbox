#!/usr/bin/env bash
# Sway compositor helpers for shepherd-launcher
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
DEFAULT_SOCKET_PATH="$DEFAULT_DEV_RUNTIME/shepherd.sock"

# Cleanup function for sway processes
sway_cleanup() {
    info "Cleaning up sway session..."
    
    # Kill the nested sway - this will clean up everything inside it
    if [[ -n "${SWAY_PID:-}" ]]; then
        kill "$SWAY_PID" 2>/dev/null || true
    fi
    
    # Explicitly kill any shepherd processes that might have escaped
    pkill -x "shepherdd" 2>/dev/null || true
    pkill -x "shepherd-launcher" 2>/dev/null || true
    pkill -x "shepherd-hud" 2>/dev/null || true
    pkill -x "shepherd-media" 2>/dev/null || true

    # Remove socket
    if [[ -n "${SHEPHERD_SOCKET:-}" ]]; then
        rm -f "$SHEPHERD_SOCKET"
    fi
}

# Kill any existing dev instances
sway_kill_existing() {
    info "Cleaning up any existing dev instances..."
    kill_matching "sway -c.*sway.conf"
    pkill -x "shepherdd" 2>/dev/null || true
    pkill -x "shepherd-launcher" 2>/dev/null || true
    pkill -x "shepherd-hud" 2>/dev/null || true
    pkill -x "shepherd-media" 2>/dev/null || true
    
    # Remove stale socket if it exists
    if [[ -n "${SHEPHERD_SOCKET:-}" ]] && [[ -e "$SHEPHERD_SOCKET" ]]; then
        rm -f "$SHEPHERD_SOCKET"
    fi
    
    # Brief pause to allow cleanup
    sleep 0.5
}

# Set up environment for shepherd binaries
sway_setup_env() {
    local data_dir="${1:-$DEFAULT_DATA_DIR}"
    local socket_path="${2:-$DEFAULT_SOCKET_PATH}"

    # Create directories
    mkdir -p "$data_dir"

    # Export environment variables
    export SHEPHERD_SOCKET="$socket_path"
    export SHEPHERD_DATA_DIR="$data_dir"

    # Make the debug binaries findable on PATH so shepherdd can spawn
    # activities that reference them by name (e.g. `shepherd-media`).
    # `config.example.toml` uses bare command names so the same config works
    # both in dev and after `shepherd install bins`. Without this, shepherdd
    # exec()s shepherd-media and gets ENOENT.
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
    
    # Set up cleanup trap
    trap sway_cleanup EXIT
    
    # Start sway with wayland backend (nested in current session)
    WLR_BACKENDS=wayland WLR_LIBINPUT_NO_DEVICES=1 sway -c "$sway_config" --unsupported-gpu &
    SWAY_PID=$!
    
    info "Sway started with PID $SWAY_PID"
    
    # Wait for sway to exit
    wait "$SWAY_PID"
}

# Ensure the example shepherd-media library is in place at the path
# `config.example.toml` references (`~/.config/shepherd/movies.toml`).
#
# `./run-dev` boots shepherdd against the in-repo `config.example.toml`, which
# uses `~/.config/shepherd/movies.toml` for its media entries. shepherdd
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

    local user_config_dir="$HOME/.config/shepherd"
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
    info "Building shepherd binaries..."
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
