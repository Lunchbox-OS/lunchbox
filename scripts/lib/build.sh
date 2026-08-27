#!/usr/bin/env bash
# Build logic for shepherd-launcher
# Wraps cargo build with project-specific settings

# Get the directory containing this script
BUILD_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source common utilities
# shellcheck source=common.sh
source "$BUILD_LIB_DIR/common.sh"

# Binary names produced by the build
SHEPHERD_BINARIES=(
    "shepherdd"
    "shepherd-launcher"
    "shepherd-hud"
    "shepherd-media"
    "shepherd-pairing-display"
    "shepherd-touch-bridge"
    "shepherd-tablet-bridge"
    "shepherd-gamepad-bridge"
)

# Get the target directory for binaries
get_target_dir() {
    local release="${1:-false}"
    local repo_root
    repo_root="$(get_repo_root)"
    
    if [[ "$release" == "true" ]]; then
        echo "$repo_root/target/release"
    else
        echo "$repo_root/target/debug"
    fi
}

# Get the path to a specific binary
get_binary_path() {
    local binary="$1"
    local release="${2:-false}"
    
    echo "$(get_target_dir "$release")/$binary"
}

# Check if all binaries exist
binaries_exist() {
    local release="${1:-false}"
    local target_dir
    target_dir="$(get_target_dir "$release")"
    
    for binary in "${SHEPHERD_BINARIES[@]}"; do
        if [[ ! -x "$target_dir/$binary" ]]; then
            return 1
        fi
    done
    return 0
}

# Build the config editor's wasm validator into shepherd-webui/src/config/wasm.
#
# Must run before the npm build, which imports it. The editor runs the real
# `shepherd_config` parser and validator rather than a TypeScript
# reimplementation, so this artifact is a build input, not an optimization.
build_config_wasm() {
    local repo_root
    repo_root="$(get_repo_root)"

    if [[ ! -d "$repo_root/crates/shepherd-config-wasm" ]]; then
        warn "shepherd-config-wasm crate not found; skipping wasm build"
        return 0
    fi

    # shellcheck source=/dev/null
    source "$HOME/.cargo/env" 2>/dev/null || true
    require_command wasm-pack

    info "Building config editor wasm validator..."
    cd "$repo_root" || die "Failed to change directory to $repo_root"
    wasm-pack build --target web --release \
        --out-dir ../../shepherd-webui/src/config/wasm \
        --out-name shepherd_config \
        crates/shepherd-config-wasm \
        || die "wasm-pack build failed"
    success "Config editor wasm built"
}

# Build the web UI (must run before cargo so rust-embed picks up dist/)
build_webui() {
    local repo_root
    repo_root="$(get_repo_root)"
    local webui_dir="$repo_root/shepherd-webui"

    if [[ ! -d "$webui_dir" ]]; then
        warn "shepherd-webui directory not found; skipping web UI build"
        return 0
    fi

    require_command npm

    build_config_wasm

    info "Building web UI..."
    cd "$webui_dir" || die "Failed to change directory to $webui_dir"

    if [[ ! -d node_modules ]]; then
        info "Installing npm dependencies..."
        npm install
    fi

    npm run build
    success "Web UI built"
    cd "$repo_root" || die "Failed to return to repo root"
}

# Build the standalone config editor bundle for static hosting. Separate from
# build_webui because it writes a different directory: `dist/` is what
# rust-embed compiles into shepherdd, and the standalone bundle must not land
# there.
build_config_editor() {
    local repo_root
    repo_root="$(get_repo_root)"
    local webui_dir="$repo_root/shepherd-webui"

    require_command npm

    build_config_wasm

    info "Building standalone config editor..."
    cd "$webui_dir" || die "Failed to change directory to $webui_dir"
    if [[ ! -d node_modules ]]; then
        info "Installing npm dependencies..."
        npm install
    fi
    npm run build:standalone
    success "Config editor built (shepherd-webui/dist-standalone/)"
    cd "$repo_root" || die "Failed to return to repo root"
}

# Run the rsbuild dev server for the web UI or the standalone config editor.
#
# Foreground and hot-reloading; Ctrl-C stops it. The management UI target
# proxies /api to localhost:8080, so shepherdd still has to be running
# separately for anything that talks to the daemon; the standalone config
# editor target needs no daemon at all.
dev_webui() {
    local repo_root
    repo_root="$(get_repo_root)"
    local webui_dir="$repo_root/shepherd-webui"
    local target="embedded"
    local rebuild_wasm=false
    local -a passthrough=()

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --standalone|--config-editor)
                target="standalone"
                shift
                ;;
            --wasm)
                rebuild_wasm=true
                shift
                ;;
            help|-h|--help)
                cat <<EOF
Usage: shepherd dev webui [--standalone] [--wasm] [-- <rsbuild args>]

Runs the rsbuild dev server in the foreground with hot reload.

Options:
    --standalone    Serve only the config editor, the way a static host would.
                    Needs no daemon. Without this, serves the management UI,
                    which proxies /api to localhost:8080.
    --wasm          Rebuild the config editor's wasm validator first. It is
                    built automatically when missing; use this after changing
                    crates/shepherd-config-wasm.

Anything after -- is passed to rsbuild, e.g. --port 3001.

Examples:
    shepherd dev webui
    shepherd dev webui --standalone
    shepherd dev webui --wasm -- --port 3001
EOF
                return 0
                ;;
            --)
                shift
                passthrough+=("$@")
                break
                ;;
            *)
                die "Unknown dev webui option: $1 (try: shepherd dev webui help)"
                ;;
        esac
    done

    [[ -d "$webui_dir" ]] || die "shepherd-webui directory not found"
    require_command npm

    # The config editor is imported by both targets — lazily in the management
    # UI, directly in the standalone one — so rspack resolves its wasm module
    # either way and the dev server fails without it.
    if [[ "$rebuild_wasm" == "true" ]] || [[ ! -d "$webui_dir/src/config/wasm" ]]; then
        build_config_wasm
    fi

    cd "$webui_dir" || die "Failed to change directory to $webui_dir"

    if [[ ! -d node_modules ]]; then
        info "Installing npm dependencies..."
        npm install
    fi

    if [[ "$target" == "standalone" ]]; then
        info "Starting the config editor dev server (no daemon needed)..."
        npm run dev:standalone -- "${passthrough[@]}"
    else
        info "Starting the management UI dev server (proxying /api to localhost:8080)..."
        npm run dev -- "${passthrough[@]}"
    fi
}

# Build the project
build_cargo() {
    local release="${1:-false}"
    local repo_root
    repo_root="$(get_repo_root)"
    
    verify_repo
    require_command cargo rust

    build_webui

    cd "$repo_root" || die "Failed to change directory to $repo_root"

    local build_type
    if [[ "$release" == "true" ]]; then
        build_type="release"
        info "Building shepherd (release mode)..."
        cargo build --release
    else
        build_type="debug"
        info "Building shepherd (debug mode)..."
        cargo build
    fi
    
    # Verify binaries were created
    if ! binaries_exist "$release"; then
        die "Build completed but some binaries are missing"
    fi
    
    local target_dir
    target_dir="$(get_target_dir "$release")"
    
    success "Built binaries ($build_type):"
    for binary in "${SHEPHERD_BINARIES[@]}"; do
        info "  $target_dir/$binary"
    done
}

# Clean build artifacts
build_clean() {
    local repo_root
    repo_root="$(get_repo_root)"
    
    verify_repo
    require_command cargo rust
    
    cd "$repo_root" || die "Failed to change directory to $repo_root"
    
    info "Cleaning build artifacts..."
    cargo clean
    success "Build artifacts cleaned"
}

# Main build command dispatcher
build_main() {
    local release=false
    
    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --release|-r)
                release=true
                shift
                ;;
            clean)
                build_clean
                return
                ;;
            config-editor)
                # The standalone static bundle. Not part of the normal build:
                # `dist/` is what ships inside shepherdd, and this writes
                # `dist-standalone/` for a static host instead.
                build_config_editor
                return
                ;;
            config-wasm)
                build_config_wasm
                return
                ;;
            help|-h|--help)
                cat <<EOF
Usage: shepherd build [OPTIONS]

Options:
    --release, -r    Build in release mode (optimized)
    clean            Clean build artifacts
    config-editor    Build the standalone config editor into dist-standalone/
    config-wasm      Build only the config editor's wasm validator
    help             Show this help

Examples:
    shepherd build                  # Debug build
    shepherd build --release        # Release build
    shepherd build clean            # Clean artifacts
    shepherd build config-editor    # Static bundle for a web host
EOF
                return
                ;;
            *)
                die "Unknown build option: $1 (try: shepherd build help)"
                ;;
        esac
    done
    
    build_cargo "$release"
}
