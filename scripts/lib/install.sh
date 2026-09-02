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

# Distro package name. Lives here rather than in package.sh because the
# uninstall path needs it to point at `apt purge`, and package.sh sources
# this file (not the other way round).
DISTRO_PACKAGE_NAME="shepherd-launcher"

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

# systemd drop-in that runs bluetoothd with experimental D-Bus interfaces,
# which is what exposes `Device1.PreferredBearer` — see install_bluetooth_dropin
# and dist/systemd/. A drop-in rather than an edit to /etc/bluetooth/main.conf
# because BlueZ has no conf.d: it reads exactly one file, so shipping config
# there means fighting the distro's conffile on every upgrade.
BLUETOOTH_DROPIN_DIR="/etc/systemd/system/bluetooth.service.d"
BLUETOOTH_DROPIN_NAME="10-shepherd-bluetooth-experimental.conf"
# Used only when the target's own unit can't be read (i.e. under DESTDIR,
# where we are staging on a build host rather than the eventual machine).
BLUETOOTHD_DEFAULT_PATH="/usr/libexec/bluetooth/bluetoothd"

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
    
    # Copy and modify the config for production use.
    #
    # `--no-harden-sway-ipc` is stripped here (issue #144). shepherdd hardens by
    # default: once it has connected it unlinks sway's IPC socket, and nothing
    # else can reach the compositor for the rest of the session. That matters
    # because sway's IPC hands any process running as shepherdd's uid — which is
    # every activity — `exec`, which starts a process outside supervision *and*
    # outside the cgroup the per-entry firewall is attached to.
    #
    # `--trust-env-binaries` is stripped for the same reason again: it lets the
    # environment name the binaries shepherdd execs, and on a device the kiosk
    # user chooses the environment (GDM's PAM stack reads `~/.pam_environment`).
    # Only the e2e suite passes it, but a hand-edited config could.
    #
    # `--no-restrict-ipc-peers` is stripped for the same reason. It opens
    # shepherdd's *own* management socket to every process at this uid, which is
    # every activity: without the check a game can call `logout`, `stop_current`
    # or `launch`. It is in `sway.conf` because a dev stack runs entirely inside
    # one shell's cgroup, where the check cannot mean anything.
    #
    # `sway.conf` carries both flags because it is the development config, where
    # the unlink would take the socket away from `swaymsg` and the headless
    # harness. An installed kiosk wants the defaults, so they come back out
    # here — and the checks below are the ones that matter: a rename upstream
    # that silently left one in would ship an unhardened device.
    sed \
        -e "s|./target/debug/shepherd-launcher|$bindir/shepherd-launcher|g" \
        -e "s|./target/debug/shepherd-hud|$bindir/shepherd-hud|g" \
        -e "s|./target/debug/shepherdd|$bindir/shepherdd|g" \
        -e "s|./config.example.toml|~/.config/shepherd/config.toml|g" \
        -e "s|-c ./sway.conf|-c $dst_config|g" \
        -e "s| --no-harden-sway-ipc||g" \
        -e "s| --no-restrict-ipc-peers||g" \
        -e "s| --trust-env-binaries||g" \
        "$src_config" > "$dst_config"

    # Scoped to the exec line: the comment above it names the flag too, and a
    # whole-file grep would fail an install that had stripped it correctly.
    local dst_exec_line
    dst_exec_line="$(grep -E "^exec .*shepherdd -c [^ ]+" "$dst_config" || true)"
    if [[ -z "$dst_exec_line" ]]; then
        die "No 'shepherdd -c <path>' exec line in $dst_config (sway.conf's shepherdd exec line may have changed)"
    fi
    if [[ "$dst_exec_line" == *--no-harden-sway-ipc* ]]; then
        die "Failed to strip --no-harden-sway-ipc from $dst_config; the installed device would leave sway's IPC socket reachable by every activity (issue #144)"
    fi
    if [[ "$dst_exec_line" == *--no-restrict-ipc-peers* ]]; then
        die "Failed to strip --no-restrict-ipc-peers from $dst_config; the installed device would let every activity drive shepherd's own management socket (issue #144)"
    fi
    if [[ "$dst_exec_line" == *--trust-env-binaries* ]]; then
        die "Failed to strip --trust-env-binaries from $dst_config; the installed device would take helper binaries from a \$PATH the kiosk user can write (issue #144)"
    fi
    
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

    # Example configs live at the repo root in a source checkout and under
    # /usr/share/shepherd on a packaged install; get_data_dir picks the right
    # one so `shepherd-admin setup-user` works without a source tree.
    local repo_root
    repo_root="$(get_data_dir)"

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
# - bluetooth: required by the BLE management transport (shepherd-ble).
#   BlueZ's polkit rules grant the `bluetooth` group permission to call
#   org.bluez.Adapter1.SetPairable and AgentManager1.RegisterAgent over
#   the system bus; without membership the BLE startup fails on the
#   first adapter call and the daemon never advertises.
#
# Add new groups here as features need them; install_user_groups walks the
# array and skips memberships the user already has.
SHEPHERD_REQUIRED_GROUPS=(
    "input"
    "video"
    "bluetooth"
)

# Add $user to each named group, skipping groups that don't exist on this
# system or that the user already belongs to. Shared by install_user_groups
# (from-source `install all`) and setup_user (shepherd-admin, .deb path) so the
# membership logic lives in one place.
add_user_to_groups() {
    local user="$1"
    shift

    local current_groups
    current_groups="$(id -nG "$user")"

    local changed=false
    local group
    for group in "$@"; do
        # Skip groups that don't exist on this system. We don't create
        # them — they're expected to come from the distro (or, for
        # shepherd-firewall, from install/packaging).
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
        info "Group changes take effect on the user's next login."
    fi
}

# Add the target user to all groups required by shepherd-launcher.
# Idempotent: skips any group the user is already in.
install_user_groups() {
    local user="${1:-}"

    if [[ -z "$user" ]]; then
        die "Usage: shepherd install groups --user USER"
    fi

    require_root
    validate_user "$user"

    add_user_to_groups "$user" "${SHEPHERD_REQUIRED_GROUPS[@]}"
    success "Updated group memberships for $user"
}

# Resolve the path bluetoothd is actually started from, so the drop-in's
# ExecStart matches the distro rather than a guess. Getting this wrong is
# not a cosmetic error — a drop-in pointing at a non-existent binary stops
# Bluetooth working entirely — so we read it back from the unit that is
# installed, and only fall back to the well-known Ubuntu path when there is
# no unit to read (staging under DESTDIR on a build host).
_bluetoothd_exec_path() {
    local line
    line="$(systemctl cat bluetooth.service 2>/dev/null \
        | awk -F= '/^ExecStart=./ { sub(/^ExecStart=/, ""); print; exit }')"
    # ExecStart may carry arguments and systemd's own prefix characters.
    line="${line#[-@:+!]}"
    line="${line%% *}"

    if [[ -n "$line" ]]; then
        printf '%s\n' "$line"
    else
        printf '%s\n' "$BLUETOOTHD_DEFAULT_PATH"
    fi
}

# Install the bluetoothd drop-in that enables experimental D-Bus interfaces.
#
# shepherd needs `Device1.PreferredBearer` to pin the admin phone to the
# BR/EDR bearer; without it BlueZ arms the kernel to auto-connect the phone
# over LE, the device ends up central, and the companion can never encrypt
# the link (see dist/systemd/ for the full story). shepherd degrades
# gracefully without this — it says so in the log and falls back — so a box
# with no BlueZ at all is skipped rather than treated as an error.
install_bluetooth_dropin() {
    local destdir="${DESTDIR:-}"
    local repo_root
    repo_root="$(get_repo_root)"

    require_root

    local template="$repo_root/dist/systemd/$BLUETOOTH_DROPIN_NAME"
    local dropin_dst="$destdir$BLUETOOTH_DROPIN_DIR/$BLUETOOTH_DROPIN_NAME"

    if [[ ! -f "$template" ]]; then
        die "Bluetooth drop-in missing at $template"
    fi

    # No bluetooth unit and not staging a package? Nothing to extend.
    if [[ -z "$destdir" ]] && ! systemctl cat bluetooth.service >/dev/null 2>&1; then
        info "No bluetooth.service on this system; skipping the bluetoothd drop-in"
        return 0
    fi

    local bluetoothd
    bluetoothd="$(_bluetoothd_exec_path)"

    info "Installing bluetoothd drop-in to $dropin_dst (ExecStart=$bluetoothd -E)..."
    ensure_dir "$(dirname "$dropin_dst")" 0755
    sed "s|@BLUETOOTHD@|$bluetoothd|" "$template" >"$dropin_dst"
    chmod 0644 "$dropin_dst"
    chown root:root "$dropin_dst"

    # Reload + restart only on a real (non-packaging) install; under DESTDIR
    # these would touch the build host.
    #
    # NOTE: the .deb runs these same steps from its generated postinst
    # instead (scripts/lib/package.sh, _package_write_control). Keep them in
    # sync.
    if [[ -z "$destdir" ]]; then
        systemctl daemon-reload 2>/dev/null \
            || warn "Could not reload systemd; the drop-in applies at next boot"
        if systemctl is-active --quiet bluetooth.service; then
            info "Restarting bluetooth to apply (briefly drops Bluetooth connections)"
            systemctl restart bluetooth.service 2>/dev/null \
                || warn "Could not restart bluetooth; restart it or reboot to apply"
        fi
    fi

    success "Installed bluetoothd drop-in"
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
    #
    # NOTE: the .deb runs these same steps from its generated postinst instead
    # (scripts/lib/package.sh, _package_write_control). If you change the
    # udev reload/trigger here, mirror it there.
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
    #
    # NOTE: the .deb runs the group-create + polkit-reload from its generated
    # postinst instead (scripts/lib/package.sh, _package_write_control), and
    # emits the per-user usermod as printed guidance. Keep them in sync.
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

# Install the system-wide components: everything that is host-global and
# DESTDIR-safe. This is the single source of truth for "what a system install
# places" — `install_all` (from-source) and `package_deb` (.deb staging, in
# scripts/lib/package.sh) both call it, so a new system component is added in
# exactly one place. The per-user steps (config, groups) are deliberately NOT
# here: install_all appends them, and the package leaves them to the admin.
#
# Args:
#   $1 -- install prefix (default: $DEFAULT_PREFIX)
#   $2 -- user to add to the shepherd-firewall group (empty under DESTDIR)
install_system() {
    local prefix="${1:-$DEFAULT_PREFIX}"
    local firewall_user="${2:-}"

    install_bins "$prefix"
    install_firewall "$firewall_user" "true"
    install_sway_config "$prefix"
    install_desktop_entry "$prefix"
    install_udev
    install_bluetooth_dropin
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

    install_system "$prefix" "$user"
    install_config "$user" "" "$force"
    install_user_groups "$user"

    success "Installation complete!"
    info ""
    info "Next steps:"
    info "  1. Edit user config at ~$user/.config/shepherd/config.toml"
    info "  2. Have $user log out and back in (so the new shepherd-firewall"
    info "     group membership takes effect for per-entry firewall rules)"
    info "  3. Select 'Shepherd Kiosk' session at login"
    info "  4. Optionally run 'shepherd harden apply --user $user' for kiosk mode"
}

# --- Uninstall -------------------------------------------------------------
#
# The uninstall_* functions below reverse the matching install_* steps. They
# only remove host-global, shepherd-owned files; they deliberately leave user
# data alone (per-user config under ~/.config/shepherd) and group memberships
# (input/video/bluetooth/shepherd-firewall) in place, since removing those can
# affect a user's other software and isn't reversible from a backup. Each
# function is a no-op for files that are already gone, so uninstall is safe to
# re-run and safe to run after a partial install.

# Count of files this uninstall left alone because dpkg owns them, so
# uninstall_all can say so once at the end rather than only per-file.
UNINSTALL_SKIPPED_OWNED=0

# True when dpkg tracks $1 as belonging to an installed package.
#
# A source uninstall must never delete a file the package manager owns.
# The .deb ships four of them as *conffiles* -- /etc/sway/shepherd.conf,
# the polkit rule, the udev rule and the bluetoothd drop-in -- and dpkg
# records a hash for each. Delete one behind dpkg's back and it reads the
# absence as "the admin removed this deliberately", so it will not put the
# file back, not even on `apt install --reinstall` of the same version.
# The result is a box that reports itself installed while missing its sway
# config (the session bounces straight back to the greeter) and its
# bluetoothd drop-in (the bearer pin silently cannot apply).
#
# Recovering from that needs --force-confmiss; see docs/INSTALL.md.
path_owned_by_dpkg() {
    local path="$1"
    # Under DESTDIR the paths point into a staging tree, where ownership is
    # meaningless and dpkg must not be consulted.
    [[ -z "${DESTDIR:-}" ]] || return 1
    command_exists dpkg-query || return 1
    dpkg-query -S "$path" >/dev/null 2>&1
}

# Remove a single file if it exists, logging what happened.
#
# Files owned by an installed package are left for that package manager to
# remove; see path_owned_by_dpkg for why deleting them is actively harmful.
remove_path() {
    local path="$1"
    if path_owned_by_dpkg "$path"; then
        warn "  Leaving $path (owned by an installed package)"
        UNINSTALL_SKIPPED_OWNED=$((UNINSTALL_SKIPPED_OWNED + 1))
        return 0
    fi
    if [[ -e "$path" || -L "$path" ]]; then
        info "  Removing $path"
        rm -f "$path"
    fi
}

# Remove the installed binaries from the bindir.
uninstall_bins() {
    local prefix="${1:-$DEFAULT_PREFIX}"
    local destdir="${DESTDIR:-}"

    require_root

    local bindir="$destdir$prefix/$DEFAULT_BINDIR"

    info "Removing binaries from $bindir..."
    for binary in "${SHEPHERD_BINARIES[@]}"; do
        remove_path "$bindir/$binary"
    done

    success "Removed binaries from $bindir"
}

# Remove the firewall helper and its polkit assets. Mirrors install_firewall.
uninstall_firewall() {
    local destdir="${DESTDIR:-}"

    require_root

    info "Removing firewall helper and polkit assets..."
    remove_path "$destdir$FIREWALL_HELPER_PATH"
    remove_path "$destdir$POLKIT_ACTIONS_DIR/$FIREWALL_POLICY_NAME"
    remove_path "$destdir$POLKIT_RULES_DIR/$FIREWALL_RULES_NAME"

    # Reload polkit on a real (non-packaging) uninstall so the dropped rule
    # stops applying. The shepherd-firewall system group is intentionally left
    # in place: it's harmless, and leftover files or other users may still
    # reference it. This mirrors the .deb postrm (scripts/lib/package.sh).
    if [[ -z "$destdir" ]]; then
        if systemctl is-active --quiet polkit 2>/dev/null; then
            info "Reloading polkit so the removed rule stops applying"
            systemctl reload polkit 2>/dev/null \
                || systemctl restart polkit 2>/dev/null \
                || warn "Could not reload polkit; restart it manually"
        fi
    fi

    success "Removed firewall helper"
}

# Remove the installed sway configuration. Mirrors install_sway_config.
uninstall_sway_config() {
    local destdir="${DESTDIR:-}"

    require_root

    local dst_dir="$destdir$SWAY_CONFIG_DIR"

    info "Removing sway configuration from $dst_dir..."
    remove_path "$dst_dir/$SHEPHERD_SWAY_CONFIG"

    # Remove the site-override drop-in dir only if it is empty, to preserve any
    # files a site placed there. Also try to remove the /etc/sway dir itself if
    # it's now empty (it may be shared with a system sway, so ignore failure).
    if rmdir "$dst_dir/$SHEPHERD_SWAY_CONFD" 2>/dev/null; then
        info "  Removed empty $dst_dir/$SHEPHERD_SWAY_CONFD"
    fi
    rmdir "$dst_dir" 2>/dev/null || true

    success "Removed sway configuration"
}

# Remove the display-manager desktop entry. Mirrors install_desktop_entry.
uninstall_desktop_entry() {
    local prefix="${1:-$DEFAULT_PREFIX}"
    local destdir="${DESTDIR:-}"

    require_root

    local dst_entry="$destdir$prefix/$DESKTOP_ENTRY_DIR/$DESKTOP_ENTRY_NAME"

    info "Removing desktop entry..."
    remove_path "$dst_entry"

    success "Removed desktop entry"
}

# Remove the bluetoothd drop-in. Mirrors install_bluetooth_dropin.
uninstall_bluetooth_dropin() {
    local destdir="${DESTDIR:-}"

    require_root

    local dropin_dst="$destdir$BLUETOOTH_DROPIN_DIR/$BLUETOOTH_DROPIN_NAME"

    info "Removing bluetoothd drop-in..."
    remove_path "$dropin_dst"
    # Leave the directory if anything else dropped files in it.
    rmdir "$destdir$BLUETOOTH_DROPIN_DIR" 2>/dev/null || true

    if [[ -z "$destdir" ]]; then
        systemctl daemon-reload 2>/dev/null \
            || warn "Could not reload systemd; bluetoothd keeps the old options until reboot"
        if systemctl is-active --quiet bluetooth.service; then
            info "Restarting bluetooth to drop the override"
            systemctl restart bluetooth.service 2>/dev/null \
                || warn "Could not restart bluetooth; restart it or reboot to apply"
        fi
    fi

    success "Removed bluetoothd drop-in"
}

# Remove the udev rule. Mirrors install_udev.
uninstall_udev() {
    local destdir="${DESTDIR:-}"

    require_root

    local rules_dst="$destdir$UDEV_RULES_DIR/$UINPUT_RULES_NAME"

    info "Removing udev rule..."
    remove_path "$rules_dst"

    # Reload rules on a real (non-packaging) uninstall so the dropped rule
    # stops applying. Mirrors the .deb postrm (scripts/lib/package.sh).
    if [[ -z "$destdir" ]]; then
        if command -v udevadm >/dev/null 2>&1; then
            info "Reloading udev rules"
            udevadm control --reload-rules 2>/dev/null \
                || warn "Could not reload udev rules; reboot to fully apply"
        fi
    fi

    success "Removed udev rule"
}

# Remove all system-wide components. The mirror of install_system: reverses
# every host-global file an `install all` / .deb places. Per-user config and
# group memberships are left untouched (see the note atop the uninstall block).
uninstall_system() {
    local prefix="${1:-$DEFAULT_PREFIX}"

    uninstall_bins "$prefix"
    uninstall_firewall
    uninstall_sway_config
    uninstall_desktop_entry "$prefix"
    uninstall_udev
    uninstall_bluetooth_dropin
}

# Remove everything shepherd installed system-wide.
uninstall_all() {
    local prefix="${1:-$DEFAULT_PREFIX}"

    require_root

    info "Uninstalling shepherd-launcher (prefix: $prefix)..."

    uninstall_system "$prefix"

    success "Uninstall complete!"

    if (( UNINSTALL_SKIPPED_OWNED > 0 )); then
        info ""
        warn "Left $UNINSTALL_SKIPPED_OWNED file(s) that an installed package owns."
        warn "Deleting those would leave dpkg unable to restore them later."
        warn "To remove the packaged install too:  sudo apt purge $DISTRO_PACKAGE_NAME"
    fi

    info ""
    info "Left in place (remove by hand if you want them gone):"
    info "  - Per-user config under ~/.config/shepherd/ (config.toml, movies.toml)"
    info "  - Group memberships (input, video, bluetooth, shepherd-firewall)"
    info "  - The shepherd-firewall system group"
}

# Main uninstall command dispatcher. Parallels install_main.
uninstall_main() {
    local subcmd="${1:-}"
    shift || true

    local prefix="$DEFAULT_PREFIX"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --prefix)
                prefix="$2"
                shift 2
                ;;
            *)
                die "Unknown option: $1"
                ;;
        esac
    done

    case "$subcmd" in
        bins)
            uninstall_bins "$prefix"
            ;;
        firewall)
            uninstall_firewall
            ;;
        sway-config)
            uninstall_sway_config
            ;;
        desktop-entry)
            uninstall_desktop_entry "$prefix"
            ;;
        udev)
            uninstall_udev
            ;;
        all)
            uninstall_all "$prefix"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd uninstall <command> [OPTIONS]

Removes files that 'shepherd install' placed system-wide. Per-user config
(~/.config/shepherd) and group memberships are left untouched.

Commands:
    bins              Remove the installed binaries
    firewall          Remove the firewall helper + polkit assets
    sway-config       Remove the sway configuration
    desktop-entry     Remove the display-manager desktop entry
    udev              Remove the udev rule
    all               Remove everything installed system-wide

Options:
    --prefix PREFIX   Installation prefix the files were installed under
                      (default: $DEFAULT_PREFIX)

Environment:
    DESTDIR           Removal root, mirroring 'install' (default: empty).
                      When set, firewall/udev skip the polkit/udev reload.

Examples:
    shepherd uninstall bins
    shepherd uninstall bins --prefix /usr
    shepherd uninstall all
EOF
            ;;
        *)
            die "Unknown uninstall command: $subcmd (try: shepherd uninstall help)"
            ;;
    esac
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
