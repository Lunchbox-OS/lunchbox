#!/usr/bin/env bash
# Shared post-install admin tasks for shepherd-launcher.
#
# These operations are useful *after* the software is installed, and they are
# deliberately repo-independent: they perform pure system actions (pip into a
# venv, flatpak installs, per-user config/group setup) with no reference to the
# source tree. That lets the same code back both entrypoints:
#
#   * scripts/shepherd            — the from-source dev/admin CLI
#   * scripts/shepherd-admin      — a slim entrypoint shipped in the .deb as
#                                   /usr/bin/shepherd-admin
#
# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

# The per-user and per-device operations here are install.sh's, shared rather
# than reimplemented: setup_user needs install_config / install_user_groups /
# add_user_to_groups / setup_state_for_user, and the custodian commands need
# install_policy and restore_state_to_home.
#
# Sourced rather than assumed. Both entrypoints happen to source install.sh
# first, so this held by luck of ordering -- and it broke the moment anything
# sourced this file on its own. No cycle: install.sh takes common, build and
# config, and none of those take this.
# shellcheck source=install.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/install.sh"

# ---------------------------------------------------------------------------
# yt-dlp
# ---------------------------------------------------------------------------

# yt-dlp virtualenv location and the symlink placed on PATH. The venv is owned
# by root and lives outside /usr so apt can never silently downgrade or remove
# yt-dlp. (Formerly in deps.sh; moved here so `shepherd deps install run` and
# `shepherd-admin yt-dlp install` share one implementation.)
YTDLP_VENV="/opt/shepherd/ytdlp-venv"
YTDLP_LINK="/usr/local/bin/yt-dlp"

# Check whether yt-dlp is available on PATH (covers both the venv symlink and
# any pre-existing system installation).
is_ytdlp_installed() {
    command_exists yt-dlp
}

# Install or upgrade yt-dlp into a dedicated virtualenv.
#
# Using a venv rather than apt keeps yt-dlp at the latest release, which matters
# because YouTube frequently changes the formats yt-dlp has to handle. Re-running
# this is idempotent and upgrades an existing installation.
install_ytdlp() {
    require_command python3

    if is_ytdlp_installed; then
        info "yt-dlp already available ($(yt-dlp --version 2>/dev/null || echo 'unknown version')); upgrading..."
    else
        info "Installing yt-dlp into virtualenv at $YTDLP_VENV..."
    fi

    # Ensure the venv parent directory exists.
    maybe_sudo mkdir -p "$(dirname "$YTDLP_VENV")"

    # Create the venv if it does not already exist.
    if [[ ! -d "$YTDLP_VENV" ]]; then
        maybe_sudo python3 -m venv "$YTDLP_VENV"
    fi

    # Install or upgrade yt-dlp inside the venv.
    maybe_sudo "$YTDLP_VENV/bin/pip" install --quiet --upgrade yt-dlp

    # Symlink the venv binary onto PATH so shepherd-media (and anything else)
    # can find it as plain `yt-dlp`.
    maybe_sudo ln -sf "$YTDLP_VENV/bin/yt-dlp" "$YTDLP_LINK"

    if is_ytdlp_installed; then
        success "yt-dlp installed ($(yt-dlp --version))"
    else
        die "yt-dlp installation failed — check pip output above"
    fi
}

# Dispatch for `shepherd-admin yt-dlp <install>`.
ytdlp_main() {
    local subcmd="${1:-install}"
    shift || true
    case "$subcmd" in
        install|update|upgrade)
            install_ytdlp
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd-admin yt-dlp install

Installs (or upgrades) yt-dlp into a virtualenv at $YTDLP_VENV and links it onto
PATH as $YTDLP_LINK. Needed only for YouTube media libraries; local mpv playback
does not use it. Re-run periodically to keep yt-dlp current.
EOF
            ;;
        *)
            die "Unknown yt-dlp command: $subcmd (try: shepherd-admin yt-dlp help)"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# VA-API drivers (hardware video decoding)
# ---------------------------------------------------------------------------
#
# libva dispatches to a per-vendor `<name>_drv_video.so`. With none installed,
# mpv's `hwdec` auto-detection finds nothing and shepherd-media decodes every frame
# on the CPU — roughly 6x the CPU for 1080p30 on an Intel HD 4000 (issue #115).
# Ubuntu's `mpv` package neither depends on nor recommends a driver, so a fresh
# install usually has none.
#
# Which driver is the right one depends on the GPU, so the packages are chosen
# from the hardware present rather than installed blanket-fashion. Whether the
# chosen driver actually loads at playback time is reported by shepherd-media
# itself, which observes mpv's `hwdec-current` and warns when video ends up on
# the CPU.

# Root of the sysfs tree the GPU inventory is read from. Overridable so the
# detection can be exercised against a fixture tree without the matching
# hardware — see `va-api detect`.
SHEPHERD_SYSFS_ROOT="${SHEPHERD_SYSFS_ROOT:-/sys}"

# Directories libva loads drivers from.
VA_DRIVER_DIRS=(/usr/lib/*/dri /usr/lib/dri)

# Inventory the display controllers on this host, one `<vendor> <detail>` line
# each, where <vendor> is one of intel/amd/nvidia/nouveau/other.
#
# PCI is the primary source (a display controller has class 0x03xxxx). Machines
# whose GPU is not on the PCI bus at all — most ARM boards — expose no such
# device, so those fall back to the kernel DRM driver's name.
va_detect_gpus() {
    local dev vendor class driver found=0

    for dev in "$SHEPHERD_SYSFS_ROOT"/bus/pci/devices/*; do
        [[ -r "$dev/class" && -r "$dev/vendor" ]] || continue
        class="$(<"$dev/class")"
        # Display controllers only: PCI base class 0x03.
        [[ "$class" == 0x03* ]] || continue
        vendor="$(<"$dev/vendor")"
        found=1
        case "$vendor" in
            # Intel.
            0x8086) printf 'intel %s\n' "${dev##*/}" ;;
            # AMD/ATI.
            0x1002 | 0x1022) printf 'amd %s\n' "${dev##*/}" ;;
            0x10de)
                # NVIDIA's own driver needs a VDPAU bridge; nouveau is served by
                # the VA driver inside Mesa. Tell them apart by what is bound.
                # Read the link text rather than canonicalising it: the driver
                # name is its last component either way, and `readlink -f` would
                # yield nothing if the target is not reachable, which would
                # silently misreport nouveau as the proprietary driver.
                driver=""
                [[ -L "$dev/driver" ]] && driver="$(basename "$(readlink "$dev/driver")")"
                if [[ "$driver" == "nouveau" ]]; then
                    printf 'nouveau %s\n' "${dev##*/}"
                else
                    printf 'nvidia %s\n' "${dev##*/}"
                fi
                ;;
            *) printf 'other %s\n' "$vendor" ;;
        esac
    done

    [[ "$found" -eq 1 ]] && return 0

    # No PCI display controller: fall back to whatever DRM devices exist.
    for dev in "$SHEPHERD_SYSFS_ROOT"/class/drm/card*; do
        [[ -L "$dev/device/driver" ]] || continue
        driver="$(basename "$(readlink "$dev/device/driver")")"
        case "$driver" in
            i915 | xe) printf 'intel %s\n' "$driver" ;;
            amdgpu | radeon) printf 'amd %s\n' "$driver" ;;
            nouveau) printf 'nouveau %s\n' "$driver" ;;
            nvidia*) printf 'nvidia %s\n' "$driver" ;;
            *) printf 'other %s\n' "$driver" ;;
        esac
    done
}

# Candidate driver packages for a vendor, most preferred first. Empty output
# means the vendor needs no package beyond what Mesa already installs.
va_packages_for_vendor() {
    case "$1" in
        intel)
            # Two generations of Intel driver, with no reliable way to tell from
            # sysfs which one a given chip needs: iHD covers Broadwell and newer,
            # i965 the generations before it. Offering both is not laziness —
            # libva probes them in turn at runtime and uses the one that
            # initialises, which is exactly the fallback observed on an HD 4000
            # (iHD fails, i965 loads).
            echo intel-media-va-driver
            echo i965-va-driver
            ;;
        amd | nouveau)
            # Gallium's VA drivers (radeonsi, r600, nouveau) ship inside Mesa,
            # which is already a runtime dependency. Some releases also split
            # them into their own package.
            echo mesa-va-drivers
            ;;
        nvidia)
            # Bridges VA-API onto NVIDIA's VDPAU/NVDEC.
            echo nvidia-vaapi-driver
            ;;
        *) ;;
    esac
}

# The libva driver names that serve a vendor, so the report can say whether the
# hardware actually has one rather than listing every driver on the system.
va_drivers_for_vendor() {
    case "$1" in
        intel) echo iHD_drv_video.so; echo i965_drv_video.so ;;
        amd) echo radeonsi_drv_video.so; echo r600_drv_video.so ;;
        nouveau) echo nouveau_drv_video.so ;;
        nvidia) echo nvidia_drv_video.so ;;
        *) ;;
    esac
}

# Filter a package list down to those this host's archive can actually install.
# A package with no candidate is one this release or architecture does not
# carry, which is normal rather than an error.
va_available_packages() {
    local pkg candidate
    for pkg in "$@"; do
        # Capture apt-cache's output instead of piping it into `grep -q`: grep
        # exits at the first match, apt-cache takes SIGPIPE, and under
        # `set -o pipefail` the pipeline then reports 141 — which would read as
        # "no candidate" for every package that *is* available.
        candidate="$(apt-cache policy "$pkg" 2>/dev/null | sed -n 's/^[[:space:]]*Candidate:[[:space:]]*//p')"
        if [[ -n "$candidate" && "$candidate" != "(none)" ]]; then
            echo "$pkg"
        fi
    done
}

# Which of `$@` (driver .so basenames) are present on this system.
va_present_drivers() {
    local dir name
    for name in "$@"; do
        for dir in "${VA_DRIVER_DIRS[@]}"; do
            if [[ -e "$dir/$name" ]]; then
                echo "$name"
                break
            fi
        done
    done
}

# Resolve the set of packages to install for the detected hardware. Prints one
# package per line; empty output means there is nothing to do.
va_resolve_packages() {
    local -a wanted=()
    local vendor _detail
    while read -r vendor _detail; do
        [[ -n "$vendor" ]] || continue
        mapfile -t -O "${#wanted[@]}" wanted < <(va_packages_for_vendor "$vendor")
    done < <(va_detect_gpus)

    if [[ ${#wanted[@]} -eq 0 ]]; then
        return 0
    fi
    # Dedupe (hybrid graphics report the same vendor twice) while keeping the
    # preference order, then drop what the archive does not offer.
    local -a unique=()
    local pkg
    for pkg in "${wanted[@]}"; do
        [[ " ${unique[*]-} " == *" $pkg "* ]] || unique+=("$pkg")
    done
    va_available_packages "${unique[@]}"
}

# Print what was detected and what would be installed, without changing
# anything.
va_api_detect() {
    local -a gpus=() packages=()
    mapfile -t gpus < <(va_detect_gpus)
    mapfile -t packages < <(va_resolve_packages)

    if [[ ${#gpus[@]} -eq 0 ]]; then
        warn "No GPU found under $SHEPHERD_SYSFS_ROOT"
        return 0
    fi

    local gpu vendor detail
    local -a wanted=() present=()
    for gpu in "${gpus[@]}"; do
        read -r vendor detail <<<"$gpu"
        mapfile -t wanted < <(va_drivers_for_vendor "$vendor")
        present=()
        [[ ${#wanted[@]} -gt 0 ]] && mapfile -t present < <(va_present_drivers "${wanted[@]}")
        if [[ ${#present[@]} -gt 0 ]]; then
            info "$vendor ($detail): driver present — ${present[*]}"
        else
            warn "$vendor ($detail): no VA-API driver installed; video will decode on the CPU"
        fi
    done

    if [[ ${#packages[@]} -eq 0 ]]; then
        info "Nothing to install: this hardware is served by the drivers Mesa ships, or its drivers are not packaged for this architecture"
    else
        info "Packages for this hardware: ${packages[*]}"
    fi
}

# Install the VA-API drivers this host's graphics hardware needs.
#
# Best-effort by design: hardware whose drivers are not packaged for this
# architecture is not an error, so this never fails the surrounding install.
install_va_api_drivers() {
    local -a packages=()
    mapfile -t packages < <(va_resolve_packages)

    if [[ ${#packages[@]} -eq 0 ]]; then
        info "No VA-API driver package applies to this host's graphics hardware; relying on the drivers Mesa ships"
        return 0
    fi

    info "Installing VA-API drivers for hardware video decoding: ${packages[*]}"
    if ! maybe_sudo apt-get install -y "${packages[@]}"; then
        warn "VA-API driver install failed; shepherd-media will decode video on the CPU"
        return 0
    fi
    success "VA-API drivers installed: ${packages[*]}"
}

# Dispatch for `shepherd-admin va-api <install|detect>`.
va_api_main() {
    local subcmd="${1:-install}"
    shift || true
    case "$subcmd" in
        install | update | upgrade)
            install_va_api_drivers
            ;;
        detect | status | show)
            va_api_detect
            ;;
        "" | help | -h | --help)
            cat <<'EOF'
Usage: shepherd-admin va-api <install|detect>

    install   Install the VA-API drivers this host's graphics hardware needs
    detect    Show the detected hardware and the packages it would install

shepherd-media decodes video on the GPU through mpv's VA-API support, which
needs a libva driver for your graphics hardware. Ubuntu's mpv package does not
pull one in, so without this every frame is decoded on the CPU.

The driver in use is reported in shepherd-media's log at the start of each
video ("mpv is decoding video with vaapi (zero-copy)", or a warning when it
ends up on the CPU).
EOF
            ;;
        *)
            die "Unknown va-api command: $subcmd (try: shepherd-admin va-api help)"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# Media dependencies
# ---------------------------------------------------------------------------

# Everything shepherd-media needs beyond the apt packages in run.pkgs: a VA-API
# driver for its hardware decoding, and yt-dlp for YouTube libraries. Both are
# kept out of run.pkgs — the drivers because the right ones depend on the
# hardware, yt-dlp because the archived build goes stale — so this is the one
# call that covers them.
install_media_deps() {
    install_va_api_drivers
    install_ytdlp
}

# Dispatch for `shepherd-admin media-deps <install>`.
media_deps_main() {
    local subcmd="${1:-install}"
    shift || true
    case "$subcmd" in
        install | update | upgrade)
            install_media_deps
            ;;
        "" | help | -h | --help)
            cat <<'EOF'
Usage: shepherd-admin media-deps install

Installs everything shepherd-media needs that apt cannot cover on its own:

    va-api    VA-API drivers matched to this host's graphics hardware,
              without which video is decoded on the CPU
    yt-dlp    into a virtualenv, for YouTube media libraries

Equivalent to running 'shepherd-admin va-api install' and
'shepherd-admin yt-dlp install'. Re-run periodically to keep yt-dlp current.
EOF
            ;;
        *)
            die "Unknown media-deps command: $subcmd (try: shepherd-admin media-deps help)"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# Android apps (Shepherd Companion + Shepherd Media)
# ---------------------------------------------------------------------------
#
# Both apps run on a phone, tablet, or Fire TV rather than on the shepherd
# device, so "installing" one means obtaining an APK and pushing it over adb to
# the attached Android device. Where the APK comes from follows how shepherd
# itself was installed:
#
#   * source checkout  — built from the app's own Gradle project, exactly as
#                        CONTRIBUTING.md documents (debug-signed; no keystore
#                        needed), so a developer installs what they just wrote.
#   * packaged install — the signed release asset matching the installed
#                        version, downloaded from the Forgejo release and
#                        verified against its published .sha256 sidecar.
#
# For a phone that should keep *itself* updated, the F-Droid repository is still
# the better route (docs/INSTALL.md). This covers what F-Droid cannot: Fire TV
# sticks, whose Android has no unattended-update path and whose F-Droid client
# has no remote-friendly UI, and development phones tracking a local build.

# The Forgejo project whose releases carry the APKs. Overridable so a fork or a
# staging server can be pointed at without editing this file.
SHEPHERD_FORGE_URL="${SHEPHERD_FORGE_URL:-https://git.armeafamily.com/albert/shepherd-launcher}"

# Downloaded release APKs are cached here so a re-run does not re-fetch. Per
# user rather than system-wide, because this command deliberately does not want
# root: adb authorises devices against the *invoking user's* ~/.android key, so
# running it under sudo is how you get a phone that reports `unauthorized`.
ANDROID_APK_CACHE="${SHEPHERD_APK_CACHE:-${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}/shepherd/apk}"

# Per-app metadata, keyed by the name `apps install` takes. Sets:
#   ANDROID_APP_DIR       Gradle project, relative to the repo root
#   ANDROID_APP_ARTIFACT  release-asset stem (<artifact>_<version>.apk, named by
#                         release.yml's apk matrix)
#   ANDROID_APP_PACKAGE   applicationId
#   ANDROID_APP_LABEL     human-readable name
# Returns 1 for an unknown app so the caller can report it.
android_app_meta() {
    case "${1:-}" in
        companion)
            ANDROID_APP_DIR="companion-android"
            ANDROID_APP_ARTIFACT="shepherd-companion"
            ANDROID_APP_PACKAGE="com.armeafamily.shepherd.companion"
            ANDROID_APP_LABEL="Shepherd Companion"
            ;;
        media)
            ANDROID_APP_DIR="crates/shepherd-media-android/android"
            ANDROID_APP_ARTIFACT="shepherd-media"
            ANDROID_APP_PACKAGE="com.armeafamily.shepherd.media"
            ANDROID_APP_LABEL="Shepherd Media"
            ;;
        *)
            return 1
            ;;
    esac
}

# True when these libs are running out of a source checkout rather than the
# .deb's /usr/lib/shepherd/lib. This is what decides build-vs-download: a
# checkout has the Gradle projects, a packaged install has only the VERSION file
# naming the release to fetch.
is_source_checkout() {
    local root
    root="$(get_repo_root)"
    [[ -f "$root/Cargo.toml" && -d "$root/companion-android" ]]
}

# The version an unqualified download asks for: the canonical VERSION shipped
# beside these scripts (repo root from source, /usr/share/shepherd when
# packaged). Kept separate from version.sh's version_read, which resolves
# through get_repo_root and so is meaningless under /usr.
installed_version() {
    local f v
    f="$(get_data_dir)/VERSION"
    [[ -f "$f" ]] || die "No VERSION file at $f; pass --version X.Y.Z"
    v="$(head -n1 "$f" | tr -d '[:space:]')"
    [[ -n "$v" ]] || die "VERSION file is empty: $f"
    printf '%s\n' "$v"
}

# Print the path to an adb binary, preferring one on PATH and falling back to
# the SDK the android deps set installs (whose platform-tools are not linked
# onto PATH).
find_adb() {
    if command_exists adb; then
        command -v adb
        return 0
    fi
    local sdk="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-/opt/android-sdk}}"
    if [[ -x "$sdk/platform-tools/adb" ]]; then
        printf '%s\n' "$sdk/platform-tools/adb"
        return 0
    fi
    return 1
}

# Resolve which attached device to install onto: $2 if given, otherwise the sole
# device in `device` state. Ambiguity is an error rather than a guess — the
# wrong pick here is an app on a family member's phone.
android_resolve_device() {
    local adb="$1" want="${2:-}"
    local -a serials=() pending=()

    # A host:port serial is a device on the network (a Fire TV with ADB
    # debugging on, typically). Connecting is idempotent and a no-op when the
    # link is already up, so try it before deciding the device is missing.
    if [[ "$want" == *:* ]]; then
        info "Connecting to $want over the network..."
        "$adb" connect "$want" >&2 || true
    fi

    # `adb devices` prints a header line, then "<serial>\t<state>".
    mapfile -t serials < <("$adb" devices 2>/dev/null | awk 'NR > 1 && $2 == "device" { print $1 }')
    mapfile -t pending < <("$adb" devices 2>/dev/null | awk 'NR > 1 && $2 != "" && $2 != "device" { print $1 " (" $2 ")" }')

    if [[ -n "$want" ]]; then
        local s
        for s in "${serials[@]-}"; do
            if [[ "$s" == "$want" ]]; then
                printf '%s\n' "$s"
                return 0
            fi
        done
        die "No attached device with serial '$want' (adb devices: ${serials[*]-none})"
    fi

    if [[ ${#serials[@]} -eq 1 ]]; then
        printf '%s\n' "${serials[0]}"
        return 0
    fi

    if [[ ${#serials[@]} -eq 0 ]]; then
        if [[ ${#pending[@]} -gt 0 ]]; then
            # Almost always `unauthorized`: the phone shows an RSA-fingerprint
            # prompt on first connection that a human has to accept.
            error "No usable Android device; adb sees: ${pending[*]}"
            die "Accept the USB-debugging prompt on the device (or re-run without sudo — adb keys are per-user)"
        fi
        error "No Android device attached (adb devices is empty)"
        die "Connect the phone/tablet/Fire TV with USB debugging enabled, or 'adb connect <host>:5555' for one on the network"
    fi

    error "Several devices attached: ${serials[*]}"
    die "Pick one with --device SERIAL"
}

# Build an app's APK from the Gradle project in this checkout and print its
# path. Debug-signed: producing the release signature needs the project's
# keystore, which lives only in CI (release.yml decodes it from a secret).
android_build_apk() {
    local app="$1"
    android_app_meta "$app" || die "Unknown Android app: $app"

    local repo_root gradle_dir
    repo_root="$(get_repo_root)"
    gradle_dir="$repo_root/$ANDROID_APP_DIR"
    [[ -x "$gradle_dir/gradlew" ]] || die "No Gradle project at $gradle_dir"

    # Gradle finds the SDK via ANDROID_SDK_ROOT or local.properties; point it at
    # the location `shepherd deps install android` uses when neither is set.
    local sdk="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-/opt/android-sdk}}"
    if [[ ! -d "$sdk" && ! -f "$gradle_dir/local.properties" ]]; then
        die "Android SDK not found at $sdk; run 'shepherd deps install android' (or set ANDROID_SDK_ROOT)"
    fi

    # The media app is a NativeActivity whose Gradle build cross-compiles a Rust
    # cdylib through cargo-ndk, so it needs more than the SDK. Say which piece is
    # missing here rather than letting Gradle fail deep inside the Exec task.
    if [[ "$app" == "media" ]]; then
        cargo ndk --version >/dev/null 2>&1 \
            || die "cargo-ndk not installed (the media APK cross-compiles a Rust cdylib); run 'shepherd deps install android'"
        if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
            # sdkmanager installs under $sdk/ndk/<version>; take the newest.
            local ndk
            ndk="$(find "$sdk/ndk" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | sort -V | tail -n1)"
            [[ -n "$ndk" ]] || die "No NDK under $sdk/ndk; run 'shepherd deps install android' (or set ANDROID_NDK_HOME)"
            export ANDROID_NDK_HOME="$ndk"
            info "Using ANDROID_NDK_HOME=$ANDROID_NDK_HOME"
        fi
    fi

    info "Building the $ANDROID_APP_LABEL APK from source ($ANDROID_APP_DIR)..."
    # Gradle's own output goes to stderr: this function's stdout is the APK path
    # its caller captures, and a build log mixed into it becomes the filename.
    ( cd "$gradle_dir" && ANDROID_SDK_ROOT="$sdk" ./gradlew --no-daemon :app:assembleDebug >&2 ) \
        || die "Gradle build failed for $ANDROID_APP_LABEL"

    local apk
    apk="$(find "$gradle_dir/app/build/outputs/apk/debug" -name '*.apk' -type f 2>/dev/null | head -n1)"
    [[ -n "$apk" ]] || die "Gradle reported success but produced no APK under $gradle_dir/app/build/outputs/apk/debug"
    printf '%s\n' "$apk"
}

# Download an app's release APK from the Forgejo release for $2 (default: the
# installed version) and print its path. Verified against the .sha256 sidecar
# release.yml uploads beside every asset — this file is about to be installed on
# a family device, so a download that cannot be checked is a failure, not a
# warning. Cached, so a re-run neither re-downloads nor re-verifies blindly.
android_download_apk() {
    local app="$1" version="${2:-}"
    android_app_meta "$app" || die "Unknown Android app: $app"
    require_command curl

    [[ -n "$version" ]] || version="$(installed_version)"

    local name url dest
    name="${ANDROID_APP_ARTIFACT}_${version}.apk"
    url="$SHEPHERD_FORGE_URL/releases/download/v${version}/${name}"
    dest="$ANDROID_APK_CACHE/$name"

    ensure_dir "$ANDROID_APK_CACHE" 0755

    if [[ -f "$dest" && -f "$dest.sha256" ]] \
        && ( cd "$ANDROID_APK_CACHE" && sha256sum --check --status "$name.sha256" ); then
        info "Using the cached $ANDROID_APP_LABEL APK at $dest"
        printf '%s\n' "$dest"
        return 0
    fi

    info "Downloading $name from $SHEPHERD_FORGE_URL (release v$version)..."
    if ! curl -fSL --proto '=https' --tlsv1.2 -o "$dest.part" "$url"; then
        rm -f "$dest.part"
        error "Could not download $url"
        die "No such release asset. Check the release page, pass --version X.Y.Z, or build from a checkout with --source."
    fi
    if ! curl -fsSL --proto '=https' --tlsv1.2 -o "$dest.sha256.part" "$url.sha256"; then
        rm -f "$dest.part" "$dest.sha256.part"
        die "Release asset $name has no .sha256 sidecar; refusing to install an unverified APK (override with --apk PATH)"
    fi

    mv "$dest.part" "$dest"
    mv "$dest.sha256.part" "$dest.sha256"
    # The sidecar records the basename only, so check from the cache directory.
    if ! ( cd "$ANDROID_APK_CACHE" && sha256sum --check --status "$name.sha256" ); then
        rm -f "$dest" "$dest.sha256"
        die "Checksum mismatch for $name; the download was corrupted or tampered with"
    fi

    success "Downloaded and verified $name"
    printf '%s\n' "$dest"
}

# `adb install -r` the APK, translating the one failure that is guaranteed to
# happen sooner or later into what to do about it.
android_adb_install() {
    local adb="$1" serial="$2" apk="$3" package="$4" label="$5"
    local out status=0

    info "Installing $label on $serial..."
    out="$("$adb" -s "$serial" install -r "$apk" 2>&1)" || status=$?
    printf '%s\n' "$out" >&2

    if [[ $status -eq 0 ]] && ! grep -qi 'INSTALL_FAILED\|^Failure' <<<"$out"; then
        success "$label installed on $serial"
        return 0
    fi

    # A local debug build and a release/F-Droid build are signed with different
    # keys, and Android refuses to replace one with the other.
    if grep -qi 'INSTALL_FAILED_UPDATE_INCOMPATIBLE\|signatures do not match\|INSTALL_FAILED_VERSION_DOWNGRADE' <<<"$out"; then
        error "$label is already installed from a differently signed (or newer) build"
        info "Replacing it means removing the installed copy first:"
        info "  adb -s $serial uninstall $package"
        if [[ "$package" == *.companion ]]; then
            warn "That erases the app's data — the admin records and claim tokens for every"
            warn "device this phone administers. A device whose token is lost has to be"
            warn "factory-reset (see docs/INSTALL.md, \"Re-pairing\") before it can be"
            warn "claimed again. Confirm with the owner first."
        fi
    fi
    die "adb install failed for $label"
}

# Install one of the Android apps onto an attached device.
android_app_install() {
    local app="$1"; shift || true
    local device="" apk="" version="" provenance=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --device|-s) device="${2:-}"; [[ -n "$device" ]] || die "--device needs a serial"; shift 2 ;;
            --apk) apk="${2:-}"; [[ -n "$apk" ]] || die "--apk needs a path"; shift 2 ;;
            --version) version="${2:-}"; [[ -n "$version" ]] || die "--version needs X.Y.Z"; shift 2 ;;
            --source) provenance="source"; shift ;;
            --release) provenance="release"; shift ;;
            *) die "Unknown option for 'apps install $app': $1" ;;
        esac
    done

    android_app_meta "$app" || die "Unknown Android app: $app"

    # Unlike the steam/chrome backends, nothing here touches the host: the APK
    # goes to a phone. Root would only hurt — adb authorises per user, and a
    # root Gradle run leaves root-owned build output in the checkout.
    if is_root; then
        if [[ -n "${SUDO_USER:-}" ]]; then
            die "Run 'apps install $app' without sudo: adb authorises devices per user, so under sudo the device reports 'unauthorized'"
        fi
        warn "Running as root: adb will use root's ~/.android key, so a device authorised for another user reports 'unauthorized'."
    fi

    local adb
    adb="$(find_adb)" \
        || die "adb not found. Install it with: apt install adb (or 'shepherd deps install android' for the full SDK)"

    # Pick the target before building or downloading: a missing phone is the
    # likeliest failure, and it should not cost a Gradle run to discover.
    local serial
    serial="$(android_resolve_device "$adb" "$device")"

    if [[ -n "$apk" ]]; then
        [[ -f "$apk" ]] || die "No such APK: $apk"
        info "Using the APK at $apk"
    else
        # Provenance follows the installation unless the caller overrides it.
        [[ -n "$provenance" ]] || { is_source_checkout && provenance="source" || provenance="release"; }
        if [[ "$provenance" == "source" ]]; then
            is_source_checkout || die "--source needs a source checkout; this is a packaged install (try --release)"
            apk="$(android_build_apk "$app")"
        else
            apk="$(android_download_apk "$app" "$version")"
        fi
    fi

    android_adb_install "$adb" "$serial" "$apk" "$ANDROID_APP_PACKAGE" "$ANDROID_APP_LABEL"

    if [[ "$app" == "companion" ]]; then
        info "Open $ANDROID_APP_LABEL and tap \"Pair a device\" — see docs/INSTALL.md, \"Pairing your phone with a device\"."
    else
        info "Open $ANDROID_APP_LABEL and add a library from Settings."
    fi
    info "For a phone that should keep itself updated, install from the F-Droid repository instead"
    info "(https://git.armeafamily.com/fdroid/repo — see docs/INSTALL.md)."
}

# ---------------------------------------------------------------------------
# Flatpak apps
# ---------------------------------------------------------------------------

FLATHUB_REMOTE_URL="https://flathub.org/repo/flathub.flatpakrepo"

# Ubuntu 23.10+ restricts unprivileged user namespaces via AppArmor
# (kernel.apparmor_restrict_unprivileged_userns=1). Steam's sandbox — like
# Flatpak's — needs one, so on a fresh system it fails with "Steam now requires
# user namespaces to be enabled". (A machine that already ran a Flatpak app
# usually has userns working, which is why Steam "just works" there.)
USERNS_SYSCTL="kernel.apparmor_restrict_unprivileged_userns"
USERNS_DROPIN="/etc/sysctl.d/90-shepherd-userns.conf"

# Permit unprivileged user namespaces so Steam's (and Flatpak's) sandbox can
# start. No-op when the kernel lacks the knob (older/non-Ubuntu) or it's already
# permitted. This relaxes an Ubuntu kernel hardening SYSTEM-WIDE; it's the
# supported way to run these sandboxes and is reversible (remove $USERNS_DROPIN).
ensure_unprivileged_userns() {
    # Older kernels / non-Ubuntu systems don't have this knob — nothing to do.
    if ! sysctl -n "$USERNS_SYSCTL" >/dev/null 2>&1; then
        return 0
    fi
    if [[ "$(sysctl -n "$USERNS_SYSCTL" 2>/dev/null || echo 0)" == "0" ]]; then
        info "Unprivileged user namespaces already permitted."
        return 0
    fi

    warn "Enabling unprivileged user namespaces ($USERNS_SYSCTL=0) so Steam's"
    warn "sandbox can start. This relaxes an Ubuntu kernel hardening system-wide;"
    warn "remove $USERNS_DROPIN and reboot to revert."
    ensure_dir "$(dirname "$USERNS_DROPIN")" 0755
    cat > "$USERNS_DROPIN" <<EOF
# Installed by \`shepherd-admin apps install steam\`.
# Steam (like Flatpak) creates an unprivileged user namespace for its sandbox;
# Ubuntu 23.10+ restricts that by default. Permit it so the sandbox starts.
$USERNS_SYSCTL=0
EOF
    chmod 0644 "$USERNS_DROPIN"
    sysctl -w "$USERNS_SYSCTL=0" >/dev/null 2>&1 \
        || warn "Could not apply $USERNS_SYSCTL live; it takes effect after a reboot"
    success "Unprivileged user namespaces permitted"
}

# Ensure the system-wide Flathub remote exists (idempotent).
ensure_flathub() {
    require_command flatpak
    if flatpak remotes --columns=name 2>/dev/null | grep -qx flathub; then
        return 0
    fi
    info "Adding the Flathub remote..."
    flatpak remote-add --if-not-exists flathub "$FLATHUB_REMOTE_URL"
}

# ---------------------------------------------------------------------------
# RetroArch (type = "retroarch" entries)
# ---------------------------------------------------------------------------

# libretro cores packaged for Ubuntu, by the short name an entry's `core =`
# field takes. The package is always `libretro-<name>` and the shared object
# `<name>_libretro.so`, which is what shepherd's core resolution looks for.
#
# Deliberately only the distro's own packages: RetroArch's built-in "core
# downloader" pulls unsigned binaries at runtime, which is not something a
# supervised kiosk should be doing behind the operator's back.
#
# Each row is `<core name>:<the system it runs>`, so the usage text can say
# what a core is for instead of listing bare names.
RETROARCH_CORES=(
    "beetle-pce-fast:PC Engine / TurboGrafx-16"
    "beetle-psx:PlayStation"
    "beetle-vb:Virtual Boy"
    "beetle-wswan:WonderSwan"
    "bsnes-mercury-accuracy:SNES (accuracy over speed)"
    "bsnes-mercury-balanced:SNES"
    "bsnes-mercury-performance:SNES (speed over accuracy)"
    "desmume:Nintendo DS"
    "gambatte:Game Boy / Color"
    "genesisplusgx:Mega Drive / Genesis / Master System"
    "mgba:Game Boy Advance, Game Boy / Color"
    "nestopia:NES"
    "sameboy:Game Boy / Color"
    "snes9x:SNES"
)

# Installed when no core is named: the one config.example.toml documents.
RETROARCH_DEFAULT_CORES=(mgba)

# Whether $1 is a core this script knows how to install.
retroarch_core_is_known() {
    local candidate="$1" row
    for row in "${RETROARCH_CORES[@]}"; do
        [[ "${row%%:*}" == "$candidate" ]] && return 0
    done
    return 1
}

# Just the core names, space-separated (for error messages).
retroarch_core_names() {
    local row names=()
    for row in "${RETROARCH_CORES[@]}"; do
        names+=("${row%%:*}")
    done
    printf '%s' "${names[*]}"
}

# The available cores, one indented `name  — system` line each.
retroarch_core_table() {
    local row
    for row in "${RETROARCH_CORES[@]}"; do
        printf '                %-26s %s\n' "${row%%:*}" "${row#*:}"
    done
}

# The libretro team's PPAs, opted into with `--ppa[=channel]`.
#
# Which one matters, and not the way the names suggest:
#
#   testing  ~98 core packages — everything the Ubuntu archive has plus N64,
#            GameCube/Wii, DS, PlayStation, MAME and the rest. This is the only
#            source that adds cores, so it is what a bare `--ppa` selects.
#   stable   the RetroArch *frontend* only: no cores at all. Its version has
#            matched the archive's on recent releases, so it is rarely worth
#            adding; offered for tracking upstream builds between Ubuntu
#            releases.
RETROARCH_PPA_CHANNELS="testing stable"
RETROARCH_PPA_DEFAULT_CHANNEL="testing"

# Whether a package exists in the configured apt sources.
apt_package_available() {
    local candidate
    candidate=$(apt-cache policy "$1" 2>/dev/null | awk '/Candidate:/ {print $2; exit}')
    [[ -n "$candidate" && "$candidate" != "(none)" ]]
}

# Add a libretro PPA (idempotent; `add-apt-repository` no-ops if present).
retroarch_add_ppa() {
    local channel="$1"
    if ! command_exists add-apt-repository; then
        info "Installing software-properties-common (for add-apt-repository)..."
        maybe_sudo apt-get install -y software-properties-common
    fi
    warn "Adding ppa:libretro/$channel — a third-party apt source, which can"
    warn "install and upgrade packages on this system from here on. Remove it"
    warn "with: sudo add-apt-repository --remove ppa:libretro/$channel"
    maybe_sudo add-apt-repository -y "ppa:libretro/$channel"
}

# Install RetroArch plus the named cores (default: mgba).
#
# Cores are named the way an entry names them (`core = "mgba"`), not by package,
# so there is one spelling to learn — and the naming holds across both the
# Ubuntu archive and the PPAs, so an entry does not care where its core came
# from. Everything lands in the multiarch libretro directory, which is on
# shepherd's core search path.
retroarch_install() {
    local -a cores=()
    local ppa_channel=""
    local arg

    for arg in "$@"; do
        case "$arg" in
            --ppa)
                ppa_channel="$RETROARCH_PPA_DEFAULT_CHANNEL"
                ;;
            --ppa=*)
                ppa_channel="${arg#--ppa=}"
                if [[ " $RETROARCH_PPA_CHANNELS " != *" $ppa_channel "* ]]; then
                    die "Unknown PPA channel '$ppa_channel' (expected: $RETROARCH_PPA_CHANNELS)"
                fi
                ;;
            -*)
                die "Unknown option '$arg' (expected: --ppa[=${RETROARCH_PPA_CHANNELS// /|}])"
                ;;
            *)
                # A package name is about to be built from this, so keep it to
                # something that cannot be mistaken for an option or a path.
                if [[ ! "$arg" =~ ^[a-z0-9][a-z0-9._+-]*$ ]]; then
                    die "Invalid core name '$arg'"
                fi
                cores+=("$arg")
                ;;
        esac
    done

    if [[ ${#cores[@]} -eq 0 ]]; then
        cores=("${RETROARCH_DEFAULT_CORES[@]}")
    fi

    # Without a PPA the catalog is known up front, so a typo can be caught
    # before asking for root. With one it isn't — the whole point is that more
    # cores become available — so those names are checked against apt after the
    # repository is added, below.
    local core
    if [[ -z "$ppa_channel" ]]; then
        for core in "${cores[@]}"; do
            if ! retroarch_core_is_known "$core"; then
                die "Unknown core '$core'. Available: $(retroarch_core_names)
Many more (N64, GameCube/Wii, Saturn, arcade...) are packaged only in the
libretro PPA, which is opt-in:
    sudo shepherd-admin apps install retroarch --ppa $core"
            fi
        done
    fi

    require_root

    if [[ -n "$ppa_channel" ]]; then
        retroarch_add_ppa "$ppa_channel"
    fi

    maybe_sudo apt-get update

    local -a packages=(retroarch retroarch-assets libretro-core-info)
    for core in "${cores[@]}"; do
        if ! apt_package_available "libretro-$core"; then
            die "No package 'libretro-$core' in the configured apt sources.
List what is available with:
    apt-cache search --names-only '^libretro-'"
        fi
        packages+=("libretro-$core")
    done

    info "Installing RetroArch and cores: ${cores[*]}"
    if ! maybe_sudo apt-get install -y "${packages[@]}"; then
        die "Failed to install: ${packages[*]}"
    fi
    success "Installed RetroArch with cores: ${cores[*]}"

    cat <<EOF

Reference a game with:

    [entries.kind]
    type = "retroarch"
    core = "${cores[0]}"
    content = "~/Games/roms/your-game.rom"

The content path must be absolute or start with ~/. shepherd stores each
entry's saves and save states under its own directory in the data dir, and
saves state on close / restores it on open by default.

No games are installed — supply your own, and only ones you have the right to.
EOF
}

# Install Okular and everything a reading activity needs (issue #160).
#
# Three packages, and the second is the one people miss: on Ubuntu, Okular's
# EPUB support ships in okular-extra-backends, so a plain `apt install okular`
# opens PDFs and refuses novels. The font is installed too, because the
# `ebook` kind's default reading font has to exist for the entry to look like
# the example — a missing family silently falls back to whatever Qt picks.
ebook_install() {
    local arg
    for arg in "$@"; do
        case "$arg" in
            -*) die "Unknown option for 'apps install okular': $arg" ;;
            *) die "'apps install okular' takes no arguments (got '$arg')" ;;
        esac
    done

    require_root
    maybe_sudo apt-get update

    local -a packages=(okular okular-extra-backends fonts-noto-core)
    info "Installing Okular, its extra format backends, and the default reading font"
    if ! maybe_sudo apt-get install -y "${packages[@]}"; then
        die "Failed to install: ${packages[*]}"
    fi
    success "Installed: ${packages[*]}"

    cat <<EOF

Reference a book with:

    [[entries]]
    id = "the-hobbit"
    label = "The Hobbit"

    [entries.kind]
    type = "ebook"
    book = "~/Books/the-hobbit.epub"

The book path must be absolute or start with ~/. shepherd generates the
reader's whole configuration per entry — the restrictions that keep a child in
the book, the page-at-a-time view, and the reading font — and re-renders it on
every launch, so nothing there needs installing or editing by hand.

No books are installed. Supply your own, DRM-free; a store's own books belong
in that store's app (a browser or Android activity), not here.
EOF
}

# Install a supported activity backend, or one of the project's own Android
# apps, using whichever packaging each actually expects — they all differ. The
# type="steam" adapter drives Canonical's Steam *snap* (config.example.toml
# documents `snap install steam`), Chrome is wrapped as the Flathub flatpak
# `com.google.Chrome`, RetroArch comes from the distro's own packages,
# type="android" runs on Waydroid (shepherd ships the privileged helper; this
# provisions the group + points at the engine install), and the companion/media
# apps are APKs pushed over adb to an attached phone or TV stick (see the
# Android-apps section above). Add rows here as new backends are supported.
apps_install() {
    local app="${1:-}"
    shift || true
    case "$app" in
        retroarch)
            retroarch_install "$@"
            ;;
        steam)
            require_root
            require_command snap
            info "Installing the Steam snap (snap install steam)..."
            snap install steam
            # The snap's sandbox needs the mount-observe interface; current snaps
            # auto-connect it, but connect explicitly for older installs (no-op
            # if already connected). This is the steam-snap maintainers' fix for
            # the "requires user namespaces" error.
            snap connect steam:mount-observe 2>/dev/null || true
            # …and unprivileged user namespaces must be permitted for that
            # sandbox to start (see ensure_unprivileged_userns).
            ensure_unprivileged_userns
            success "Installed the Steam snap"
            info "Launch Steam once and log in before using type=\"steam\" entries."
            ;;
        okular|ebook)
            ebook_install "$@"
            ;;
        chrome)
            require_root
            ensure_flathub
            info "Installing com.google.Chrome from Flathub..."
            flatpak install -y flathub com.google.Chrome
            success "Installed com.google.Chrome"
            info "Reference it with kind = \"flatpak\", app_id = \"com.google.Chrome\"."
            ;;
        android)
            # The shepherd side (helper binary + polkit assets) ships with the
            # package via install_system; this opt-in step provisions the host:
            # the shepherd-waydroid group + membership (shared with the
            # from-source `shepherd install waydroid` via provision_waydroid_host)
            # and guidance for the Waydroid engine itself, which lives in a
            # third-party repo and pulls ~2.4 GB of Android images.
            local user="${2:-}"
            require_root
            if [[ ! -x "$WAYDROID_HELPER_PATH" ]]; then
                die "shepherd-waydroid-helper missing at $WAYDROID_HELPER_PATH; reinstall shepherd-launcher (the package ships it)."
            fi
            [[ -n "$user" ]] && validate_user "$user"

            provision_waydroid_host "$user"

            if ! command -v waydroid >/dev/null 2>&1; then
                warn "Waydroid itself is not installed. Install the engine, then re-run to finish:"
                info "  curl https://repo.waydro.id | sudo bash -s \"\$(. /etc/os-release && echo \"\$VERSION_CODENAME\")\""
                info "  sudo apt install -y waydroid"
                info "  echo binder_linux | sudo tee /etc/modules-load.d/waydroid.conf && sudo modprobe binder_linux"
                info "  sudo waydroid init   # downloads the Android images (~2.4 GB)"
            else
                info "Waydroid engine detected. If not yet initialized: sudo waydroid init"
            fi

            success "Provisioned the shepherd side of the Android (Waydroid) backend"
            if [[ -z "$user" ]]; then
                info "Add the kiosk user to the group with:"
                info "  shepherd-admin apps install android USER"
            fi
            info "Reference Android apps with type = \"android\". The Lock Task DPC in"
            info "dpc-waydroid/ is a verified building block but is NOT wired in by default."
            ;;

        companion|media)
            android_app_install "$app" "$@"
            ;;
        ""|help|-h|--help)
            apps_usage
            return 0
            ;;
        *)
            die "Unknown app '$app' (supported: steam, chrome, retroarch, okular, android, companion, media)"
            ;;
    esac
}

apps_usage() {
    cat <<EOF
Usage: shepherd-admin apps install <steam|chrome|retroarch [core...]|okular>
       shepherd-admin apps install android [USER]
       shepherd-admin apps install <companion|media> [options]

Installs a supported activity backend, or one of shepherd's own Android apps,
with the packaging each expects (they differ):

    steam       Canonical's Steam snap (drives type = "steam" entries). Launch
                it and log in once before those entries will work. Also permits
                unprivileged user namespaces (Ubuntu restricts them by default),
                which Steam's sandbox needs — see $USERNS_DROPIN.
    chrome      com.google.Chrome from Flathub (for kind = "flatpak" entries).
    retroarch   RetroArch and libretro cores (drives type = "retroarch"
                entries). Takes the cores to install, named as an entry's
                \`core =\` field names them; defaults to ${RETROARCH_DEFAULT_CORES[*]}.
                Installs no games.

                From the Ubuntu archive by default. Pass --ppa to add the
                libretro team's PPA instead, which is the only way to get cores
                the archive does not package (N64, GameCube/Wii, Saturn, arcade
                via MAME, and ~85 more). That is a third-party apt source for
                the whole system, so it is opt-in:

                    --ppa            ppa:libretro/testing — the cores
                    --ppa=testing    same
                    --ppa=stable     the RetroArch frontend only, NO cores
                                     (and the same version the archive ships
                                     on recent releases)

                Cores in the Ubuntu archive (docs/emulators.md lists the
                full catalog, including everything --ppa adds):
$(retroarch_core_table)
                e.g.  shepherd-admin apps install retroarch mgba nestopia
                      shepherd-admin apps install retroarch --ppa mupen64plus-next

    okular      Okular, its extra format backends, and the default reading
                font (drives type = "ebook" entries). The backends matter:
                EPUB support is packaged separately from Okular itself, so
                without them a reading activity opens PDFs and refuses novels.
                Installs no books.

    android     Enable the Waydroid backend (drives type = "android" entries).
                Creates the $WAYDROID_GROUP group, adds USER to it if given,
                and prints the steps to install the Waydroid engine. The
                helper + polkit assets already ship with the package.

    companion   Shepherd Companion, the parent-facing admin app.
    media       Shepherd Media, the media player for phones/tablets/Fire TV.

Both Android apps are installed onto an attached Android device over adb. The
APK is built from this checkout's Gradle project when run from source, and
otherwise downloaded from the release matching the installed version — at
$SHEPHERD_FORGE_URL — and verified
against its .sha256 sidecar. Run them WITHOUT sudo: adb authorises devices per
user, so under sudo the device reports 'unauthorized'.

Options (companion/media):
    --device SERIAL   Install onto this device (default: the only one attached).
                      A HOST:PORT serial is connected to first, for a TV stick
                      reached over the network.
    --source          Force a build from this checkout, not a release download
    --release         Force a release download, even in a checkout
    --version X.Y.Z   Download this release instead of the installed version
    --apk PATH        Install a specific APK file

A phone that should keep itself updated is better served by the F-Droid
repository (docs/INSTALL.md); this is the sideload path, for Fire TV sticks and
development phones.
EOF
}

# Dispatch for `shepherd-admin apps <install> ...`.
apps_main() {
    local subcmd="${1:-}"
    shift || true
    case "$subcmd" in
        install)
            apps_install "$@"
            ;;
        ""|help|-h|--help)
            apps_usage
            ;;
        *)
            die "Unknown apps command: $subcmd (try: shepherd-admin apps help)"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# Per-user setup (config + group memberships)
# ---------------------------------------------------------------------------

# Deploy the example config and add the kiosk user to every group shepherd
# needs. This is the packaged equivalent of the from-source
# `shepherd install config` + `install groups`, plus the shepherd-firewall
# membership that `install all` adds via install_firewall. Depends on install.sh
# being sourced by the entrypoint (install_config, install_user_groups,
# add_user_to_groups, FIREWALL_GROUP, SHEPHERD_REQUIRED_GROUPS).
# `shepherd-admin policy USER --source PATH` -- install a policy, validated.
#
# The packaged face of `shepherd install policy`. On a device the policy lives
# with the state custodian and `sudoedit` is the other way in; this is the one
# that checks the file first, which matters because an unparseable policy is
# fatal at shepherdd's *startup* rather than on reload.
#
# `--source` is required here, unlike the from-source command, which defaults to
# the user's home copy. After migration that path holds a signpost rather than a
# policy, so defaulting to it would only ever produce the refusal.
admin_policy() {
    local user="" source_config=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --source) source_config="$2"; shift 2 ;;
            -h|--help|help)
                cat <<'EOF'
Usage: shepherd-admin policy USER --source PATH

Install PATH as USER's policy, validating it first. The state custodian holds
the policy shepherdd reads; shepherdd reloads within a second.

The other way to change it on a device is to edit the custodian's copy directly:

  sudoedit /var/lib/shepherdd/state/USER/config.toml

That takes effect the same way, but nothing checks it first.
EOF
                return 0
                ;;
            --*) die "Unknown option: $1" ;;
            *)
                if [[ -z "$user" ]]; then user="$1"; shift; else die "Unexpected argument: $1"; fi
                ;;
        esac
    done
    [[ -n "$user" ]] || die "Usage: shepherd-admin policy USER --source PATH"
    [[ -n "$source_config" ]] || die "shepherd-admin policy needs --source PATH"

    install_policy "$user" "$source_config" "true"
}

# `shepherd-admin migrate-state USER` -- move USER's state under the custodian.
#
# `setup-user` does this as part of setting a user up. This is the same step on
# its own, for a device that gained the custodian after its state was already
# in the home directory -- an upgrade, or a `setup-user` from before the
# packaged path did the migration. Idempotent: anything already migrated is
# reported and left.
admin_migrate_state() {
    local user="${1:-}"
    case "$user" in
        -h|--help|help|"")
            echo "Usage: shepherd-admin migrate-state USER"
            echo
            echo "Move USER's database, BLE admin record and policy under the state"
            echo "custodian, and enable its socket. Safe to re-run."
            return 0
            ;;
    esac
    require_root
    validate_user "$user"
    setup_state_for_user "$user"
}

# `shepherd-admin restore-state` -- move state back out of the custodian.
#
# Run before downgrading to a release without the custodian. The migration
# *moves* the database and the admin record, so an older shepherdd would find an
# empty home directory, open a fresh database and -- with no `admin.toml` --
# report itself unclaimed, letting the next phone to pair claim the device.
#
# Unlike `shepherd uninstall state --restore-to-home`, this leaves the binary
# and units alone: on a packaged device they belong to dpkg, and `apt` is what
# removes them.
admin_restore_state() {
    case "${1:-}" in
        -h|--help|help)
            echo "Usage: shepherd-admin restore-state"
            echo
            echo "Stop the state custodian and move every user's state back to their"
            echo "home directory. Run this BEFORE downgrading to a release that has no"
            echo "custodian, which would otherwise start from an empty database and an"
            echo "unclaimed device. The package's own files are left for apt to remove."
            return 0
            ;;
        "") ;;
        *) die "Unexpected argument: $1" ;;
    esac
    restore_state_to_home
}

setup_user() {
    local user=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --*) die "Unknown option: $1" ;;
            *)
                if [[ -z "$user" ]]; then user="$1"; shift; else die "Unexpected argument: $1"; fi
                ;;
        esac
    done

    if [[ -z "$user" ]]; then
        die "Usage: shepherd-admin setup-user USER"
    fi

    require_root
    validate_user "$user"

    # Config + media library (reads the examples via get_data_dir).
    install_config "$user" ""

    # input/video/bluetooth plus the shepherd-firewall system group that the
    # package's postinst (or install_firewall) created.
    add_user_to_groups "$user" "${SHEPHERD_REQUIRED_GROUPS[@]}" "$FIREWALL_GROUP"

    # The state custodian is per-user, so the package cannot set it up: the
    # kiosk user is not known at package time. This does the whole per-user half
    # -- the protected directory, the device's migrated state, and the socket --
    # because on a packaged system there is nowhere else to get it. The `.deb`
    # ships `shepherd-admin`, which has no `install` verb, so `shepherd install
    # state --user <user>` is a from-source command only.
    #
    # Enabling the socket alone was the earlier shape, and it was worse than it
    # looked: the custodian came up owning an empty directory while the device's
    # usage history, quota balances and BLE admin record stayed in the home
    # directory, unread. An upgrade looked like a factory reset, and the
    # diagnostic that noticed named a command a packaged device does not have.
    if command_exists systemctl && [[ -f "$STATED_UNIT_DIR/$STATED_SOCKET_UNIT" ]]; then
        info "Setting up the state custodian for $user..."
        setup_state_for_user "$user"
    fi

    success "Set up $user"
    info "Have $user log out and back in so the new group memberships apply,"
    info "then pick the \"Shepherd Kiosk\" session at login."
}

# ---------------------------------------------------------------------------
# Power-button behaviour
# ---------------------------------------------------------------------------

# systemd-logind drop-in that maps a short press of the hardware power button.
# The distro default is `poweroff`; a kiosk usually wants `suspend` (tap to
# sleep). A long press (HandlePowerKeyLongPress, left at the default) still
# powers off. shepherdd already listens for logind's PrepareForSleep to draw the
# suspend cover, so the device wakes back into the session cleanly.
POWER_KEY_DROPIN="/etc/systemd/logind.conf.d/10-shepherd-power-key.conf"

# Restart logind so a logind.conf change takes effect. Best-effort; on modern
# systemd active graphical sessions survive the restart.
_reload_logind() {
    if command_exists systemctl; then
        info "Restarting systemd-logind so the change takes effect..."
        systemctl restart systemd-logind 2>/dev/null \
            || warn "Could not restart systemd-logind; reboot for the change to apply"
    else
        warn "systemctl not found; the change applies after a reboot"
    fi
}

# Map the power button to suspend instead of shutdown.
power_key_suspend() {
    require_root
    ensure_dir "$(dirname "$POWER_KEY_DROPIN")" 0755
    cat > "$POWER_KEY_DROPIN" <<'EOF'
# Installed by `shepherd-admin power-key suspend`.
# Tap the hardware power button to suspend instead of powering off. A long press
# (HandlePowerKeyLongPress, left at the distro default) still powers off.
[Login]
HandlePowerKey=suspend
EOF
    chmod 0644 "$POWER_KEY_DROPIN"
    success "Power button now suspends on a short press ($POWER_KEY_DROPIN)"
    _reload_logind
}

# Remove the override, restoring the distro default (usually poweroff).
power_key_default() {
    require_root
    if [[ -f "$POWER_KEY_DROPIN" ]]; then
        rm -f "$POWER_KEY_DROPIN"
        success "Removed $POWER_KEY_DROPIN; power button reverts to the distro default"
        _reload_logind
    else
        info "No shepherd power-key override present ($POWER_KEY_DROPIN); nothing to do"
    fi
}

# Show the current override + what logind reports.
power_key_status() {
    if [[ -f "$POWER_KEY_DROPIN" ]]; then
        echo "shepherd power-key override: present ($POWER_KEY_DROPIN)"
        grep -E '^HandlePowerKey' "$POWER_KEY_DROPIN" 2>/dev/null || true
    else
        echo "shepherd power-key override: not installed (distro default, usually poweroff)"
    fi
    if command_exists loginctl; then
        loginctl show-manager -p HandlePowerKey 2>/dev/null || true
    fi
}

# Dispatch for `shepherd-admin power-key <suspend|default|status>`.
power_key_main() {
    local subcmd="${1:-}"
    shift || true
    case "$subcmd" in
        suspend)
            power_key_suspend
            ;;
        default|revert)
            power_key_default
            ;;
        status)
            power_key_status
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd-admin power-key <suspend|default|status>

Configures how a short press of the hardware power button behaves, via a
systemd-logind drop-in ($POWER_KEY_DROPIN).

Commands:
    suspend   Tap power to sleep instead of powering off (a long press still
              powers off).
    default   Remove the override; restore the distro default (usually poweroff).
    status    Show whether the override is installed and what logind reports.
EOF
            ;;
        *)
            die "Unknown power-key command: $subcmd (try: shepherd-admin power-key help)"
            ;;
    esac
}
