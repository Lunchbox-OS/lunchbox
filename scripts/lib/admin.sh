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
# those are available at call time. (deps.sh sources this file only for the
# yt-dlp helpers and never calls setup_user.)

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
# Flatpak apps
# ---------------------------------------------------------------------------

FLATHUB_REMOTE_URL="https://flathub.org/repo/flathub.flatpakrepo"

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
             and log in once before those entries will work.
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
