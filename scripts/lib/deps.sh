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

# For arch_to_triple: `deps install cross` adds the same Rust target that
# `shepherd build --arch` will ask for, and the two must agree.
# shellcheck source=build.sh
source "$DEPS_LIB_DIR/build.sh"

# Directory containing package lists
DEPS_DIR="$(get_repo_root)/scripts/deps"

# Rust installation URL
RUSTUP_URL="https://sh.rustup.rs"

# bpf-linker is pinned because its LLVM coupling changes between releases,
# and an unpinned `cargo install` silently adopts whatever that coupling
# has become. 0.10.3 defaults to the `rust-llvm-22` feature, which links
# through `aya-rustc-llvm-proxy` against **rustc's own bundled LLVM** — so
# it needs no system LLVM at all, and it matches the toolchain that
# actually compiles the BPF crate (`rustc --version --verbose` reports
# LLVM 22). 0.11.0 dropped the `rust-llvm-*` features entirely and
# defaults to `llvm-23`, i.e. a *system* LLVM 23 that Ubuntu doesn't ship
# yet; picking it up unpinned is what broke the CI image build with
# "could not find llvm-config in directories specified by ... PATH".
#
# Before bumping this: check which LLVM the toolchain bundles
# (`rustc --version --verbose | grep LLVM`) and pick the bpf-linker
# feature that matches it. A mismatch here produces BPF objects the
# kernel verifier rejects, not a build error.
BPF_LINKER_VERSION="0.10.3"

# Android SDK location and the command-line-tools bundle used to bootstrap
# sdkmanager. The SDK lives outside any user home so it can be shared and
# so apt never touches it. Versions track what the companion-android
# Gradle project (companion-android/) targets; bump together.
ANDROID_SDK_ROOT="/opt/android-sdk"
ANDROID_CMDLINE_TOOLS_URL="https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip"
# The NDK is required for the lunchbox-media-android cdylib, which cargo-ndk
# cross-compiles for aarch64-linux-android. Keep this in sync with the version
# the crate is validated against (see crates/lunchbox-media-android/README.md);
# sdkmanager installs it under $ANDROID_SDK_ROOT/ndk/$ANDROID_NDK_VERSION, which
# is what cargo-ndk finds via ANDROID_NDK_HOME.
ANDROID_NDK_VERSION="27.2.12479018"
# cargo-ndk is pinned for the same reason as BPF_LINKER_VERSION: an
# unpinned `cargo install` adopts whatever upstream published last, and a
# cross-compiler driver is a bad place to discover that unattended. This
# is the version the Android CI job has been building with, so pinning it
# changes nothing today — it just stops the toolchain from moving on its
# own. It bridges the Rust targets above to $ANDROID_NDK_VERSION's
# clang/sysroot, so bump it deliberately, alongside the NDK.
CARGO_NDK_VERSION="4.1.2"

# wasm-pack builds crates/lunchbox-config-wasm into the browser artifact the
# web config editor loads. Pinned for the same reason as the tools above: an
# unpinned install silently adopts whatever upstream published last, and the
# generated JS glue has to match the `wasm-bindgen` version the crate compiles
# against.
#
# Installed with `cargo install`, not the upstream `init.sh`: that script has
# its version baked in and ignores any argument you pass it, so pinning through
# it does not work — it would install whatever is current and then fail this
# file's own version check on every run. Building from source also keeps the
# install arch-neutral, which a hardcoded release-tarball URL would not be
# (see scripts/ci/check-arch-neutral.sh).
WASM_PACK_VERSION="0.13.1"
# Components sdkmanager installs. compileSdk / build-tools must match
# companion-android/build.gradle.kts.
# Rust targets cargo-ndk cross-compiles the lunchbox-media-android cdylib for:
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

# Add clippy and rustfmt to the stable toolchain.
#
# install_rust asks rustup for `--profile minimal`, which has neither, and it
# returns early on a host that already has Rust -- so nothing put them back.
# CONTRIBUTING tells developers to run `cargo clippy` and `cargo fmt`, and
# .ci/Dockerfile has carried its own `rustup component add clippy rustfmt` for
# the same reason, which is why CI never noticed.
install_lint_components() {
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env" 2>/dev/null || true
    command_exists rustup || return 0

    local missing=()
    local component
    for component in clippy rustfmt; do
        if ! rustup component list --installed 2>/dev/null | grep -q "^$component"; then
            missing+=("$component")
        fi
    done

    if [[ ${#missing[@]} -eq 0 ]]; then
        info "clippy and rustfmt are already installed"
        return 0
    fi

    info "Adding Rust components: ${missing[*]}..."
    rustup component add "${missing[@]}"
}

# Install the BPF authoring toolchain: nightly Rust (for `-Zbuild-std`),
# rust-src (so the BPF crate can build core for bpfel-unknown-none), and
# `bpf-linker` (which crates/lunchbox-firewall-bpf uses to emit the
# cgroup_skb program object embedded into lunchbox-firewall-helper).
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

    # Match on the exact pinned version, not merely "is it on PATH": a host
    # (or a cached image layer) carrying a different bpf-linker is the
    # failure this pin exists to prevent, and skipping the install because
    # *something* is present would preserve it.
    # Guard the probe: this script runs under `set -euo pipefail`, so on a
    # host without bpf-linker the missing command returns 127, pipefail
    # promotes that to the pipeline's status, the bare assignment inherits
    # it, and `set -e` kills the whole install. That is not hypothetical —
    # it is how this line first reached CI, which died with exit 127 on a
    # fresh image while working on every machine that already had the
    # binary. `command_exists` keeps the probe off the failure path.
    local installed=""
    if command_exists bpf-linker; then
        installed="$(bpf-linker --version 2>/dev/null | awk '{print $2}' || true)"
    fi
    if [[ "$installed" == "$BPF_LINKER_VERSION" ]]; then
        info "bpf-linker $BPF_LINKER_VERSION already installed"
    else
        if [[ -n "$installed" ]]; then
            info "Replacing bpf-linker $installed with pinned $BPF_LINKER_VERSION..."
        else
            info "Installing bpf-linker $BPF_LINKER_VERSION (cargo install, ~1-2 min on first build)..."
        fi
        # --locked: build against the versions upstream released with, so a
        # dependency publishing a breaking change can't fail this the way an
        # unpinned bpf-linker itself did. No LLVM_* variable is set on
        # purpose — see BPF_LINKER_VERSION; this build links rustc's LLVM.
        cargo install bpf-linker --version "$BPF_LINKER_VERSION" --locked --force
    fi
}

# Install the wasm toolchain the web config editor's validator needs: the
# wasm32 target and a pinned wasm-pack. Idempotent — each step skips when
# already satisfied.
install_wasm_toolchain() {
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env" 2>/dev/null || true

    if ! command_exists rustup; then
        die "rustup not on PATH; install_rust must succeed before install_wasm_toolchain"
    fi

    if rustup target list --installed 2>/dev/null | grep -qx "wasm32-unknown-unknown"; then
        info "wasm32-unknown-unknown target already installed"
    else
        info "Installing wasm32-unknown-unknown target..."
        rustup target add wasm32-unknown-unknown
    fi

    # Same guarded probe as bpf-linker: under `set -euo pipefail` a missing
    # command would otherwise return 127 and kill the whole install.
    local installed=""
    if command_exists wasm-pack; then
        installed="$(wasm-pack --version 2>/dev/null | awk '{print $2}' || true)"
    fi
    if [[ "$installed" == "$WASM_PACK_VERSION" ]]; then
        info "wasm-pack $WASM_PACK_VERSION already installed"
    else
        if [[ -n "$installed" ]]; then
            info "Replacing wasm-pack $installed with pinned $WASM_PACK_VERSION..."
        else
            info "Installing wasm-pack $WASM_PACK_VERSION (cargo install, ~1-2 min on first build)..."
        fi
        cargo install wasm-pack --version "$WASM_PACK_VERSION" --locked --force
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
# lunchbox-media-android cdylib cross-compiles with. Idempotent: skips
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
# lunchbox-media-android cdylib. Part of the `android` set so the Android CI
# image (built FROM the base image, which already has Rust) bakes them in — the
# release/CI jobs then don't install cargo-ndk at runtime.
#
# Requires Rust: skipped with a note when cargo is absent, so `deps install
# android` still works standalone for the Kotlin-only companion app. On a host
# that builds lunchbox-media-android, run `deps install build` first.
install_cargo_ndk() {
    if ! command_exists cargo; then
        warn "cargo not found; skipping cargo-ndk + Android Rust targets."
        warn "Run 'shepherd deps install build' first to build lunchbox-media-android."
        return 0
    fi

    info "Adding Android Rust targets: ${ANDROID_RUST_TARGETS[*]}"
    rustup target add "${ANDROID_RUST_TARGETS[@]}"

    # Version-matched rather than "is it on PATH", for the same reason as
    # bpf-linker: a host or cached layer carrying a different build is
    # exactly what the pin is meant to displace.
    #
    # Ask via the *subcommand* form. `cargo-ndk --version` does not report a
    # version — the binary answers "This binary may only be called via
    # `cargo ndk`." and exits 0, so parsing its output yields an empty
    # string and every run would reinstall.
    # `|| true` for the same reason the bpf-linker probe is guarded: under
    # `set -euo pipefail`, `cargo ndk` on a host without cargo-ndk exits
    # non-zero, pipefail propagates it, and the bare assignment would abort
    # the install. command_exists can't help here — the binary is invoked
    # through cargo — so absorb the status instead.
    local ndk_installed
    ndk_installed="$(cargo ndk --version 2>/dev/null | awk '{print $2}' || true)"
    if [[ "$ndk_installed" == "$CARGO_NDK_VERSION" ]]; then
        info "cargo-ndk $CARGO_NDK_VERSION already installed"
    else
        if [[ -n "$ndk_installed" ]]; then
            info "Replacing cargo-ndk $ndk_installed with pinned $CARGO_NDK_VERSION..."
        else
            info "Installing cargo-ndk $CARGO_NDK_VERSION (cargo install, ~1-2 min on first build)..."
        fi
        cargo install cargo-ndk --version "$CARGO_NDK_VERSION" --locked --force
    fi
}

# Fallback mirror, for the case where the configured one does not carry the
# target architecture. Named for its role, not for any architecture, because
# which arches live where is not stable enough to encode: on Ubuntu 26.04 the
# main archive serves arm64 too (verified 2026-09-04 --
# dists/resolute/main/binary-arm64/Release is present and real), so the old
# rule that ports carried arm64 and the archive carried amd64/i386 no longer
# holds. Hardcoding it would add a redundant source and rewrite a working
# sources file for nothing. Hence the probe below.
#
# This is only the right fallback for an architecture the primary archive
# lacks. Ports carries arm64 but not amd64, so cross-compiling towards amd64
# from a host whose only mirror is ports is not something it can rescue -- that
# fails with a clear error rather than adding a source that would not work.
DEPS_PORTS_URI="http://ports.ubuntu.com/ubuntu-ports/"

# Emit one TAB-separated record per deb822 stanza: file, first URI, first
# suite, components, declared architectures.
#
# Per *stanza*, not per file: ubuntu.sources alone carries two (the archive and
# the security mirror), and a host may have any number of PPA and vendor files
# beside it. The previous version read the first `URIs:`/`Suites:` line across
# the whole glob, which is whichever file sorts first -- a PPA on any machine
# that has one -- and then drew conclusions about "the configured mirror" from
# it.
_deps_source_stanzas() {
    local f
    for f in "$@"; do
        awk -v file="$f" '
            function flush() {
                if (uri != "" && suite != "")
                    printf "%s\t%s\t%s\t%s\t%s\n", file, uri, suite, comps, arches
                uri = ""; suite = ""; comps = ""; arches = ""
            }
            /^[[:space:]]*#/  { next }
            /^[[:space:]]*$/  { flush(); next }
            /^URIs:/          { sub(/^URIs:[[:space:]]*/, "");          uri = $1; next }
            /^Suites:/        { sub(/^Suites:[[:space:]]*/, "");        suite = $1; next }
            /^Components:/    { sub(/^Components:[[:space:]]*/, "");    comps = $0; next }
            /^Architectures:/ { sub(/^Architectures:[[:space:]]*/, ""); arches = $0; next }
            END { flush() }
        ' "$f"
    done
}

# Does the mirror at $1 serve architecture $3 for suite $2, across components $4?
#
# Probes the per-architecture binary index. The suite Release file's
# `Architectures:` field cannot answer this -- it lists every architecture the
# *suite* defines, so a mirror that carries only some of them still advertises
# them all -- but binary-<arch>/Release is either there or it is not.
#
# The components come from the stanza rather than being hardcoded, and every
# one of them must serve the architecture. Both halves matter. `apt-get update`
# fetches an index for each component x architecture the stanza declares and
# fails hard on a 404, so "serves this arch" has to mean *all* of its
# components. But hardcoding `main universe` made that test unanswerable for a
# Launchpad PPA, which only ever publishes `main`: the probe 404ed on a
# component the stanza never claimed to have, and reported that the PPA did not
# serve an architecture it does serve. ppa:libretro/testing publishes arm64 for
# 119 packages, which is precisely the emulation core set an arm64 device wants.
_deps_mirror_serves() {
    local uri="${1%/}" suite="$2" arch="$3" components="$4" component
    [[ -n "$components" ]] || return 1
    for component in $components; do
        curl -sfI --max-time 20 -o /dev/null \
            "$uri/dists/$suite/$component/binary-$arch/Release" || return 1
    done
    return 0
}

# Make apt able to install packages for a foreign architecture.
#
# Two steps, the second of which is conditional: dpkg has to be told the
# architecture exists, and apt has to have somewhere to fetch it from. When the
# sources already carry it -- true of most full mirrors, and the reason this
# probes instead of assuming Ubuntu's archive/ports split -- there is nothing
# else to do.
#
# Where a source does *not* carry it, that source must be pinned to the
# architectures it does serve or `apt-get update` fails hard on the 404. That
# is decided per source, against the components each one declares, so a stanza
# that serves the target architecture keeps serving it. Pinning every source
# indiscriminately would have cost this host its arm64 libretro cores.
_deps_enable_foreign_arch() {
    local arch="$1"
    local ports_file="/etc/apt/sources.list.d/ubuntu-ports-$arch.sources"

    if ! dpkg --print-foreign-architectures | grep -qx -- "$arch"; then
        info "Telling dpkg about the $arch architecture..."
        maybe_sudo dpkg --add-architecture "$arch"
    fi

    if [[ -f "$ports_file" ]]; then
        info "A ports entry for $arch is already configured ($ports_file)"
        return 0
    fi

    # Every deb822 source, whatever it is called: Ubuntu ships ubuntu.sources,
    # but images and derivatives rename and split it.
    local -a sources=()
    local f
    for f in /etc/apt/sources.list.d/*.sources; do
        [[ -f "$f" ]] && sources+=("$f")
    done

    if [[ ${#sources[@]} -eq 0 ]]; then
        # Nothing to probe or pin. If apt can already reach the architecture
        # (a full mirror, or a source configured some other way) the install
        # below simply works; if it cannot, apt's own error is the honest one.
        warn "No deb822 apt source found to read a mirror and suite from."
        warn "Assuming apt can already fetch $arch packages; if it cannot, add a"
        warn "$arch source manually and re-run."
        return 0
    fi

    # Walk every stanza: note which files need pinning, and whether the archive
    # that provides the base system serves the target architecture. That last
    # one is what decides the ports fallback -- the -dev packages a cross build
    # links against come from the Ubuntu archive, not from a PPA, so a PPA
    # serving the architecture is not a substitute for the archive doing so.
    local -a needs_pin=()
    local base_uri="" base_suite="" base_components="" base_serves="false"
    local file uri suite comps arches
    while IFS=$'\t' read -r file uri suite comps arches; do
        [[ -n "$uri" && -n "$suite" ]] || continue

        # An Ubuntu-archive-shaped stanza: the one carrying `universe` beside
        # `main`. A PPA or vendor repo publishes `main` alone.
        if [[ -z "$base_uri" && " $comps " == *" main "* && " $comps " == *" universe "* ]]; then
            base_uri="$uri"
            base_suite="$suite"
            base_components="$comps"
            if _deps_mirror_serves "$uri" "$suite" "$arch" "$comps"; then
                base_serves="true"
            fi
        fi

        # A stanza that already declares its architectures needs nothing: apt
        # will not ask it for the new one.
        [[ -n "$arches" ]] && continue

        if _deps_mirror_serves "$uri" "$suite" "$arch" "$comps"; then
            info "$uri ($suite) serves $arch; leaving $file alone"
            continue
        fi

        info "$uri ($suite) does not serve $arch; $file will be pinned"
        # One file may hold several stanzas; pin it once.
        local seen
        for seen in ${needs_pin[@]+"${needs_pin[@]}"}; do
            [[ "$seen" == "$file" ]] && continue 2
        done
        needs_pin+=("$file")
    done < <(_deps_source_stanzas "${sources[@]}")

    if [[ -z "$base_uri" ]]; then
        warn "No Ubuntu-archive-shaped apt source (main + universe) was found."
        warn "Assuming apt can already fetch $arch packages; if it cannot, add a"
        warn "$arch source manually and re-run."
        return 0
    fi

    # Pin the sources that cannot serve the new architecture, whether or not a
    # ports entry follows: a source that 404s for $arch breaks `apt-get update`
    # on its own, now that dpkg knows the architecture exists.
    local native
    native="$(dpkg --print-architecture)"
    for f in ${needs_pin[@]+"${needs_pin[@]}"}; do
        info "Pinning $f to $native..."
        maybe_sudo cp -n "$f" "$f.pre-cross" || true
        # The `$` below belong to awk (an end-of-line anchor), not to the
        # shell, so the program has to stay single-quoted. The architecture
        # reaches it via -v.
        # shellcheck disable=SC2016
        maybe_sudo awk -v arches="$native" '
            /^[[:space:]]*$/ { flush(); print; next }
            { buf[n++] = $0 }
            END { flush() }
            function flush(   i) {
                for (i = 0; i < n; i++) {
                    print buf[i]
                    if (buf[i] ~ /^Types:/) print "Architectures: " arches
                }
                n = 0
            }
        ' "$f" | maybe_sudo tee "$f.new" >/dev/null
        maybe_sudo mv "$f.new" "$f"
    done

    if [[ "$base_serves" == "true" ]]; then
        info "The archive ($base_uri) already serves $arch; no ports entry needed"
        return 0
    fi

    info "$base_uri does not serve $arch; falling back to $DEPS_PORTS_URI"
    _deps_mirror_serves "$DEPS_PORTS_URI" "$base_suite" "$arch" "$base_components" \
        || die "Neither $base_uri nor $DEPS_PORTS_URI serves $arch packages for $base_suite"

    info "Adding a $DEPS_PORTS_URI entry for $arch..."
    maybe_sudo tee "$ports_file" >/dev/null <<EOF
# Added by \`shepherd deps install cross --arch $arch\`: the configured mirror
# does not carry $arch, so its packages come from Ubuntu's ports mirror.
Types: deb
URIs: $DEPS_PORTS_URI
Suites: $base_suite $base_suite-updates $base_suite-backports $base_suite-security
Components: $base_components
Architectures: $arch
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
}

# Refuse to install a cross set that would uninstall the native one.
#
# The two architectures' -dev chains are not always co-installable, and apt
# resolves that by *removing* the native half -- then exits 0. Installing
# libmpv-dev:<arch> on this workspace's dependency set drags out the host's own
# libmpv-dev, libgirepository1.0-dev, libarchive-dev, libcdio-dev,
# libext2fs-dev and libtool-bin, because several of those are `Multi-Arch: no`
# and libmpv-dev depends on them. The native build then fails at link time with
# `cannot find -lmpv`, a long way from anything that mentions cross-compiling.
#
# So simulate first and stop, rather than leave someone with a broken
# workstation and no idea why. A CI image that only ever cross-compiles has no
# native build to protect and passes --allow-remove.
_deps_refuse_destructive_install() {
    local -a packages=("$@")
    local sim removed

    info "Checking whether this would remove any natively-installed packages..."
    # `|| true`: a simulation that fails has nothing to say about removals, and
    # the real install below will report the failure properly.
    sim="$(maybe_sudo apt-get install -s -y "${packages[@]}" 2>/dev/null || true)"

    # apt lists removals under a header, one indented continuation block.
    removed="$(awk '
        /^The following packages will be REMOVED:/ { inblock = 1; next }
        inblock && /^[[:space:]]/ { print; next }
        inblock { inblock = 0 }
    ' <<<"$sim" | tr -s '[:space:]' ' ' | sed 's/^ //; s/ $//')"

    [[ -n "$removed" ]] || return 0

    warn "This would REMOVE these natively-installed packages:"
    warn "  $removed"
    die "Refusing: that would break the native build on this host (a missing
native libmpv-dev shows up as \`cannot find -lmpv\` when linking, which says
nothing about cross-compiling).

The two architectures' -dev chains are not co-installable here, so this host
can have one or the other, not both. Options:

  * Cross-compile in a container instead, and keep this host native. That is
    what CI does; see .ci/Dockerfile.cross.
  * Accept the trade and pass --allow-remove. Restore the native set afterwards
    with: shepherd deps install build"
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

# Get packages for a specific set.
#
# $2 is the Debian architecture, required only by the `cross` set, whose file is
# a template: every `@ARCH@` becomes the architecture being cross-compiled for.
get_packages() {
    local set_name="$1"
    local arch="${2:-}"
    
    case "$set_name" in
        cross)
            [[ -n "$arch" ]] || die "The 'cross' set needs --arch <debian-arch>"
            read_package_file "$DEPS_DIR/cross.pkgs" | sed "s/@ARCH@/$arch/g"
            ;;
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
            die "Unknown package set: $set_name (valid: build, run, test, android, agent, cross, dev)"
            ;;
    esac
}

# Split "<set> [--arch ARCH]" into DEPS_SET and DEPS_ARCH.
_deps_parse_args() {
    DEPS_SET="${1:-}"
    DEPS_ARCH=""
    DEPS_ALLOW_REMOVE=false
    shift || true

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --arch)
                [[ -n "${2:-}" ]] || die "--arch needs a Debian architecture"
                DEPS_ARCH="$2"
                shift 2
                ;;
            --allow-remove)
                DEPS_ALLOW_REMOVE=true
                shift
                ;;
            *)
                die "Unknown deps option: $1 (try: shepherd deps help)"
                ;;
        esac
    done

    # The cross set is the only one that is per-architecture, and the only
    # architecture worth cross-compiling for is one that is not the host's.
    if [[ "$DEPS_SET" == "cross" ]]; then
        [[ -n "$DEPS_ARCH" ]] || die "Usage: shepherd deps <cmd> cross --arch <debian-arch>"
        if [[ "$DEPS_ARCH" == "$(dpkg --print-architecture)" ]]; then
            die "$DEPS_ARCH is this host's own architecture; build for it natively instead"
        fi
    fi
}

# Print packages for a set (one per line)
deps_print() {
    local DEPS_SET DEPS_ARCH DEPS_ALLOW_REMOVE
    _deps_parse_args "$@"
    
    if [[ -z "$DEPS_SET" ]]; then
        die "Usage: shepherd deps print <build|run|cross|dev>"
    fi
    
    get_packages "$DEPS_SET" "$DEPS_ARCH"
}

# Install packages for a set
deps_install() {
    local DEPS_SET DEPS_ARCH DEPS_ALLOW_REMOVE
    _deps_parse_args "$@"
    local set_name="$DEPS_SET"
    
    if [[ -z "$set_name" ]]; then
        die "Usage: shepherd deps install <build|run|cross|dev>"
    fi
    
    check_ubuntu_version
    
    info "Installing $set_name dependencies..."

    # Multiarch first: none of the `:<arch>` packages below can even be
    # resolved until dpkg knows the architecture and apt has a mirror for it.
    if [[ "$set_name" == "cross" ]]; then
        _deps_enable_foreign_arch "$DEPS_ARCH"
    fi
    
    # Get the package list
    local packages
    packages=$(get_packages "$set_name" "$DEPS_ARCH" | tr '\n' ' ')
    
    if [[ -z "$packages" ]]; then
        warn "No packages to install for set: $set_name"
        return 0
    fi
    
    info "Packages: $packages"

    # The cross set is the one that can collide with what is already installed
    # for the host; every other set only adds to it.
    if [[ "$set_name" == "cross" ]] && [[ "$DEPS_ALLOW_REMOVE" != "true" ]]; then
        # shellcheck disable=SC2086  # word splitting is the point
        _deps_refuse_destructive_install $packages
    fi
    
    # Install using apt
    maybe_sudo apt-get update
    # shellcheck disable=SC2086
    maybe_sudo apt-get install -y $packages
    
    # For build and dev sets, also install Rust + the BPF authoring
    # toolchain (needed by crates/lunchbox-firewall-bpf).
    if [[ "$set_name" == "build" ]] || [[ "$set_name" == "dev" ]]; then
        install_rust
        install_bpf_toolchain
        install_wasm_toolchain
    fi

    # Only for dev: CI's image adds these itself, and a build-only host has no
    # use for them.
    if [[ "$set_name" == "dev" ]]; then
        install_lint_components
    fi

    # For run and dev sets, add lunchbox-media's non-apt dependencies: yt-dlp in
    # its virtualenv, and the VA-API drivers for this host's GPU. Neither can
    # live in run.pkgs — that file is installed as one unconditional apt
    # transaction, while the right VA driver depends on the hardware and yt-dlp
    # deliberately comes from pip. Runs after apt so python3-venv is present.
    if [[ "$set_name" == "run" ]] || [[ "$set_name" == "dev" ]]; then
        install_media_deps
    fi

    # The cross set needs rustc's own std for the target, which apt cannot
    # provide. `shepherd build --arch` derives the same triple.
    if [[ "$set_name" == "cross" ]]; then
        source "$HOME/.cargo/env" 2>/dev/null || true
        local triple
        triple="$(arch_to_triple "$DEPS_ARCH")"
        info "Adding the $triple Rust target..."
        rustup target add "$triple"
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
    local DEPS_SET DEPS_ARCH DEPS_ALLOW_REMOVE
    _deps_parse_args "$@"
    local set_name="$DEPS_SET"
    
    if [[ -z "$set_name" ]]; then
        die "Usage: shepherd deps check <build|run|cross|dev>"
    fi
    
    local packages
    packages=$(get_packages "$set_name" "$DEPS_ARCH")
    
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

    # For the dev set, also check the components CONTRIBUTING asks developers
    # to run: `cargo clippy` and `cargo fmt`.
    if [[ "$set_name" == "dev" ]] && command_exists rustup; then
        local component
        for component in clippy rustfmt; do
            if ! rustup component list --installed 2>/dev/null | grep -q "^$component"; then
                warn "$component is not installed (run: shepherd deps install dev)"
                return 1
            fi
        done
    fi

    # For the cross set, also check rustc's std for the target.
    if [[ "$set_name" == "cross" ]]; then
        local triple
        triple="$(arch_to_triple "$DEPS_ARCH")"
        if command_exists rustup \
            && ! rustup target list --installed 2>/dev/null | grep -qx -- "$triple"; then
            warn "The $triple Rust target is not installed (run: shepherd deps install cross --arch $DEPS_ARCH)"
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
    test     Extra packages needed for the lunchbox-e2e harness
    android  JDK + Android SDK + NDK for the companion-android and
             lunchbox-media-android apps
    agent    Headless-dev tooling for 'shepherd dev headless' (grim/wtype/jq)
    cross    Cross-compilation toolchain and the target architecture's half of
             the build set. Needs --arch <debian-arch>; see 'shepherd build --arch'.
             Refuses to run if it would uninstall the native build set (the two
             are not always co-installable); --allow-remove overrides, and is
             what the CI cross image passes.
    dev      All dependencies (build + run + test + agent + dev extras + Rust)

Note: The 'build' and 'dev' sets automatically install Rust via rustup.
      The 'android' set is standalone (not part of 'dev') because it
      downloads the Android SDK + NDK into /opt/android-sdk.

Examples:
    shepherd deps print build
    shepherd deps install dev
    shepherd deps install android
    shepherd deps install cross --arch arm64
    shepherd deps check run
EOF
            ;;
        *)
            die "Unknown deps command: $subcmd (try: shepherd deps help)"
            ;;
    esac
}
