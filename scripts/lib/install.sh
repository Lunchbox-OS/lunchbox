#!/usr/bin/env bash
# Installation logic for shepherd-launcher
# Handles binary installation, config deployment, and desktop entry setup

# Get the directory containing this script
INSTALL_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source common utilities
# shellcheck source=common.sh
source "$INSTALL_LIB_DIR/common.sh"

# Source build utilities for binary paths
# shellcheck source=build.sh
source "$INSTALL_LIB_DIR/build.sh"

# Default installation paths
DEFAULT_PREFIX="/usr/local"
DEFAULT_BINDIR="bin"

# Standard sway config location
SWAY_CONFIG_DIR="/etc/sway"
SHEPHERD_SWAY_CONFIG="shepherd.conf"
SHEPHERD_SWAY_CONFD="shepherd.conf.d"

# Desktop entry location
DESKTOP_ENTRY_DIR="share/wayland-sessions"
DESKTOP_ENTRY_NAME="shepherd.desktop"

# Install release binaries
install_bins() {
    local prefix="${1:-$DEFAULT_PREFIX}"
    local destdir="${DESTDIR:-}"
    
    require_root
    
    local bindir="$destdir$prefix/$DEFAULT_BINDIR"
    local target_dir
    target_dir="$(get_target_dir true)"
    
    # Ensure release build exists
    if ! binaries_exist true; then
        die "Release binaries not found. Run 'shepherd build --release' first."
    fi
    
    info "Installing binaries to $bindir..."
    
    ensure_dir "$bindir" 0755
    
    for binary in "${SHEPHERD_BINARIES[@]}"; do
        local src="$target_dir/$binary"
        local dst="$bindir/$binary"
        
        info "  Installing $binary..."
        install -m 0755 "$src" "$dst"
    done
    
    success "Installed binaries to $bindir"
}

# Install the sway configuration
install_sway_config() {
    local destdir="${DESTDIR:-}"
    local repo_root
    repo_root="$(get_repo_root)"
    
    require_root
    
    local src_config="$repo_root/sway.conf"
    local dst_dir="$destdir$SWAY_CONFIG_DIR"
    local dst_config="$dst_dir/$SHEPHERD_SWAY_CONFIG"
    local dst_confd="$dst_dir/$SHEPHERD_SWAY_CONFD"

    if [[ ! -f "$src_config" ]]; then
        die "Source sway.conf not found at $src_config"
    fi

    info "Installing sway configuration to $dst_config..."

    ensure_dir "$dst_dir" 0755
    # Drop-in directory for site-local overrides (e.g. HiDPI scale).
    # ensure_dir is a no-op if it already exists, so user-placed files are
    # preserved across re-installs.
    ensure_dir "$dst_confd" 0755
    
    # Create a production version of the sway config
    # Replace debug paths with installed paths
    local prefix="${1:-$DEFAULT_PREFIX}"
    local bindir="$prefix/$DEFAULT_BINDIR"
    
    # Copy and modify the config for production use
    sed \
        -e "s|./target/debug/shepherd-launcher|$bindir/shepherd-launcher|g" \
        -e "s|./target/debug/shepherd-hud|$bindir/shepherd-hud|g" \
        -e "s|./target/debug/shepherdd|$bindir/shepherdd|g" \
        -e "s|./config.example.toml|~/.config/shepherd/config.toml|g" \
        -e "s|-c ./sway.conf|-c $dst_config|g" \
        "$src_config" > "$dst_config"
    
    chmod 0644 "$dst_config"
    
    success "Installed sway configuration"
}

# Install desktop entry for display manager
install_desktop_entry() {
    local prefix="${1:-$DEFAULT_PREFIX}"
    local destdir="${DESTDIR:-}"
    
    require_root
    
    local dst_dir="$destdir$prefix/$DESKTOP_ENTRY_DIR"
    local dst_entry="$dst_dir/$DESKTOP_ENTRY_NAME"
    
    info "Installing desktop entry to $dst_entry..."
    
    ensure_dir "$dst_dir" 0755
    
    cat > "$dst_entry" <<EOF
[Desktop Entry]
Name=Shepherd Kiosk
Comment=Shepherd game launcher kiosk mode
Exec=sway -c $SWAY_CONFIG_DIR/$SHEPHERD_SWAY_CONFIG --unsupported-gpu
Type=Application
DesktopNames=shepherd
EOF
    
    chmod 0644 "$dst_entry"
    
    success "Installed desktop entry"
}

# Deploy user configuration
install_config() {
    local user="${1:-}"
    local source_config="${2:-}"
    local force="${3:-false}"
    
    if [[ -z "$user" ]]; then
        die "Usage: shepherd install config --user USER [--source CONFIG] [--force]"
    fi
    
    validate_user "$user"
    
    local repo_root
    repo_root="$(get_repo_root)"
    
    # Default source is the example config
    if [[ -z "$source_config" ]]; then
        source_config="$repo_root/config.example.toml"
    fi
    
    if [[ ! -f "$source_config" ]]; then
        die "Source config not found: $source_config"
    fi
    
    # Get user's config directory
    local user_home
    user_home="$(get_user_home "$user")"
    local user_config_dir="$user_home/.config/shepherd"
    local dst_config="$user_config_dir/config.toml"
    
    info "Installing user config to $dst_config..."
    
    # Create config directory owned by user
    maybe_sudo mkdir -p "$user_config_dir"
    maybe_sudo chown "$user:$user" "$user_config_dir"
    maybe_sudo chmod 0755 "$user_config_dir"
    
    # Check if config already exists
    if maybe_sudo test -f "$dst_config"; then
        if [[ "$force" == "true" ]]; then
            warn "Overwriting existing config at $dst_config"
            maybe_sudo cp "$source_config" "$dst_config"
            maybe_sudo chown "$user:$user" "$dst_config"
            maybe_sudo chmod 0644 "$dst_config"
            success "Overwrote user configuration for $user"
        else
            warn "Config file already exists at $dst_config, skipping (use --force to overwrite)"
        fi
    else
        # Copy config file
        maybe_sudo cp "$source_config" "$dst_config"
        maybe_sudo chown "$user:$user" "$dst_config"
        maybe_sudo chmod 0644 "$dst_config"
        success "Installed user configuration for $user"
    fi

    # The example config references `~/.config/shepherd/movies.toml` for the
    # shepherd-media entries. If the user picked the default example config,
    # also drop the matching example library so the entries don't 404 on
    # first launch. The user is still expected to edit the URIs.
    local source_library="$repo_root/movies-library.example.toml"
    local dst_library="$user_config_dir/movies.toml"
    if [[ -f "$source_library" ]]; then
        if maybe_sudo test -f "$dst_library"; then
            if [[ "$force" == "true" ]]; then
                warn "Overwriting existing media library at $dst_library"
                maybe_sudo cp "$source_library" "$dst_library"
                maybe_sudo chown "$user:$user" "$dst_library"
                maybe_sudo chmod 0644 "$dst_library"
                success "Overwrote media library for $user"
            else
                info "Media library already exists at $dst_library, skipping"
            fi
        else
            maybe_sudo cp "$source_library" "$dst_library"
            maybe_sudo chown "$user:$user" "$dst_library"
            maybe_sudo chmod 0644 "$dst_library"
            success "Installed media library for $user (edit URIs before use)"
        fi
    fi
}

# Groups the kiosk user must belong to for shepherd-launcher features.
#
# - input: required by shepherd-touch-bridge (used when an entry has
#   `input_compat = "touch_to_mouse"`) so it can read /dev/input/event*.
#
# Add new groups here as features need them; install_user_groups walks the
# array and skips memberships the user already has.
SHEPHERD_REQUIRED_GROUPS=(
    "input"
)

# Add the target user to all groups required by shepherd-launcher.
# Idempotent: skips any group the user is already in.
install_user_groups() {
    local user="${1:-}"

    if [[ -z "$user" ]]; then
        die "Usage: shepherd install groups --user USER"
    fi

    require_root
    validate_user "$user"

    local current_groups
    current_groups="$(id -nG "$user")"

    local changed=false
    for group in "${SHEPHERD_REQUIRED_GROUPS[@]}"; do
        # Skip groups that don't exist on this system. We don't create
        # them — they're expected to come from the distro.
        if ! getent group "$group" >/dev/null 2>&1; then
            warn "Group '$group' does not exist on this system; skipping"
            continue
        fi

        # ` $group ` matches the group as a whole token in the space-
        # separated list returned by `id -nG`.
        if [[ " $current_groups " == *" $group "* ]]; then
            info "User '$user' is already in group '$group'"
            continue
        fi

        info "Adding user '$user' to group '$group'..."
        usermod -aG "$group" "$user"
        changed=true
    done

    if [[ "$changed" == "true" ]]; then
        success "Updated group memberships for $user"
        info "Group changes take effect on the user's next login."
    else
        success "User '$user' already has all required group memberships"
    fi
}

# Install everything
install_all() {
    local user="${1:-}"
    local prefix="${2:-$DEFAULT_PREFIX}"
    local force="${3:-false}"

    if [[ -z "$user" ]]; then
        die "Usage: shepherd install all --user USER [--prefix PREFIX] [--force]"
    fi

    require_root
    validate_user "$user"

    info "Installing shepherd-launcher (prefix: $prefix)..."

    install_bins "$prefix"
    install_sway_config "$prefix"
    install_desktop_entry "$prefix"
    install_config "$user" "" "$force"
    install_user_groups "$user"

    success "Installation complete!"
    info ""
    info "Next steps:"
    info "  1. Edit user config at ~$user/.config/shepherd/config.toml"
    info "  2. Select 'Shepherd Kiosk' session at login"
    info "  3. Optionally run 'shepherd harden apply --user $user' for kiosk mode"
}

# Main install command dispatcher
install_main() {
    local subcmd="${1:-}"
    shift || true
    
    local user=""
    local prefix="$DEFAULT_PREFIX"
    local source_config=""
    local force="false"
    
    # Parse remaining arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --user)
                user="$2"
                shift 2
                ;;
            --prefix)
                prefix="$2"
                shift 2
                ;;
            --source)
                source_config="$2"
                shift 2
                ;;
            --force|-f)
                force="true"
                shift
                ;;
            *)
                die "Unknown option: $1"
                ;;
        esac
    done
    
    case "$subcmd" in
        bins)
            install_bins "$prefix"
            ;;
        config)
            install_config "$user" "$source_config" "$force"
            ;;
        sway-config)
            install_sway_config "$prefix"
            ;;
        desktop-entry)
            install_desktop_entry "$prefix"
            ;;
        groups)
            install_user_groups "$user"
            ;;
        all)
            install_all "$user" "$prefix" "$force"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd install <command> [OPTIONS]

Commands:
    bins              Install release binaries
    config            Deploy user configuration
    sway-config       Install sway configuration
    desktop-entry     Install display manager desktop entry
    groups            Add the target user to required groups (e.g. 'input'
                      for touch-to-mouse compatibility)
    all               Install everything

Options:
    --user USER       Target user for config/groups deployment (required for config/groups/all)
    --prefix PREFIX   Installation prefix (default: $DEFAULT_PREFIX)
    --source CONFIG   Source config file (default: config.example.toml)
    --force, -f       Overwrite existing configuration files

Environment:
    DESTDIR           Installation root for packaging (default: empty)

Examples:
    shepherd install bins --prefix /usr/local
    shepherd install config --user kiosk
    shepherd install config --user kiosk --force
    shepherd install groups --user kiosk
    shepherd install all --user kiosk --prefix /usr
EOF
            ;;
        *)
            die "Unknown install command: $subcmd (try: shepherd install help)"
            ;;
    esac
}
