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

# Install one of the supported Flathub apps, system-wide, so the kiosk user can
# launch it. Only the apps shepherd's example config references are supported;
# add rows here as needed.
apps_install() {
    local app="${1:-}"
    local id
    case "$app" in
        steam)  id="com.valvesoftware.Steam" ;;
        chrome) id="com.google.Chrome" ;;
        ""|help|-h|--help)
            apps_usage
            return 0
            ;;
        *)
            die "Unknown app '$app' (supported: steam, chrome)"
            ;;
    esac

    require_root
    ensure_flathub
    info "Installing $id from Flathub (system-wide)..."
    flatpak install -y flathub "$id"
    success "Installed $id"
    info "Reference it from an entry with kind = flatpak, app_id = \"$id\"."
}

apps_usage() {
    cat <<EOF
Usage: shepherd-admin apps install <steam|chrome>

Installs a supported activity backend from Flathub, system-wide (adds the
Flathub remote first if missing). Then reference it from a config entry:

    [entries.kind]
    type = "flatpak"
    app_id = "com.valvesoftware.Steam"   # or com.google.Chrome
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
