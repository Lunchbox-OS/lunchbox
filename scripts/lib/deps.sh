#!/usr/bin/env bash
# Dependency management for shepherd-launcher
# Provides functions to read, union, and install package sets

# Get the directory containing this script
DEPS_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source common utilities
# shellcheck source=common.sh
source "$DEPS_LIB_DIR/common.sh"
# yt-dlp install/upgrade lives in the shared admin lib so `deps install run` and
# `shepherd-admin yt-dlp install` share one implementation.
# shellcheck source=admin.sh
source "$DEPS_LIB_DIR/admin.sh"

# Directory containing package lists
DEPS_DIR="$(get_repo_root)/scripts/deps"

# Rust installation URL
RUSTUP_URL="https://sh.rustup.rs"

# Android SDK location and the command-line-tools bundle used to bootstrap
# sdkmanager. The SDK lives outside any user home so it can be shared and
# so apt never touches it. Versions track what the companion-android
# Gradle project (companion-android/) targets; bump together.
ANDROID_SDK_ROOT="/opt/android-sdk"
ANDROID_CMDLINE_TOOLS_URL="https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip"
# The NDK is required for the shepherd-media-android cdylib, which cargo-ndk
# cross-compiles for aarch64-linux-android. Keep this in sync with the version
# the crate is validated against (see crates/shepherd-media-android/README.md);
# sdkmanager installs it under $ANDROID_SDK_ROOT/ndk/$ANDROID_NDK_VERSION, which
# is what cargo-ndk finds via ANDROID_NDK_HOME.
ANDROID_NDK_VERSION="27.2.12479018"
# Components sdkmanager installs. compileSdk / build-tools must match
# companion-android/build.gradle.kts.
# Rust targets cargo-ndk cross-compiles the shepherd-media-android cdylib for:
# arm64-v8a (aarch64) and armeabi-v7a (armv7). Installed alongside cargo-ndk in
# the android set so the CI image bakes them in.
ANDROID_RUST_TARGETS=(
    "aarch64-linux-android"
    "armv7-linux-androideabi"
)
ANDROID_SDK_PACKAGES=(
    "platform-tools"
    "platforms;android-35"
    "build-tools;35.0.0"
    "ndk;${ANDROID_NDK_VERSION}"
)

# yt-dlp install/upgrade + its venv constants (YTDLP_VENV/YTDLP_LINK,
# is_ytdlp_installed, install_ytdlp) now live in scripts/lib/admin.sh, sourced
# above. deps_install/deps_check below call them for the run/dev sets.

# Check if Rust is installed
is_rust_installed() {
    command_exists rustc && command_exists cargo
}

# Install Rust via rustup
install_rust() {
    if is_rust_installed; then
        info "Rust is already installed ($(rustc --version))"
        return 0
    fi
    
    info "Installing Rust via rustup..."
    
    # Download and run rustup installer
    curl --proto '=https' --tlsv1.2 -sSf "$RUSTUP_URL" | sh -s -- -y --profile minimal
    
    # Source cargo env for current session
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env" 2>/dev/null || true
    
    if is_rust_installed; then
        success "Rust installed successfully ($(rustc --version))"
    else
        die "Rust installation failed. Please run: curl --proto '=https' --tlsv1.2 -sSf $RUSTUP_URL | sh"
    fi
}

# Install the BPF authoring toolchain: nightly Rust (for `-Zbuild-std`),
# rust-src (so the BPF crate can build core for bpfel-unknown-none), and
# `bpf-linker` (which crates/shepherd-firewall-bpf uses to emit the
# cgroup_skb program object embedded into shepherd-firewall-helper).
# Idempotent: each step skips if already satisfied.
install_bpf_toolchain() {
    # Make sure rustup/cargo are on PATH if install_rust just placed them.
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env" 2>/dev/null || true

    if ! command_exists rustup; then
        die "rustup not on PATH; install_rust must succeed before install_bpf_toolchain"
    fi

    if rustup toolchain list 2>/dev/null | grep -q '^nightly-'; then
        info "Rust nightly toolchain already installed"
    else
        info "Installing Rust nightly toolchain (with rust-src)..."
        rustup toolchain install nightly --component rust-src --profile minimal
    fi

    if command_exists bpf-linker; then
        info "bpf-linker already installed ($(bpf-linker --version 2>/dev/null || echo '?'))"
    else
        info "Installing bpf-linker (cargo install, ~1-2 min on first build)..."
        # llvm-sys 201.x looks for $LLVM_SYS_201_PREFIX; Ubuntu's llvm-20-dev
        # puts everything under /usr/lib/llvm-20.
        LLVM_SYS_201_PREFIX=/usr/lib/llvm-20 cargo install bpf-linker
    fi
}

# Check whether the Android SDK (sdkmanager + a platform + the NDK) is present.
is_android_sdk_installed() {
    [[ -x "$ANDROID_SDK_ROOT/cmdline-tools/latest/bin/sdkmanager" ]] \
        && [[ -d "$ANDROID_SDK_ROOT/platforms/android-35" ]] \
        && [[ -d "$ANDROID_SDK_ROOT/ndk/$ANDROID_NDK_VERSION" ]]
}

# Download the command-line tools and use sdkmanager to install the SDK
# components the companion-android app builds against, plus the NDK the
# shepherd-media-android cdylib cross-compiles with. Idempotent: skips
# the cmdline-tools download when already extracted and re-runs
# sdkmanager (which no-ops for already-installed packages).
#
# The SDK is installed under $ANDROID_SDK_ROOT (owned by the invoking
# user via sudo chown) so Gradle can write to it without root.
install_android_sdk() {
    local cmdline_dir="$ANDROID_SDK_ROOT/cmdline-tools"
    local sdkmanager="$cmdline_dir/latest/bin/sdkmanager"

    if ! command_exists java; then
        die "java not found; the 'android' apt packages must install first"
    fi

    maybe_sudo mkdir -p "$ANDROID_SDK_ROOT"
    # Hand the tree to the current user so Gradle/sdkmanager need no sudo.
    maybe_sudo chown -R "$(id -u):$(id -g)" "$ANDROID_SDK_ROOT"

    if [[ ! -x "$sdkmanager" ]]; then
        info "Downloading Android command-line tools..."
        local tmp_zip
        tmp_zip="$(mktemp --suffix=.zip)"
        curl -fSL "$ANDROID_CMDLINE_TOOLS_URL" -o "$tmp_zip"
        # The zip extracts to a top-level `cmdline-tools/` dir; sdkmanager
        # insists on living at `cmdline-tools/latest/`, so stage and move.
        local tmp_extract
        tmp_extract="$(mktemp -d)"
        unzip -q "$tmp_zip" -d "$tmp_extract"
        mkdir -p "$cmdline_dir"
        rm -rf "$cmdline_dir/latest"
        mv "$tmp_extract/cmdline-tools" "$cmdline_dir/latest"
        rm -rf "$tmp_zip" "$tmp_extract"
    else
        info "Android command-line tools already present"
    fi

    info "Accepting Android SDK licenses..."
    # `yes` receives SIGPIPE (exit 141) when sdkmanager closes stdin after
    # reading enough confirmations; under `set -o pipefail` that 141 would
    # abort the whole script even though sdkmanager succeeded. Disable
    # pipefail just for this pipeline. A genuine license/SDK failure still
    # surfaces at the package-install step and the final validation below.
    set +o pipefail
    yes | "$sdkmanager" --sdk_root="$ANDROID_SDK_ROOT" --licenses >/dev/null
    set -o pipefail

    info "Installing Android SDK packages: ${ANDROID_SDK_PACKAGES[*]}"
    "$sdkmanager" --sdk_root="$ANDROID_SDK_ROOT" "${ANDROID_SDK_PACKAGES[@]}"

    if is_android_sdk_installed; then
        success "Android SDK installed at $ANDROID_SDK_ROOT"
        info "Export ANDROID_SDK_ROOT=$ANDROID_SDK_ROOT (companion-android/local.properties also points here)"
        info "Export ANDROID_NDK_HOME=$ANDROID_SDK_ROOT/ndk/$ANDROID_NDK_VERSION for the cargo-ndk cross-compile"
    else
        die "Android SDK installation failed — check sdkmanager output above"
    fi
}

# Install cargo-ndk + the Android Rust targets used to cross-compile the
# shepherd-media-android cdylib. Part of the `android` set so the Android CI
# image (built FROM the base image, which already has Rust) bakes them in — the
# release/CI jobs then don't install cargo-ndk at runtime.
#
# Requires Rust: skipped with a note when cargo is absent, so `deps install
# android` still works standalone for the Kotlin-only companion app. On a host
# that builds shepherd-media-android, run `deps install build` first.
install_cargo_ndk() {
    if ! command_exists cargo; then
        warn "cargo not found; skipping cargo-ndk + Android Rust targets."
        warn "Run 'shepherd deps install build' first to build shepherd-media-android."
        return 0
    fi

    info "Adding Android Rust targets: ${ANDROID_RUST_TARGETS[*]}"
    rustup target add "${ANDROID_RUST_TARGETS[@]}"

    if command_exists cargo-ndk; then
        info "cargo-ndk already installed ($(cargo-ndk --version 2>/dev/null || echo '?'))"
    else
        info "Installing cargo-ndk (cargo install, ~1-2 min on first build)..."
        cargo install cargo-ndk
    fi
}

# Read a package file, stripping comments and empty lines
read_package_file() {
    local file="$1"
    
    if [[ ! -f "$file" ]]; then
        die "Package file not found: $file"
    fi
    
    # Strip comments (# to end of line) and empty lines, trim whitespace
    grep -v '^\s*#' "$file" | grep -v '^\s*$' | sed 's/#.*//' | tr -s '[:space:]' '\n' | grep -v '^$'
}

# Get packages for a specific set
get_packages() {
    local set_name="$1"
    
    case "$set_name" in
        build)
            read_package_file "$DEPS_DIR/build.pkgs"
            ;;
        run)
            read_package_file "$DEPS_DIR/run.pkgs"
            ;;
        test)
            read_package_file "$DEPS_DIR/test.pkgs"
            ;;
        android)
            read_package_file "$DEPS_DIR/android.pkgs"
            ;;
        agent)
            read_package_file "$DEPS_DIR/agent.pkgs"
            ;;
        dev)
            # Union of build + run + test + agent + dev extras, deduplicated.
            # `agent` is included so a dev checkout can drive `shepherd dev
            # headless` (grim/wtype/jq) out of the box.
            {
                read_package_file "$DEPS_DIR/build.pkgs"
                read_package_file "$DEPS_DIR/run.pkgs"
                read_package_file "$DEPS_DIR/test.pkgs"
                read_package_file "$DEPS_DIR/agent.pkgs"
                read_package_file "$DEPS_DIR/dev.pkgs"
            } | sort -u
            ;;
        *)
            die "Unknown package set: $set_name (valid: build, run, test, android, agent, dev)"
            ;;
    esac
}

# Print packages for a set (one per line)
deps_print() {
    local set_name="${1:-}"
    
    if [[ -z "$set_name" ]]; then
        die "Usage: shepherd deps print <build|run|dev>"
    fi
    
    get_packages "$set_name"
}

# Install packages for a set
deps_install() {
    local set_name="${1:-}"
    
    if [[ -z "$set_name" ]]; then
        die "Usage: shepherd deps install <build|run|dev>"
    fi
    
    check_ubuntu_version
    
    info "Installing $set_name dependencies..."
    
    # Get the package list
    local packages
    packages=$(get_packages "$set_name" | tr '\n' ' ')
    
    if [[ -z "$packages" ]]; then
        warn "No packages to install for set: $set_name"
        return 0
    fi
    
    info "Packages: $packages"
    
    # Install using apt
    maybe_sudo apt-get update
    # shellcheck disable=SC2086
    maybe_sudo apt-get install -y $packages
    
    # For build and dev sets, also install Rust + the BPF authoring
    # toolchain (needed by crates/shepherd-firewall-bpf).
    if [[ "$set_name" == "build" ]] || [[ "$set_name" == "dev" ]]; then
        install_rust
        install_bpf_toolchain
    fi

    # For run and dev sets, install yt-dlp into its virtualenv.
    # This runs after apt so that python3-venv is already present.
    if [[ "$set_name" == "run" ]] || [[ "$set_name" == "dev" ]]; then
        install_ytdlp
    fi

    # For the android set, fetch the SDK after the JDK + unzip apt
    # packages are present, then cargo-ndk + the Android Rust targets.
    if [[ "$set_name" == "android" ]]; then
        install_android_sdk
        install_cargo_ndk
    fi

    success "Installed $set_name dependencies"
}

# Check if all packages for a set are installed
deps_check() {
    local set_name="${1:-}"
    
    if [[ -z "$set_name" ]]; then
        die "Usage: shepherd deps check <build|run|dev>"
    fi
    
    local packages
    packages=$(get_packages "$set_name")
    
    local missing=()
    while IFS= read -r pkg; do
        if ! dpkg -l "$pkg" &>/dev/null; then
            missing+=("$pkg")
        fi
    done <<< "$packages"
    
    # For build and dev sets, also check Rust.
    if [[ "$set_name" == "build" ]] || [[ "$set_name" == "dev" ]]; then
        if ! is_rust_installed; then
            warn "Rust is not installed"
            return 1
        fi
    fi

    # For run and dev sets, also check yt-dlp.
    if [[ "$set_name" == "run" ]] || [[ "$set_name" == "dev" ]]; then
        if ! is_ytdlp_installed; then
            warn "yt-dlp is not installed (run: shepherd deps install run)"
            return 1
        fi
    fi

    # For the android set, also check the SDK and (when Rust is present) cargo-ndk.
    if [[ "$set_name" == "android" ]]; then
        if ! is_android_sdk_installed; then
            warn "Android SDK is not installed (run: shepherd deps install android)"
            return 1
        fi
        if command_exists cargo && ! command_exists cargo-ndk; then
            warn "cargo-ndk is not installed (run: shepherd deps install android)"
            return 1
        fi
    fi
    
    if [[ ${#missing[@]} -gt 0 ]]; then
        warn "Missing packages: ${missing[*]}"
        return 1
    fi
    
    success "All $set_name dependencies are installed"
    return 0
}

# Main deps command dispatcher
deps_main() {
    local subcmd="${1:-}"
    shift || true
    
    case "$subcmd" in
        print)
            deps_print "$@"
            ;;
        install)
            deps_install "$@"
            ;;
        check)
            deps_check "$@"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd deps <command> <set>

Commands:
    print   <set>    Print packages in the set (one per line)
    install <set>    Install packages from the set
    check   <set>    Check if all packages from the set are installed

Package sets:
    build    Build-time dependencies (+ Rust via rustup)
    run      Runtime dependencies only
    test     Extra packages needed for the shepherd-e2e harness
    android  JDK + Android SDK + NDK for the companion-android and
             shepherd-media-android apps
    agent    Headless-dev tooling for 'shepherd dev headless' (grim/wtype/jq)
    dev      All dependencies (build + run + test + agent + dev extras + Rust)

Note: The 'build' and 'dev' sets automatically install Rust via rustup.
      The 'android' set is standalone (not part of 'dev') because it
      downloads the Android SDK + NDK into /opt/android-sdk.

Examples:
    shepherd deps print build
    shepherd deps install dev
    shepherd deps install android
    shepherd deps check run
EOF
            ;;
        *)
            die "Unknown deps command: $subcmd (try: shepherd deps help)"
            ;;
    esac
}
