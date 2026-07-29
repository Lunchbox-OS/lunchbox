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
# setup_user needs install_config / install_user_groups / add_user_to_groups
# from install.sh and FIREWALL_GROUP; the entrypoints source install.sh, so
# those are available at call time. (deps.sh sources this file only for
# install_media_deps and its yt-dlp/VA-API halves, and never calls setup_user.)

# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

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
# mpv's `hwdec=auto-safe` finds nothing and shepherd-media decodes every frame
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

# Install a supported activity backend, using whichever packaging shepherd's
# integration actually expects for it. These are NOT both flatpaks: the
# type="steam" adapter drives Canonical's Steam *snap* (config.example.toml
# documents `snap install steam`), while Chrome is wrapped as the Flathub
# flatpak `com.google.Chrome`. Add rows here as new backends are supported.
apps_install() {
    local app="${1:-}"
    case "$app" in
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
        chrome)
            require_root
            ensure_flathub
            info "Installing com.google.Chrome from Flathub..."
            flatpak install -y flathub com.google.Chrome
            success "Installed com.google.Chrome"
            info "Reference it with kind = \"flatpak\", app_id = \"com.google.Chrome\"."
            ;;
        ""|help|-h|--help)
            apps_usage
            return 0
            ;;
        *)
            die "Unknown app '$app' (supported: steam, chrome)"
            ;;
    esac
}

apps_usage() {
    cat <<EOF
Usage: shepherd-admin apps install <steam|chrome>

Installs a supported activity backend with the packaging shepherd's integration
expects (they differ):

    steam    Canonical's Steam snap (drives type = "steam" entries). Launch it
             and log in once before those entries will work. Also permits
             unprivileged user namespaces (Ubuntu restricts them by default),
             which Steam's sandbox needs — see $USERNS_DROPIN.
    chrome   com.google.Chrome from Flathub (for kind = "flatpak" entries).
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
setup_user() {
    local user=""
    local force="false"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --force|-f) force="true"; shift ;;
            --*) die "Unknown option: $1" ;;
            *)
                if [[ -z "$user" ]]; then user="$1"; shift; else die "Unexpected argument: $1"; fi
                ;;
        esac
    done

    if [[ -z "$user" ]]; then
        die "Usage: shepherd-admin setup-user USER [--force]"
    fi

    require_root
    validate_user "$user"

    # Config + media library (reads the examples via get_data_dir).
    install_config "$user" "" "$force"

    # input/video/bluetooth plus the shepherd-firewall system group that the
    # package's postinst (or install_firewall) created.
    add_user_to_groups "$user" "${SHEPHERD_REQUIRED_GROUPS[@]}" "$FIREWALL_GROUP"

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
