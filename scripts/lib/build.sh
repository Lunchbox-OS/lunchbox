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
    # The screen lock administrator mode spawns (issue #154). Like the pairing
    # overlay above it, it is a standalone binary shepherdd starts for one
    # purpose rather than a daemon or a sidecar -- and like it, shepherdd looks
    # for it beside its own executable and then, failing that, on the trusted
    # path (issue #144). Leaving it out of this list left a device where
    # `lock_device` answered "failed to start shepherd-lock: No such file or
    # directory", because the sibling probe finds nothing next to
    # /usr/bin/shepherdd and nothing installed it there either. This list is the
    # only one: `binaries_exist`, `install_bins` and `uninstall_bins` all read
    # it, and the .deb is built by driving install.sh with DESTDIR set.
    "shepherd-lock"
    "shepherd-touch-bridge"
    "shepherd-tablet-bridge"
    "shepherd-gamepad-bridge"
    # Not a daemon or a sidecar: the policy validator, shipped because
    # `install policy` validates before it installs and a packaged device has
    # no source tree to build it from (issue #157). Without it on a device the
    # only route to a policy is an unchecked one, and an unparseable policy is
    # fatal at shepherdd's *startup* -- a session that ends at the next boot,
    # on a device whose kiosk user has no shell to fix it from.
    "shepherd-validate-config"
)

# Rust target triple to build for, or empty for a native build.
#
# Set by `build_set_target` from --target/--arch and read by get_target_dir, so
# that everything which locates a built binary follows a cross build without
# knowing that one is happening. Exported so a re-exec (package deb's fakeroot
# re-entry) keeps it.
export SHEPHERD_CARGO_TARGET="${SHEPHERD_CARGO_TARGET:-}"

# Rust triples that name an ISA revision the GNU type does not.
#
# A GNU type says nothing about which revision of an architecture it means, and
# for one Debian architecture that matters. dpkg-architecture reports
# `arm-linux-gnueabihf` for armhf, which translates to
# `arm-unknown-linux-gnueabihf` -- a real Rust target, so the target-list check
# in arch_to_triple accepts it, but the wrong one. That triple is the ARMv6
# baseline; Debian defines armhf as ARMv7-A hard-float, which Rust spells
# `armv7-unknown-linux-gnueabihf`.
#
# Nothing fails loudly when this is wrong, which is exactly why it is corrected
# here rather than left to be noticed: ARMv6 code runs on an ARMv7 machine, so
# the result is a package labelled armhf whose contents quietly target an older
# ISA than the label promises.
#
# Spelling an arm literal here is fine: check-arch-neutral.sh guards this path
# against hardcoding the *host* architecture, which is the one thing that has to
# be derived at runtime so a package is labelled for the machine it was built
# for. A target architecture named as a target is the opposite of that.
declare -A ARCH_TRIPLE_OVERRIDES=(
    [arm-unknown-linux-gnueabihf]="armv7-unknown-linux-gnueabihf"
)

# Translate a Debian architecture name into its Rust target triple.
#
# Derived with dpkg-architecture rather than a lookup table on purpose: a table
# would have to spell out the arch literals that scripts/ci/check-arch-neutral.sh
# forbids in this path, and it is exactly the sort of thing that goes stale.
# ARCH_TRIPLE_OVERRIDES above then corrects the one case the derivation gets
# wrong while still satisfying rustc, and the result is checked against rustc's
# own target list, so an architecture whose GNU type does not translate at all
# fails here rather than at link time.
arch_to_triple() {
    local arch="$1"
    local gnu triple

    require_command dpkg-architecture
    gnu="$(dpkg-architecture -a"$arch" -qDEB_HOST_GNU_TYPE 2>/dev/null)" \
        || die "Not a Debian architecture: $arch"

    # e.g. aarch64-linux-gnu -> aarch64-unknown-linux-gnu
    triple="${gnu/-linux-/-unknown-linux-}"
    triple="${ARCH_TRIPLE_OVERRIDES[$triple]:-$triple}"

    if ! rustc --print target-list | grep -qx -- "$triple"; then
        die "No Rust target for Debian architecture '$arch' (derived $triple).
Pass --target <triple> explicitly if you know the right one."
    fi
    echo "$triple"
}

# Resolve --target/--arch into SHEPHERD_CARGO_TARGET.
#
# An --arch that names the host architecture stays a native build: it leaves the
# triple unset, so the output paths and the cargo fingerprints are the ones every
# other build on this machine already uses. That makes `--arch $(dpkg
# --print-architecture)` a no-op rather than a second, redundant target
# directory, which is what lets a CI matrix pass --arch on every leg.
# --target always sets the triple, even when it is the host's.
build_set_target() {
    local kind="$1" value="$2"

    case "$kind" in
        target)
            SHEPHERD_CARGO_TARGET="$value"
            ;;
        arch)
            if [[ "$value" == "$(dpkg --print-architecture)" ]]; then
                SHEPHERD_CARGO_TARGET=""
            else
                SHEPHERD_CARGO_TARGET="$(arch_to_triple "$value")"
            fi
            ;;
        *)
            die "build_set_target: unknown kind '$kind'"
            ;;
    esac
    export SHEPHERD_CARGO_TARGET
}

# Get the target directory for binaries
get_target_dir() {
    local release="${1:-false}"
    local repo_root
    repo_root="$(get_repo_root)"

    local profile="debug"
    [[ "$release" == "true" ]] && profile="release"

    # cargo puts a --target build under target/<triple>/ instead of target/.
    if [[ -n "${SHEPHERD_CARGO_TARGET:-}" ]]; then
        echo "$repo_root/target/$SHEPHERD_CARGO_TARGET/$profile"
    else
        echo "$repo_root/target/$profile"
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

# Export the cross-compilation environment for SHEPHERD_CARGO_TARGET.
#
# Only touches variables that are unset, so a CI image that bakes them in wins
# and a developer can override any one of them. Everything is derived from the
# triple, so no architecture is named here.
#
# Nothing is exported for a native build: cargo, pkg-config and cc all do the
# right thing unaided, and pointing PKG_CONFIG_LIBDIR at the host's own
# directory would only be a way to get it wrong.
_build_export_cross_env() {
    local triple="$1"
    local gnu upper

    # aarch64-unknown-linux-gnu -> aarch64-linux-gnu, the multiarch tuple that
    # names both the cross gcc and the sysroot's pkgconfig directory.
    gnu="${triple/-unknown-/-}"
    upper="${triple//-/_}"
    upper="${upper^^}"

    # A cross build of the -sys crates needs pkg-config pointed at the target
    # sysroot. Ubuntu 26.04 ships no pkg-config-<gnu> package (the binary comes
    # from pkgconf-bin now), so set the search path explicitly rather than
    # relying on a per-target wrapper being on PATH.
    export PKG_CONFIG_ALLOW_CROSS="${PKG_CONFIG_ALLOW_CROSS:-1}"
    export PKG_CONFIG_LIBDIR="${PKG_CONFIG_LIBDIR:-/usr/lib/$gnu/pkgconfig:/usr/share/pkgconfig}"
    # A multiarch sysroot is the host root, so the .pc prefixes need no rewrite.
    export PKG_CONFIG_SYSROOT_DIR="${PKG_CONFIG_SYSROOT_DIR:-/}"

    # cargo's linker override for this target. The `cc` crate finds the same
    # compiler off the triple on its own, so this is only for the final link.
    local linker_var="CARGO_TARGET_${upper}_LINKER"
    if [[ -z "${!linker_var:-}" ]]; then
        export "$linker_var=$gnu-gcc"
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

    local -a cargo_args=()
    local build_type
    if [[ "$release" == "true" ]]; then
        build_type="release"
        cargo_args+=(--release)
    else
        build_type="debug"
    fi

    local for_target=""
    if [[ -n "${SHEPHERD_CARGO_TARGET:-}" ]]; then
        # Pass --target on the command line rather than exporting
        # CARGO_BUILD_TARGET: shepherd-firewall-helper's build.rs shells out to
        # a nightly cargo for the sibling BPF crate, and an environment
        # variable would beat that crate's own [build] target and try to build
        # the eBPF program for this triple. (build.rs strips it too, belt and
        # braces -- but the command line never reaches the child at all.)
        cargo_args+=(--target "$SHEPHERD_CARGO_TARGET")
        for_target=" for $SHEPHERD_CARGO_TARGET"

        if [[ "$SHEPHERD_CARGO_TARGET" != "$(rustc -vV | awk '/^host: /{print $2}')" ]]; then
            _build_export_cross_env "$SHEPHERD_CARGO_TARGET"
        fi
    fi

    info "Building shepherd ($build_type mode)$for_target..."
    cargo build "${cargo_args[@]}"

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
            --target)
                [[ -n "${2:-}" ]] || die "--target needs a Rust target triple"
                build_set_target target "$2"
                shift 2
                ;;
            --arch)
                [[ -n "${2:-}" ]] || die "--arch needs a Debian architecture"
                build_set_target arch "$2"
                shift 2
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
    --arch ARCH      Build for a Debian architecture (arm64, …). The host's own
                     architecture builds natively; any other cross-compiles and
                     lands in target/<triple>/, which install and package follow.
                     Needs the cross toolchain: shepherd deps install cross
                     --arch ARCH
    --target TRIPLE  Build for a Rust target triple, always explicitly, even
                     when it is the host's. Lower-level form of --arch.
    clean            Clean build artifacts
    config-editor    Build the standalone config editor into dist-standalone/
    config-wasm      Build only the config editor's wasm validator
    help             Show this help

Examples:
    shepherd build                  # Debug build
    shepherd build --release        # Release build
    shepherd build --arch arm64     # Cross-compile (from another architecture)
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
