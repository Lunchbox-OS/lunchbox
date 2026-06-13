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

# Firewall helper paths (hardcoded -- the polkit .policy file references
# the absolute path to the helper binary, and polkit's own dirs are fixed
# system locations regardless of $prefix).
FIREWALL_HELPER_PATH="/usr/libexec/shepherd-firewall-helper"
POLKIT_ACTIONS_DIR="/usr/share/polkit-1/actions"
POLKIT_RULES_DIR="/etc/polkit-1/rules.d"
FIREWALL_POLICY_NAME="org.shepherd.firewall.policy"
FIREWALL_RULES_NAME="50-shepherd-firewall.rules"
FIREWALL_GROUP="shepherd-firewall"

# udev rules. Installed to a fixed system location regardless of --prefix
# (udev only reads /etc/udev/rules.d and /usr/lib/udev/rules.d). Currently
# just the /dev/uinput access rule the input-compat sidecars need.
UDEV_RULES_DIR="/etc/udev/rules.d"
UINPUT_RULES_NAME="71-shepherd-uinput.rules"

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
# - input: required by shepherd-touch-bridge, shepherd-tablet-bridge, and
#   shepherd-gamepad-bridge (used when an entry has `input_compat =
#   "touch_to_mouse"`, `"tablet_to_touch"`, or `gamepad_*`) so they can read
#   /dev/input/event* and write /dev/uinput.
#   The uinput write access also needs the udev rule installed by
#   `install_udev`; see that function and dist/udev/.
# - video: required by the brightness slider. `brightnessctl`'s udev rule
#   grants `video` write access to /sys/class/backlight/*/brightness; the
#   daemon runs as the desktop user, so without this membership every
#   brightness write fails with EACCES even though brightnessctl is
#   installed.
#
# Add new groups here as features need them; install_user_groups walks the
# array and skips memberships the user already has.
SHEPHERD_REQUIRED_GROUPS=(
    "input"
    "video"
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

# Install the udev rules shepherd-launcher needs.
#
# Currently just the /dev/uinput access rule. The input-compat sidecars
# synthesize their mouse/keyboard output through /dev/uinput, which is
# root-only by default; the rule grants the 'input' group access (the same
# group `install_user_groups` already adds the user to). This is what lets
# the sidecars run on any Wayland compositor, not just wlroots ones.
install_udev() {
    local destdir="${DESTDIR:-}"
    local repo_root
    repo_root="$(get_repo_root)"

    require_root

    local rules_src="$repo_root/dist/udev/$UINPUT_RULES_NAME"
    local rules_dst="$destdir$UDEV_RULES_DIR/$UINPUT_RULES_NAME"

    if [[ ! -f "$rules_src" ]]; then
        die "udev rule missing at $rules_src"
    fi

    info "Installing udev rule to $rules_dst..."
    ensure_dir "$(dirname "$rules_dst")" 0755
    install -m 0644 -o root -g root "$rules_src" "$rules_dst"

    # Reload + apply only on a real (non-packaging) install. Under DESTDIR
    # these would touch the build host, which is wrong for packaging.
    if [[ -z "$destdir" ]]; then
        if command -v udevadm >/dev/null 2>&1; then
            info "Reloading udev rules so the new rule takes effect"
            udevadm control --reload-rules 2>/dev/null \
                || warn "Could not reload udev rules; reboot for the rule to apply"
            # Nudge the static node into existence with the new ownership.
            udevadm trigger /dev/uinput 2>/dev/null || true
        else
            warn "udevadm not found; reboot for the /dev/uinput rule to apply"
        fi
    fi

    success "Installed udev rule"
}

# Install the firewall helper and its polkit assets.
#
# Args:
#   $1 -- target user to add to the shepherd-firewall group (optional;
#         skipped when DESTDIR is set so packaging doesn't mutate hosts)
#   $2 -- "true" for release binary, "false" for debug (default: true)
install_firewall() {
    local user="${1:-}"
    local release="${2:-true}"
    local destdir="${DESTDIR:-}"
    local repo_root
    repo_root="$(get_repo_root)"

    require_root

    local helper_src
    helper_src="$(get_target_dir "$release")/shepherd-firewall-helper"
    local helper_dst="$destdir$FIREWALL_HELPER_PATH"
    local policy_src="$repo_root/dist/polkit/$FIREWALL_POLICY_NAME"
    local policy_dst="$destdir$POLKIT_ACTIONS_DIR/$FIREWALL_POLICY_NAME"
    local rules_src="$repo_root/dist/polkit/$FIREWALL_RULES_NAME"
    local rules_dst="$destdir$POLKIT_RULES_DIR/$FIREWALL_RULES_NAME"

    if [[ ! -x "$helper_src" ]]; then
        if [[ "$release" == "true" ]]; then
            die "shepherd-firewall-helper not found at $helper_src; run 'shepherd build --release' first"
        else
            die "shepherd-firewall-helper not found at $helper_src; run 'cargo build --bin shepherd-firewall-helper' first"
        fi
    fi
    if [[ ! -f "$policy_src" || ! -f "$rules_src" ]]; then
        die "Polkit assets missing at $repo_root/dist/polkit/"
    fi

    info "Installing firewall helper to $helper_dst..."
    ensure_dir "$(dirname "$helper_dst")" 0755
    install -m 0755 -o root -g root "$helper_src" "$helper_dst"

    info "Installing polkit policy to $policy_dst..."
    ensure_dir "$(dirname "$policy_dst")" 0755
    install -m 0644 -o root -g root "$policy_src" "$policy_dst"

    info "Installing polkit rule to $rules_dst..."
    ensure_dir "$(dirname "$rules_dst")" 0755
    install -m 0644 -o root -g root "$rules_src" "$rules_dst"

    # Group + user membership + polkit reload only on a real (non-packaging)
    # install. Under DESTDIR these would mutate the build host and the
    # resulting package, which is wrong.
    if [[ -z "$destdir" ]]; then
        if ! getent group "$FIREWALL_GROUP" >/dev/null; then
            info "Creating system group: $FIREWALL_GROUP"
            groupadd --system "$FIREWALL_GROUP"
        fi
        if [[ -n "$user" ]]; then
            if id -nG "$user" 2>/dev/null | tr ' ' '\n' | grep -qx "$FIREWALL_GROUP"; then
                info "$user is already a member of $FIREWALL_GROUP"
            else
                info "Adding $user to $FIREWALL_GROUP"
                usermod -aG "$FIREWALL_GROUP" "$user"
                warn "$user must log out and back in for the new group membership to take effect."
            fi
        fi
        if systemctl is-active --quiet polkit 2>/dev/null; then
            info "Reloading polkit so the new rule takes effect"
            systemctl reload polkit 2>/dev/null \
                || systemctl restart polkit 2>/dev/null \
                || warn "Could not reload polkit; restart it manually for the rule to apply"
        fi
    fi

    success "Firewall helper installed"
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
    install_firewall "$user" "true"
    install_sway_config "$prefix"
    install_desktop_entry "$prefix"
    install_config "$user" "" "$force"
    install_user_groups "$user"
    install_udev

    success "Installation complete!"
    info ""
    info "Next steps:"
    info "  1. Edit user config at ~$user/.config/shepherd/config.toml"
    info "  2. Have $user log out and back in (so the new shepherd-firewall"
    info "     group membership takes effect for per-entry firewall rules)"
    info "  3. Select 'Shepherd Kiosk' session at login"
    info "  4. Optionally run 'shepherd harden apply --user $user' for kiosk mode"
}

# Main install command dispatcher
install_main() {
    local subcmd="${1:-}"
    shift || true
    
    local user=""
    local prefix="$DEFAULT_PREFIX"
    local source_config=""
    local force="false"
    local release="true"

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
            --debug)
                release="false"
                shift
                ;;
            --release)
                release="true"
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
        firewall)
            install_firewall "$user" "$release"
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
        udev)
            install_udev
            ;;
        all)
            install_all "$user" "$prefix" "$force"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd install <command> [OPTIONS]

Commands:
    bins              Install release binaries
    firewall          Install the privileged firewall helper + polkit rule
    config            Deploy user configuration
    sway-config       Install sway configuration
    desktop-entry     Install display manager desktop entry
    groups            Add the target user to required groups (e.g. 'input'
                      for touch-to-mouse compatibility)
    udev              Install udev rules (e.g. /dev/uinput access for the
                      input-compat sidecars)
    all               Install everything (incl. firewall helper)

Options:
    --user USER       Target user for config / groups / firewall group
                      (required for config / groups / all; optional for
                      firewall)
    --prefix PREFIX   Installation prefix (default: $DEFAULT_PREFIX)
    --source CONFIG   Source config file (default: config.example.toml)
    --force, -f       Overwrite existing configuration files
    --release         Use release binaries (default)
    --debug           Use debug binaries (for 'firewall' during development)

Environment:
    DESTDIR           Installation root for packaging (default: empty).
                      When set, 'firewall' skips groupadd/usermod/polkit-reload.

Notes:
    The firewall helper installs to a fixed system path
    ($FIREWALL_HELPER_PATH) regardless of --prefix, because polkit's
    .policy file references the helper by absolute path and polkit's
    own directories are not relocatable.

Examples:
    shepherd install bins --prefix /usr/local
    shepherd install firewall --user kiosk
    shepherd install config --user kiosk --force
    shepherd install groups --user kiosk
    shepherd install udev
    shepherd install all --user kiosk --prefix /usr
EOF
            ;;
        *)
            die "Unknown install command: $subcmd (try: shepherd install help)"
            ;;
    esac
}
