#!/usr/bin/env bash
# Installation logic for lunchbox-launcher
# Handles binary installation, config deployment, and desktop entry setup

# Get the directory containing this script
INSTALL_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source common utilities
# shellcheck source=common.sh
source "$INSTALL_LIB_DIR/common.sh"

# Source build utilities for binary paths
# shellcheck source=build.sh
source "$INSTALL_LIB_DIR/build.sh"

# Source config utilities: `install policy` validates before it installs, and
# `get_validate_binary` is where the validator's path is decided.
# shellcheck source=config.sh
source "$INSTALL_LIB_DIR/config.sh"

# Source the shepherd -> lunchbox migration, which `install all` runs before
# it installs anything (see the call site for why the order matters).
# shellcheck source=migrate.sh
source "$INSTALL_LIB_DIR/migrate.sh"

# Distro package name. Lives here rather than in package.sh because the
# uninstall path needs it to point at `apt purge`, and package.sh sources
# this file (not the other way round).
DISTRO_PACKAGE_NAME="lunchbox-launcher"

# Where a packaged install keeps the data files that have no repo to come from
# (the example config, the media library, VERSION, and the bluetoothd drop-in
# template). `scripts/lunchbox-admin` points `LUNCHBOX_DATA_DIR` here, and
# `package.sh` stages into it; it lives here because both of those are
# downstream of this file.
PACKAGED_DATA_DIR="/usr/share/lunchbox"

# Default installation paths
DEFAULT_PREFIX="/usr/local"
DEFAULT_BINDIR="bin"

# Standard sway config location
SWAY_CONFIG_DIR="/etc/sway"
LUNCHBOX_SWAY_CONFIG="lunchbox.conf"
LUNCHBOX_SWAY_CONFD="lunchbox.conf.d"

# Desktop entry location
DESKTOP_ENTRY_DIR="share/wayland-sessions"
DESKTOP_ENTRY_NAME="lunchbox.desktop"

# Firewall helper paths (hardcoded -- the polkit .policy file references
# the absolute path to the helper binary, and polkit's own dirs are fixed
# system locations regardless of $prefix).
#
# Both the action and the rule go to polkit's *vendor* directories under
# /usr/share, because both are lunchbox's own files rather than site policy
# (issue #177). The rule grants lunchbox's own action to lunchbox's own group;
# nothing about it is a local decision. An admin who wants to override it still
# can, the way polkit intends: a same-named file in /etc/polkit-1/rules.d,
# which is read first and wins.
FIREWALL_HELPER_PATH="/usr/libexec/lunchbox-firewall-helper"
POLKIT_ACTIONS_DIR="/usr/share/polkit-1/actions"
POLKIT_RULES_DIR="/usr/share/polkit-1/rules.d"
# Where the rule used to go, before #177 moved it. Installs and uninstalls
# clear a copy left there by an older from-source install; the .deb's
# maintainer scripts do the same with `dpkg-maintscript-helper rm_conffile`.
POLKIT_LEGACY_RULES_DIR="/etc/polkit-1/rules.d"
FIREWALL_POLICY_NAME="com.lunchbox-os.firewall.policy"
FIREWALL_RULES_NAME="50-lunchbox-firewall.rules"
FIREWALL_GROUP="lunchbox-firewall"

# State custodian (issue #157). Like the firewall helper, the binary lives at a
# fixed path regardless of --prefix, because a systemd unit references it
# absolutely and units are not relocatable. It is not a command an operator
# runs, so /usr/libexec is where it belongs anyway.
STATED_PATH="/usr/libexec/lunchbox-stated"
STATED_USER="lunchbox-state"
STATED_UNIT_DIR="/etc/systemd/system"
STATED_SOCKET_UNIT="lunchbox-stated@.socket"
STATED_SERVICE_UNIT="lunchbox-stated@.service"
# Where the protected files live. `/var/lib/lunchboxd` is already the tree's
# system state root (harden.sh keeps its rollback state there), and both
# crates/lunchboxd/README.md and docs/INSTALL.md have always described state as
# living under it.
STATED_STATE_ROOT="/var/lib/lunchboxd/state"
# The session watchdog's authority (issue #172). The custodian is outside the
# kiosk session at its own uid, which is what lets it notice a killed lunchboxd
# -- and what means logind treats it as a stranger to the session it has to end.
# This rule is the difference between a watchdog that fires and one that only
# looks like it will.
SESSION_GUARD_RULES_NAME="50-lunchbox-session-guard.rules"

# The file names lunchbox's protected files have, and what happens to each when
# a device gains or loses the custodian.
#
# These mirror `ProtectedFile` in `lunchbox-util`, which is the Rust half of the
# same list, plus `lunchboxd.db`, which the custodian owns without it being a
# `ProtectedFile` (it is reached through `Store`, not `ProtectedFiles`).
# `crates/lunchbox-util/tests/installer_covers_protected_files.rs` fails if a
# name is added there and not accounted for here -- the two lists cannot be one
# list, so they are held together by something that breaks loudly instead of by
# a comment asking nicely.
#
# Moved from `~/.local/share/lunchboxd/` into this user's custodian directory,
# and back again by `uninstall state --restore-to-home`.
LUNCHBOX_MIGRATED_FILES=(lunchboxd.db)
# The device's files rather than a user's, so they move to the *shared*
# directory instead. There is one Bluetooth adapter and one BlueZ bond table,
# and forgetting a bond forgets it for the machine -- so an admin record kept
# per-user while the bond was system-wide gave a two-child device behaviour
# nobody chose. The same argument covers the other two: there is one management
# API on one port, so a per-user web password would be two passwords for one
# door, and one certificate for the host that serves it.
# `ProtectedFile::scope` is the Rust half of this split.
LUNCHBOX_SYSTEM_FILES=(admin.toml unbond-queue.toml web-auth.toml tls.pem)
# Where they go. Shared by every kiosk user, at the same uid and mode as the
# per-user directories, so it is no more reachable from an activity.
STATED_ADMIN_DIR="/var/lib/lunchboxd/admin"
# The policy, moved from `~/.config/lunchbox/` -- a different directory, so it
# is handled apart from the list above rather than being in it.
LUNCHBOX_POLICY_FILE="config.toml"
# Deliberately *not* moved: a one-shot instruction rather than state, so moving
# a stale one would factory-reset a device during an upgrade. Declared rather
# than merely omitted, so the drift test can tell "decided against" apart from
# "forgotten" -- which is the whole distinction it exists to check. (It is a
# device file like the ones above, and lunchboxd reads it from the shared
# directory; it is simply never carried across.)
# shellcheck disable=SC2034  # read by installer_covers_protected_files.rs
LUNCHBOX_UNMIGRATED_FILES=(.factory-reset-ble)
# Named individually where a caller needs one by name. shellcheck reads each
# file alone, so it cannot see the libraries below using these.
# shellcheck disable=SC2034  # used by bluetooth.sh, which sources this file
LUNCHBOX_ADMIN_RECORD_FILE="admin.toml"
# shellcheck disable=SC2034  # used by bluetooth.sh, which sources this file
LUNCHBOX_RESET_SENTINEL_FILE=".factory-reset-ble"
# shellcheck disable=SC2034  # used by webauth.sh, which sources this file
LUNCHBOX_WEB_AUTH_FILE="web-auth.toml"

# The socket unit instance that serves `$1`.
#
# A function rather than a format string repeated at each call site: the
# template names above are the *unit files*, and this is the instance, which is
# what `systemctl enable` and `systemctl stop` actually take.
stated_socket_unit_for() {
    echo "lunchbox-stated@$1.socket"
}

# udev rules. Installed to a fixed system location regardless of --prefix
# (udev only reads /usr/lib/udev/rules.d and /etc/udev/rules.d). Currently
# just the /dev/uinput access rule the input-compat sidecars need.
#
# The vendor directory, for the same reason as the polkit rule above (issue
# #177): this is lunchbox's rule, not the site's. /etc/udev/rules.d is read
# afterwards and a same-named file there still overrides it.
UDEV_RULES_DIR="/usr/lib/udev/rules.d"
# Where the rule used to go, before #177 moved it. See POLKIT_LEGACY_RULES_DIR.
UDEV_LEGACY_RULES_DIR="/etc/udev/rules.d"
UINPUT_RULES_NAME="71-lunchbox-uinput.rules"

# systemd drop-in that runs bluetoothd with experimental D-Bus interfaces,
# which is what exposes `Device1.PreferredBearer` — see install_bluetooth_dropin
# and dist/systemd/. A drop-in rather than an edit to /etc/bluetooth/main.conf
# because BlueZ has no conf.d: it reads exactly one file, so shipping config
# there means fighting the distro's conffile on every upgrade.
BLUETOOTH_DROPIN_DIR="/etc/systemd/system/bluetooth.service.d"
BLUETOOTH_DROPIN_NAME="10-lunchbox-bluetooth-experimental.conf"
# The drop-in names the *rendering* machine's bluetoothd, so a build host can
# never write a correct one -- which is why a package ships the template here
# instead of the rendered file and renders it in its postinst (issue #177).
BLUETOOTH_DROPIN_TEMPLATE_DIR="$PACKAGED_DATA_DIR/systemd"
# Used only when the target's own unit is readable but its ExecStart is not
# (a shape of bluetooth.service we have never seen). Rendering nothing would
# be worse than rendering the path every distro we support actually uses.
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
        die "Release binaries not found. Run 'lunchbox build --release' first."
    fi
    
    info "Installing binaries to $bindir..."
    
    ensure_dir "$bindir" 0755
    
    for binary in "${LUNCHBOX_BINARIES[@]}"; do
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
    local dst_config="$dst_dir/$LUNCHBOX_SWAY_CONFIG"
    local dst_confd="$dst_dir/$LUNCHBOX_SWAY_CONFD"

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
    # `--no-harden-sway-ipc` is stripped here (issue #144). lunchboxd hardens by
    # default: once it has connected it unlinks sway's IPC socket, and nothing
    # else can reach the compositor for the rest of the session. That matters
    # because sway's IPC hands any process running as lunchboxd's uid — which is
    # every activity — `exec`, which starts a process outside supervision *and*
    # outside the cgroup the per-entry firewall is attached to.
    #
    # `--trust-environment` is stripped for the same reason again: it lets the
    # environment name the binaries lunchboxd execs and redirect where the
    # browser policy is written, and on a device the kiosk user chooses the
    # environment (GDM's PAM stack reads `~/.pam_environment`). Only the e2e
    # suite passes it, but a hand-edited config could.
    #
    # `--no-state-custodian` is stripped for the same reason again: it keeps
    # lunchbox's policy and state in the kiosk user's home, where every
    # activity can read and rewrite them (issue #157).
    #
    # `--no-restrict-ipc-peers` is stripped for the same reason. It opens
    # lunchboxd's *own* management socket to every process at this uid, which is
    # every activity: without the check a game can call `logout`, `stop_current`
    # or `launch`. It is in `sway.conf` because a dev stack runs entirely inside
    # one shell's cgroup, where the check cannot mean anything.
    #
    # The two `swaymsg exit` fallbacks are rewritten rather than stripped (issue
    # #144's defect 2) -- the one that runs when lunchboxd exits, and the one
    # behind the `Mod4+Shift+Escape` escape hatch, which fires only when there is
    # no lunchboxd to signal. `swaymsg exit` cannot work on a device:
    # lunchboxd unlinks sway's IPC socket once it has connected, so a daemon
    # that dies after that leaves sway up with nothing supervising the session.
    # `loginctl terminate-session` needs no compositor socket, and terminating
    # one's own session needs no polkit authorisation. `sway.conf` keeps
    # `swaymsg exit` because a development sway is nested inside the developer's
    # own login session and inherits its `XDG_SESSION_ID`.
    #
    # This does not make a *deliberate* kill safe on its own: the `sh -c` wrapper
    # runs at the kiosk uid, so an activity can kill it first and leave nothing
    # to run the fallback. What closes that is the state custodian's session
    # watchdog (#172), which is outside the session at a uid nothing in it can
    # signal; this line stays as the fast path for the ordinary case. See the
    # note above the exec line in `sway.conf`.
    # `sway.conf` carries both flags because it is the development config, where
    # the unlink would take the socket away from `swaymsg` and the headless
    # harness. An installed kiosk wants the defaults, so they come back out
    # here — and the checks below are the ones that matter: a rename upstream
    # that silently left one in would ship an unhardened device.
    # shellcheck disable=SC2016  # $XDG_SESSION_ID must reach the config
    # literally, for the session's own shell to expand when the fallback runs.
    sed \
        -e "s|./target/debug/lunchbox-launcher|$bindir/lunchbox-launcher|g" \
        -e "s|./target/debug/lunchbox-hud|$bindir/lunchbox-hud|g" \
        -e "s|./target/debug/lunchboxd|$bindir/lunchboxd|g" \
        -e "s|./config.example.toml|~/.config/lunchbox/config.toml|g" \
        -e "s|-c ./sway.conf|-c $dst_config|g" \
        -e "s| --no-harden-sway-ipc||g" \
        -e "s| --no-restrict-ipc-peers||g" \
        -e "s| --trust-environment||g" \
        -e "s| --no-state-custodian||g" \
        -e '/^exec .*lunchboxd -c /s|swaymsg exit|loginctl terminate-session "$XDG_SESSION_ID"|' \
        -e '/^bindsym .*pkill -TERM lunchboxd/s|swaymsg exit|loginctl terminate-session "$XDG_SESSION_ID"|' \
        "$src_config" > "$dst_config"

    # Scoped to the exec line: the comment above it names the flag too, and a
    # whole-file grep would fail an install that had stripped it correctly.
    local dst_exec_line
    dst_exec_line="$(grep -E "^exec .*lunchboxd -c [^ ]+" "$dst_config" || true)"
    if [[ -z "$dst_exec_line" ]]; then
        die "No 'lunchboxd -c <path>' exec line in $dst_config (sway.conf's lunchboxd exec line may have changed)"
    fi
    if [[ "$dst_exec_line" == *--no-harden-sway-ipc* ]]; then
        die "Failed to strip --no-harden-sway-ipc from $dst_config; the installed device would leave sway's IPC socket reachable by every activity (issue #144)"
    fi
    if [[ "$dst_exec_line" == *--no-restrict-ipc-peers* ]]; then
        die "Failed to strip --no-restrict-ipc-peers from $dst_config; the installed device would let every activity drive lunchbox's own management socket (issue #144)"
    fi
    if [[ "$dst_exec_line" == *--no-state-custodian* ]]; then
        die "Failed to strip --no-state-custodian from $dst_config; the installed device would keep policy and state in the kiosk user's home, where every activity can rewrite them (issue #157)"
    fi
    if [[ "$dst_exec_line" == *"swaymsg exit"* ]]; then
        die "Failed to rewrite the 'swaymsg exit' fallback in $dst_config; lunchboxd unlinks sway's IPC socket, so a daemon that died would leave the session running with nothing supervising it (issue #144)"
    fi
    if [[ "$dst_exec_line" != *"terminate-session"* ]]; then
        die "No session-teardown fallback on the exec line in $dst_config; a daemon that died would leave the session running with nothing supervising it (issue #144)"
    fi
    local dst_exit_binding
    dst_exit_binding="$(grep -E "^bindsym .*pkill -TERM lunchboxd" "$dst_config" || true)"
    if [[ -z "$dst_exit_binding" ]]; then
        die "No 'pkill -TERM lunchboxd' exit binding in $dst_config (sway.conf's exit keybinding may have changed)"
    fi
    if [[ "$dst_exit_binding" == *"swaymsg exit"* ]]; then
        die "Failed to rewrite the 'swaymsg exit' fallback on the exit binding in $dst_config; lunchboxd unlinks sway's IPC socket, so the escape hatch would do nothing when there is no lunchboxd to signal (issue #144)"
    fi
    if [[ "$dst_exec_line" == *--trust-environment* ]]; then
        die "Failed to strip --trust-environment from $dst_config; the installed device would take helper binaries, and the browser-policy root, from an environment the kiosk user can write (issue #144)"
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
Name=Lunchbox Kiosk
Comment=Lunchbox game launcher kiosk mode
Exec=sway -c $SWAY_CONFIG_DIR/$LUNCHBOX_SWAY_CONFIG --unsupported-gpu
Type=Application
DesktopNames=lunchbox
EOF
    
    chmod 0644 "$dst_entry"
    
    success "Installed desktop entry"
}

# Deploy user configuration
install_config() {
    local user="${1:-}"
    local source_config="${2:-}"

    if [[ -z "$user" ]]; then
        die "Usage: lunchbox install config --user USER [--source CONFIG]"
    fi
    
    validate_user "$user"

    # Example configs live at the repo root in a source checkout and under
    # /usr/share/lunchbox on a packaged install; get_data_dir picks the right
    # one so `lunchbox-admin setup-user` works without a source tree.
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
    local user_config_dir="$user_home/.config/lunchbox"
    local dst_config="$user_config_dir/config.toml"
    
    info "Installing user config to $dst_config..."
    
    # Create config directory owned by user
    maybe_sudo mkdir -p "$user_config_dir"
    maybe_sudo chown "$user:$user" "$user_config_dir"
    maybe_sudo chmod 0755 "$user_config_dir"
    
    # Deploy the example only where there is nothing yet. There is deliberately
    # no way to make this overwrite: `install config` seeds a device, and
    # replacing a policy is `install policy`'s job, which writes the copy that
    # actually decides what a child may do and validates before it does.
    #
    # An overwrite here used to exist as `--force`, and under the custodian it
    # was the worst of both (issue #157): it replaced the home copy, which is
    # only the seed, and left the custodian's copy -- the one lunchboxd reads --
    # untouched. The operator saw "Overwrote user configuration" and the device
    # kept running the old policy.
    #
    # Where it lands depends on whether this device has a custodian. With one,
    # the example goes straight to it and the home path gets the signpost --
    # seeding the home copy and then syncing it would recreate exactly the two
    # files this issue stopped having. Without one (a dev box, a DESTDIR stage,
    # a device that opted out) the home path is the live policy and this is
    # unchanged.
    local custodian_dir="$STATED_STATE_ROOT/$user"
    if [[ -d "$custodian_dir" && -z "${DESTDIR:-}" ]]; then
        if [[ -e "$custodian_dir/config.toml" ]]; then
            warn "Policy already exists at $custodian_dir/config.toml, leaving it alone"
            info "  To change the policy this device runs:"
            info "    lunchbox install policy --user $user --source PATH"
        else
            install -m 0600 -o "$STATED_USER" -g "$STATED_USER" \
                "$source_config" "$custodian_dir/config.toml"
            success "Installed $user's policy to the state custodian"
        fi
        _write_policy_placeholder "$user"
    elif maybe_sudo test -f "$dst_config"; then
        warn "Config file already exists at $dst_config, leaving it alone"
        info "  To change the policy this device runs:"
        info "    lunchbox install policy --user $user [--source PATH]"
    else
        maybe_sudo cp "$source_config" "$dst_config"
        maybe_sudo chown "$user:$user" "$dst_config"
        maybe_sudo chmod 0644 "$dst_config"
        success "Installed user configuration for $user"
    fi

    # The example config references `~/.config/lunchbox/movies.toml` for the
    # lunchbox-media entries. If the user picked the default example config,
    # also drop the matching example library so the entries don't 404 on
    # first launch. The user is still expected to edit the URIs.
    local source_library="$repo_root/movies-library.example.toml"
    local dst_library="$user_config_dir/movies.toml"
    if [[ -f "$source_library" ]]; then
        if maybe_sudo test -f "$dst_library"; then
            info "Media library already exists at $dst_library, skipping"
        else
            maybe_sudo cp "$source_library" "$dst_library"
            maybe_sudo chown "$user:$user" "$dst_library"
            maybe_sudo chmod 0644 "$dst_library"
            success "Installed media library for $user (edit URIs before use)"
        fi
    fi
}

# Groups the kiosk user must belong to for lunchbox-launcher features.
#
# - input: required by lunchbox-touch-bridge, lunchbox-tablet-bridge, and
#   lunchbox-gamepad-bridge (used when an entry has `input_compat =
#   "touch_to_mouse"`, `"tablet_to_touch"`, or `gamepad_*`) so they can read
#   /dev/input/event* and write /dev/uinput.
#   The uinput write access also needs the udev rule installed by
#   `install_udev`; see that function and dist/udev/.
# - video: required by the brightness slider. `brightnessctl`'s udev rule
#   grants `video` write access to /sys/class/backlight/*/brightness; the
#   daemon runs as the desktop user, so without this membership every
#   brightness write fails with EACCES even though brightnessctl is
#   installed.
# - bluetooth: required by the BLE management transport (lunchbox-ble).
#   BlueZ's polkit rules grant the `bluetooth` group permission to call
#   org.bluez.Adapter1.SetPairable and AgentManager1.RegisterAgent over
#   the system bus; without membership the BLE startup fails on the
#   first adapter call and the daemon never advertises.
#
# Add new groups here as features need them; install_user_groups walks the
# array and skips memberships the user already has.
LUNCHBOX_REQUIRED_GROUPS=(
    "input"
    "video"
    "bluetooth"
)

# Add $user to each named group, skipping groups that don't exist on this
# system or that the user already belongs to. Shared by install_user_groups
# (from-source `install all`) and setup_user (lunchbox-admin, .deb path) so the
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
        # lunchbox-firewall, from install/packaging).
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

# Add the target user to all groups required by lunchbox-launcher.
# Idempotent: skips any group the user is already in.
install_user_groups() {
    local user="${1:-}"

    if [[ -z "$user" ]]; then
        die "Usage: lunchbox install groups --user USER"
    fi

    require_root
    validate_user "$user"

    add_user_to_groups "$user" "${LUNCHBOX_REQUIRED_GROUPS[@]}"
    success "Updated group memberships for $user"
}

# Resolve the path bluetoothd is actually started from, so the drop-in's
# ExecStart matches the distro rather than a guess. Getting this wrong is
# not a cosmetic error — a drop-in pointing at a non-existent binary stops
# Bluetooth working entirely — so we read it back from the unit that is
# installed, and only fall back to the well-known Ubuntu path when the unit is
# there but its ExecStart cannot be parsed. (A machine with no bluetooth.service
# at all never reaches here: install_bluetooth_dropin returns first.)
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
# lunchbox needs `Device1.PreferredBearer` to pin the admin phone to the
# BR/EDR bearer; without it BlueZ arms the kernel to auto-connect the phone
# over LE, the device ends up central, and the companion can never encrypt
# the link (see dist/systemd/ for the full story). lunchbox degrades
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

    # Packaging stages the *template*, not the drop-in (issue #177).
    #
    # ExecStart has to name the bluetoothd of the machine the file ends up on,
    # and under DESTDIR that machine is a build host. The old arrangement
    # shipped a drop-in carrying the build host's path and had the postinst
    # `sed` it -- which worked, but rewriting a file dpkg had just checksummed
    # made every subsequent upgrade see a locally-modified conffile and either
    # prompt or silently keep the stale copy. Handing the postinst a template
    # to render leaves nothing for dpkg to checksum and no prompt to answer.
    if [[ -n "$destdir" ]]; then
        local template_dst="$destdir$BLUETOOTH_DROPIN_TEMPLATE_DIR/$BLUETOOTH_DROPIN_NAME"
        info "Staging bluetoothd drop-in template to $template_dst..."
        ensure_dir "$(dirname "$template_dst")" 0755
        install -m 0644 -o root -g root "$template" "$template_dst"
        success "Staged bluetoothd drop-in template"
        return 0
    fi

    # No bluetooth unit? Nothing to extend.
    if ! systemctl cat bluetooth.service >/dev/null 2>&1; then
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

    # Only a real install gets here (the DESTDIR branch returned above), so the
    # reload is unconditional.
    #
    # NOTE: the .deb renders and reloads from its generated postinst instead
    # (scripts/lib/package.sh, _package_write_control). Keep them in sync.
    systemctl daemon-reload 2>/dev/null \
        || warn "Could not reload systemd; the drop-in applies at next boot"
    if systemctl is-active --quiet bluetooth.service; then
        info "Restarting bluetooth to apply (briefly drops Bluetooth connections)"
        systemctl restart bluetooth.service 2>/dev/null \
            || warn "Could not restart bluetooth; restart it or reboot to apply"
    fi

    success "Installed bluetoothd drop-in"
}

# Drop a copy of one of lunchbox's files left at a path an earlier release used.
#
# Issue #177 moved the udev and polkit rules out of the admin directories into
# the vendor ones. Both subsystems read both locations, so a copy left behind is
# not merely untidy -- it is a second, older rule that still applies, and the
# one in /etc is the one that wins. Only a from-source install can have put one
# there; the .deb clears its own with `dpkg-maintscript-helper rm_conffile`
# (see scripts/lib/package.sh), and a file the package manager still owns is
# left for it rather than deleted behind its back (path_owned_by_dpkg, defined
# with the uninstall functions below, explains why that matters).
#
# Callers must only reach this on a real install: under DESTDIR the argument
# names a path on the build host, not in the staging tree.
#
# Args:
#   $1 -- the superseded absolute path
#   $2 -- where the file lives now, for the log line
remove_superseded_copy() {
    local path="$1" now_at="$2"

    [[ -e "$path" ]] || return 0
    if path_owned_by_dpkg "$path"; then
        warn "  Leaving the superseded $path (owned by an installed package)"
        return 0
    fi
    info "  Removing the superseded $path (this file now lives at $now_at)"
    rm -f "$path"
}

# Install the udev rules lunchbox-launcher needs.
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
        remove_superseded_copy \
            "$UDEV_LEGACY_RULES_DIR/$UINPUT_RULES_NAME" "$rules_dst"
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
#   $1 -- target user to add to the lunchbox-firewall group (optional;
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
    helper_src="$(get_target_dir "$release")/lunchbox-firewall-helper"
    local helper_dst="$destdir$FIREWALL_HELPER_PATH"
    local policy_src="$repo_root/dist/polkit/$FIREWALL_POLICY_NAME"
    local policy_dst="$destdir$POLKIT_ACTIONS_DIR/$FIREWALL_POLICY_NAME"
    local rules_src="$repo_root/dist/polkit/$FIREWALL_RULES_NAME"
    local rules_dst="$destdir$POLKIT_RULES_DIR/$FIREWALL_RULES_NAME"

    if [[ ! -x "$helper_src" ]]; then
        if [[ "$release" == "true" ]]; then
            die "lunchbox-firewall-helper not found at $helper_src; run 'lunchbox build --release' first"
        else
            die "lunchbox-firewall-helper not found at $helper_src; run 'cargo build --bin lunchbox-firewall-helper' first"
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
        remove_superseded_copy \
            "$POLKIT_LEGACY_RULES_DIR/$FIREWALL_RULES_NAME" "$rules_dst"
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

# Give one user a custodian: the directory, their migrated state, and the socket.
#
# Split out of `install_state` because a packaged device cannot reach that.
# The `.deb` ships `lunchbox-admin`, which has no `install` verb -- so on a
# packaged system this is the only route to the per-user half, and
# `lunchbox-admin setup-user` is what calls it. Without that, `setup-user`
# enabled the socket and nothing else: the custodian came up with an empty
# directory while the device's real history sat in the home directory, and the
# diagnostic that noticed named `lunchbox install state`, a command that does
# not exist there.
#
# Args:
#   $1 -- the kiosk user
setup_state_for_user() {
    local user="$1"

    require_root
    id "$user" >/dev/null 2>&1 || die "No such user: $user"
    getent passwd "$STATED_USER" >/dev/null \
        || die "The system user $STATED_USER does not exist; install the state custodian first"

    # Create the state directory here rather than leaving it to the unit's
    # `StateDirectory=`. That would also do it -- but only when the *service*
    # first starts, and socket activation means that is the moment lunchboxd
    # first connects. Migration has to have already happened by then, or the
    # daemon creates an empty database and the device's history is stranded in
    # the home directory it came from.
    #
    # `StateDirectory=` is idempotent, so it accepts what is already here and
    # keeps enforcing the mode.
    info "Creating $STATED_STATE_ROOT/$user"
    install -d -m 0700 -o "$STATED_USER" -g "$STATED_USER" "$STATED_STATE_ROOT/$user"

    _migrate_state_for_user "$user"

    info "Enabling the state custodian's socket for $user"
    local socket_unit
    socket_unit="$(stated_socket_unit_for "$user")"
    systemctl enable --now "$socket_unit" 2>/dev/null \
        || warn "Could not enable $socket_unit; enable it manually"
}

# Install the state custodian: the binary, its systemd units, and the system
# user that owns lunchbox's policy and state (issue #157).
#
# Why this exists at all: lunchboxd runs as the same uid as every activity it
# launches, so `lunchboxd.db`, `config.toml` and the BLE admin record are
# writable by the software the device is meant to be supervising. That was
# measured, not inferred — an activity resetting today's usage and adding an
# unlimited entry to the policy, live, on an installed device
# (docs/ai/history/2026-08-29 005). No file mode can help at a shared uid, so
# the files move to a uid the activities do not have and lunchboxd reaches them
# over a socket that admits only its own session's cgroup.
#
# Args:
#   $1 -- the kiosk user whose session is trusted (required outside DESTDIR)
#   $2 -- "true" for release binaries (default), "false" for debug
install_state() {
    local user="${1:-}"
    local release="${2:-true}"
    local destdir="${DESTDIR:-}"
    local repo_root
    repo_root="$(get_repo_root)"

    require_root

    local bin_src
    bin_src="$(get_target_dir "$release")/lunchbox-stated"
    if [[ ! -x "$bin_src" ]]; then
        if [[ "$release" == "true" ]]; then
            die "lunchbox-stated not found at $bin_src; run 'lunchbox build --release' first"
        else
            die "lunchbox-stated not found at $bin_src; run 'cargo build --bin lunchbox-stated' first"
        fi
    fi
    for unit in "$STATED_SOCKET_UNIT" "$STATED_SERVICE_UNIT"; do
        [[ -f "$repo_root/dist/systemd/$unit" ]] \
            || die "systemd unit missing at $repo_root/dist/systemd/$unit"
    done
    local guard_src="$repo_root/dist/polkit/$SESSION_GUARD_RULES_NAME"
    [[ -f "$guard_src" ]] \
        || die "Session watchdog polkit rule missing at $guard_src"

    info "Installing state custodian to $destdir$STATED_PATH..."
    ensure_dir "$(dirname "$destdir$STATED_PATH")" 0755
    install -m 0755 -o root -g root "$bin_src" "$destdir$STATED_PATH"

    for unit in "$STATED_SOCKET_UNIT" "$STATED_SERVICE_UNIT"; do
        info "Installing $unit to $destdir$STATED_UNIT_DIR/..."
        ensure_dir "$destdir$STATED_UNIT_DIR" 0755
        install -m 0644 -o root -g root "$repo_root/dist/systemd/$unit" \
            "$destdir$STATED_UNIT_DIR/$unit"
    done

    # Without this the watchdog is inert: it notices, logs, calls
    # TerminateSession and is refused (issue #172). Installed with the custodian
    # rather than beside the firewall's rule because it is the custodian's
    # authority, and a device that has one without the other is the shape this
    # is trying to avoid.
    local guard_dst="$destdir$POLKIT_RULES_DIR/$SESSION_GUARD_RULES_NAME"
    info "Installing session watchdog polkit rule to $guard_dst..."
    ensure_dir "$(dirname "$guard_dst")" 0755
    install -m 0644 -o root -g root "$guard_src" "$guard_dst"

    # Host mutation only on a real install; under DESTDIR these would change the
    # build host and bake its state into the package.
    #
    # NOTE: the .deb runs the user-create + daemon-reload from its generated
    # postinst instead (scripts/lib/package.sh, _package_write_control), and
    # leaves the per-user enable to the admin. Keep them in sync.
    if [[ -z "$destdir" ]]; then
        remove_superseded_copy \
            "$POLKIT_LEGACY_RULES_DIR/$SESSION_GUARD_RULES_NAME" "$guard_dst"
        if ! getent passwd "$STATED_USER" >/dev/null; then
            info "Creating system user: $STATED_USER"
            # No home and no shell: this uid exists to own files and answer one
            # socket. Anything it could log in with is surface it does not need.
            useradd --system --no-create-home --home-dir /nonexistent \
                --shell /usr/sbin/nologin "$STATED_USER"
        fi

        systemctl daemon-reload 2>/dev/null \
            || warn "Could not reload systemd; the state custodian applies at next boot"

        if systemctl is-active --quiet polkit 2>/dev/null; then
            info "Reloading polkit so the session watchdog may end a session"
            systemctl reload polkit 2>/dev/null \
                || systemctl restart polkit 2>/dev/null \
                || warn "Could not reload polkit; restart it manually or the session watchdog \
will be refused when it fires"
        fi

        [[ -n "$user" ]] && setup_state_for_user "$user"
    fi

    success "State custodian installed"
}

# The line that marks a policy file as lunchbox's signpost rather than a policy.
#
# Matched, not just written: `install policy` must never push one of these to
# the custodian, and the migration must not "move" one it wrote itself on an
# earlier run.
POLICY_PLACEHOLDER_MARK="# lunchbox: this device's policy lives with the state custodian"

# Whether $1 is the placeholder rather than a real policy.
_is_policy_placeholder() {
    [[ -f "$1" ]] && grep -qF "$POLICY_PLACEHOLDER_MARK" "$1"
}

# Leave a signpost where the policy used to be.
#
# The policy now lives at a uid activities do not have, and the path every doc
# and every habit points at is empty. A missing file would be read as "not
# configured yet"; this says where it went and what to do instead.
#
# It parses, and grants nothing. Nothing should ever read it as a policy: a
# device with a signpost has a custodian, and a lunchboxd that cannot reach its
# custodian now refuses to start rather than running on whatever is in the home
# directory. Keeping the file valid means that if some path ever does read it,
# what it grants is nothing -- rather than the daemon dying on a parse error
# somewhere the message would be less clear than the one it exits with.
_write_policy_placeholder() {
    local user="$1"
    local home dst
    home="$(getent passwd "$user" | cut -d: -f6)"
    [[ -n "$home" ]] || return 0
    dst="$home/.config/lunchbox/config.toml"

    # Never over a real policy: on a device without a custodian that file is
    # the live one, and this function is called from paths that also run there.
    if [[ -e "$dst" ]] && ! _is_policy_placeholder "$dst"; then
        return 0
    fi

    install -d -m 0755 -o "$user" -g "$user" "$home/.config/lunchbox"
    cat > "$dst" <<EOF
$POLICY_PLACEHOLDER_MARK
#
# It was moved to a uid no activity has, so that the software this device
# supervises cannot rewrite the rules it is supervised by (issue #157):
#
#     $STATED_STATE_ROOT/$user/config.toml
#
# To change what this device allows, edit that file as root --
#
#     sudoedit $STATED_STATE_ROOT/$user/config.toml
#
# -- or install one from anywhere, validated before it is applied:
#
#     sudo lunchbox install policy --user $user --source ./new-config.toml
#
# Either way lunchboxd reloads within a second; no restart is needed.
#
# Editing *this* file changes nothing. If the custodian ever cannot be reached,
# lunchboxd refuses to start rather than falling back to this one -- the session
# ends at the login screen, which says something is wrong, where a device with
# an empty launcher would look like an ordinary evening with nothing available.

config_version = 1
EOF
    chown "$user:$user" "$dst"
    chmod 0644 "$dst"
    info "  Left a signpost at $dst"
}

# Move an existing device's state into the custodian's directory.
#
# Without this an upgrade looks like a factory reset: lunchboxd would ask the
# custodian for a database that has never been written, and a child's usage
# history, quota balances and BLE admin record would still be sitting in their
# home directory, unread. Silently starting from zero is the worst available
# outcome, so this runs as part of the install rather than being left to a note
# in a changelog.
#
# Root's job, necessarily: `lunchbox-state` cannot read the user's home (0750),
# so it could not migrate its own state even if it wanted to.
#
# Idempotent and non-destructive. A file already in the protected directory is
# never overwritten -- if both exist, the protected one is the live one and the
# home copy is stale, so it is left alone and reported rather than merged.
#
# The policy moves with everything else, and a placeholder takes its place at
# the familiar path saying where it went. Leaving a real copy there was the
# earlier design and it was worse: two files that look equally authoritative,
# only one of which decides anything, and a diagnostic whose whole job was to
# notice they had drifted apart. One file that decides, and one signpost, needs
# no diagnostic.
_migrate_state_for_user() {
    local user="$1"
    local home state_dir moved=0 skipped=0
    home="$(getent passwd "$user" | cut -d: -f6)"
    [[ -n "$home" && -d "$home" ]] || return 0
    state_dir="$STATED_STATE_ROOT/$user"

    # Created by the caller just above; if it is not there, something went
    # wrong earlier and moving files into nowhere would lose them.
    [[ -d "$state_dir" ]] || return 0

    # `.factory-reset-ble` is deliberately not migrated: it is a one-shot
    # instruction, not state, and moving a stale one would factory-reset a
    # device during an upgrade.
    local src dst name
    for name in "${LUNCHBOX_MIGRATED_FILES[@]}"; do
        src="$home/.local/share/lunchboxd/$name"
        dst="$state_dir/$name"
        [[ -f "$src" ]] || continue
        if [[ -e "$dst" ]]; then
            warn "  $dst already exists; leaving $src in place (the protected copy is the live one)"
            skipped=$((skipped + 1))
            continue
        fi
        info "  Moving $name into the custodian's directory"
        install -m 0600 -o "$STATED_USER" -g "$STATED_USER" "$src" "$dst"
        # SQLite may have side files; move them with the database or the next
        # open sees a journal that does not match.
        local side
        for side in "-wal" "-shm" "-journal"; do
            [[ -f "$src$side" ]] || continue
            install -m 0600 -o "$STATED_USER" -g "$STATED_USER" "$src$side" "$dst$side"
            rm -f "$src$side"
        done
        rm -f "$src"
        moved=$((moved + 1))
    done

    # The device's files, into the shared directory rather than this user's.
    #
    # First user wins on a machine where two of them were separately claimed
    # before the custodian existed: there is one bond table, so there can only
    # be one admin record, and picking the later one would silently discard a
    # pairing that still works. The one left behind is reported, not deleted.
    install -d -m 0700 -o "$STATED_USER" -g "$STATED_USER" "$STATED_ADMIN_DIR"
    for name in "${LUNCHBOX_SYSTEM_FILES[@]}"; do
        src="$home/.local/share/lunchboxd/$name"
        dst="$STATED_ADMIN_DIR/$name"
        [[ -f "$src" ]] || continue
        if [[ -e "$dst" ]]; then
            warn "  $dst already exists; leaving $user's $name in place"
            info "    This device already has an admin record. A second one cannot apply:"
            info "    the BlueZ bond it names is the machine's, not $user's."
            skipped=$((skipped + 1))
            continue
        fi
        info "  Moving $name into the device's shared directory"
        install -m 0600 -o "$STATED_USER" -g "$STATED_USER" "$src" "$dst"
        rm -f "$src"
        moved=$((moved + 1))
    done

    # The policy, moved like the rest, with a signpost left behind.
    src="$home/.config/lunchbox/$LUNCHBOX_POLICY_FILE"
    dst="$state_dir/$LUNCHBOX_POLICY_FILE"
    if [[ -f "$src" ]] && ! _is_policy_placeholder "$src"; then
        if [[ -e "$dst" ]]; then
            warn "  $dst already exists; not replacing it from $src"
            skipped=$((skipped + 1))
        else
            info "  Moving the policy into the custodian's directory"
            install -m 0600 -o "$STATED_USER" -g "$STATED_USER" "$src" "$dst"
            rm -f "$src"
            moved=$((moved + 1))
        fi
    fi
    # Whether or not there was one to move: a device that reaches here keeps
    # its policy with the custodian, and the familiar path should say so rather
    # than be missing. Also covers a re-run, where the move already happened.
    if [[ -d "$state_dir" ]]; then
        _write_policy_placeholder "$user"
    fi

    if [[ "$moved" -gt 0 ]]; then
        success "Migrated $moved state file(s) for $user into $state_dir"
    fi
    if [[ "$skipped" -gt 0 ]]; then
        info "  $skipped file(s) were left in $home/.local/share/lunchboxd (already migrated)"
    fi
}

# Where to find the policy validator, installed or built.
#
# A device has it on `PATH` -- it ships in the package -- and a source tree has
# it under `target/`. Checked in that order, because on a device the installed
# one is the one that matches the daemon, and a stale `target/` from an old
# checkout would be the wrong answer to validate against.
#
# Deliberately never *builds* it, unlike `lunchbox config validate`: this runs
# as root, and a cargo build as root leaves a root-owned `target/` behind that
# the developer's next plain build cannot write.
_resolve_validator() {
    local release="${1:-true}"
    local installed
    if installed="$(command -v lunchbox-validate-config 2>/dev/null)" && [[ -x "$installed" ]]; then
        echo "$installed"
        return 0
    fi
    local built
    built="$(get_validate_binary "$release")"
    if [[ -x "$built" ]]; then
        echo "$built"
        return 0
    fi
    return 1
}

# Push a policy to the custodian, and say so.
#
# `install config` deploys the *example* config; this is the other direction --
# take a policy and make it the one the daemon reads. That is the remedy
# lunchboxd's divergence warning names, so it has to exist as a command an
# operator can actually run.
#
# With `--source` the policy comes straight from a path the administrator
# names, and the kiosk user's home is never touched. That is the case hardening
# creates: `harden apply` gives the kiosk user `nologin` and denies it SSH, so
# an administrator cannot `su` in to edit the config the way every doc used to
# assume. Editing it as root through `~kiosk/` still works and remains the
# default when no `--source` is given, but routing an edit through the home
# directory of the uid this issue exists to distrust should not be the only way
# to reconfigure a device.
#
# Args:
#   $1 -- the kiosk user whose custodian receives the policy (required)
#   $2 -- policy to push (default: that user's ~/.config/lunchbox/config.toml)
#   $3 -- "true" for the release validator (default), "false" for debug
install_policy() {
    local user="${1:-}"
    local source_config="${2:-}"
    local release="${3:-true}"
    require_root
    [[ -n "$user" ]] || die "Usage: lunchbox install policy --user USER [--source PATH]"
    validate_user "$user"

    local home src
    home="$(getent passwd "$user" | cut -d: -f6)"
    if [[ -n "$source_config" ]]; then
        src="$source_config"
        [[ -f "$src" ]] || die "No policy at $src to push"
    else
        src="$home/.config/lunchbox/config.toml"
        [[ -f "$src" ]] \
            || die "No policy at $src to push (name one with --source PATH)"
    fi

    # The signpost is not a policy. Pushing one would replace a device's real
    # policy with an empty one -- every activity gone -- which is a bad enough
    # outcome to be worth a check rather than a comment.
    if _is_policy_placeholder "$src"; then
        die "$src is the signpost lunchbox leaves when the policy moves to the custodian, not a policy.
  The live one is at $STATED_STATE_ROOT/$user/config.toml -- edit it with
    sudoedit $STATED_STATE_ROOT/$user/config.toml
  or install a different one with --source PATH."
    fi

    [[ -d "$STATED_STATE_ROOT/$user" ]] \
        || die "No state custodian for $user; run 'lunchbox install state --user $user' first"

    # Validate before installing, not after. lunchboxd tolerates a bad policy on
    # *reload* -- it keeps the running one and logs -- but at startup
    # `load_policy` is fatal, and since #172 a lunchboxd that exits takes the
    # session down with `loginctl terminate-session`. So a policy with a typo
    # costs nothing until the next boot, and then costs the whole session, on a
    # device whose kiosk user has no shell to fix it from. The validator already
    # exists and the file is right here; there is no reason to find out later.
    local validator
    validator="$(_resolve_validator "$release")" \
        || die "lunchbox-validate-config not found; run 'lunchbox build' first (from a source tree), \
or reinstall the package, which ships it"
    info "Validating $src..."
    "$validator" "$src" \
        || die "$src did not validate; nothing was pushed (the device keeps its current policy)"

    install -m 0600 -o "$STATED_USER" -g "$STATED_USER" \
        "$src" "$STATED_STATE_ROOT/$user/config.toml"
    success "Pushed $src to $user's state custodian"
    info "lunchboxd reloads it within a second; no restart needed."
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
#   $2 -- user to add to the lunchbox-firewall group (empty under DESTDIR)
install_system() {
    local prefix="${1:-$DEFAULT_PREFIX}"
    local firewall_user="${2:-}"

    install_bins "$prefix"
    install_firewall "$firewall_user" "true"
    install_state "$firewall_user" "true"
    install_sway_config "$prefix"
    install_desktop_entry "$prefix"
    install_udev
    install_bluetooth_dropin
}

# Install everything
install_all() {
    local user="${1:-}"
    local prefix="${2:-$DEFAULT_PREFIX}"

    if [[ -z "$user" ]]; then
        die "Usage: lunchbox install all --user USER [--prefix PREFIX]"
    fi

    require_root
    validate_user "$user"

    info "Installing lunchbox-launcher (prefix: $prefix)..."

    # Before anything is installed, not after. The migration moves a legacy
    # device's state to the lunchbox paths, and it refuses to move onto a path
    # that already exists -- so if `install_state` ran first and created an
    # empty /var/lib/lunchboxd, the real state under /var/lib/shepherdd would
    # be stranded there and the device would come up as if it were new.
    # No-op on a device that was never a shepherd.
    migrate_from_shepherd "$user" "$prefix"

    install_system "$prefix" "$user"
    install_config "$user" ""
    install_user_groups "$user"

    success "Installation complete!"
    info ""
    info "Next steps:"
    # Step 1 named ~/.config/lunchbox/config.toml until issue #157 moved the
    # policy to the custodian and left a signpost there. An operator following
    # the old wording would edit a file that decides nothing.
    info "  1. Set the policy. It lives with the state custodian now --"
    info "     $STATED_STATE_ROOT/$user/config.toml -- and"
    info "     ~$user/.config/lunchbox/config.toml is a signpost saying so."
    info "     Edit it in place:"
    info "       sudoedit $STATED_STATE_ROOT/$user/config.toml"
    info "     or install one from anywhere, validated before it is applied:"
    info "       sudo lunchbox install policy --user $user --source PATH"
    info "     Either way lunchboxd reloads within a second."
    info "  2. Have $user log out and back in (so the new lunchbox-firewall"
    info "     group membership takes effect for per-entry firewall rules)"
    info "  3. Select 'Lunchbox Kiosk' session at login"
    # Not "optionally" any more (issue #157): two of lunchbox's own protections
    # rest on hardening, so a device a child uses is not finished without it.
    info "  4. Run 'lunchbox harden apply --user $user'. On a device a child"
    info "     uses this is not optional: it is what stops a second login for"
    info "     $user, which the state custodian refuses to choose between, and"
    info "     what stops PAM reading an environment $user wrote."
}

# --- Uninstall -------------------------------------------------------------
#
# The uninstall_* functions below reverse the matching install_* steps. They
# only remove host-global, lunchbox-owned files; they deliberately leave user
# data alone (per-user config under ~/.config/lunchbox) and group memberships
# (input/video/bluetooth/lunchbox-firewall) in place, since removing those can
# affect a user's other software and isn't reversible from a backup. Each
# function is a no-op for files that are already gone, so uninstall is safe to
# re-run and safe to run after a partial install.

# Count of files this uninstall left alone because dpkg owns them, so
# uninstall_all can say so once at the end rather than only per-file.
UNINSTALL_SKIPPED_OWNED=0

# True when dpkg tracks $1 as belonging to an installed package.
#
# A source uninstall must never delete a file the package manager owns. On a
# packaged box that is most of what these functions would remove -- the
# binaries, the sway config, the polkit and udev rules -- and dpkg's file list
# goes stale the moment one of them disappears behind its back: the box reports
# itself installed while missing its sway config, and the session bounces
# straight back to the greeter.
#
# The package declares no conffiles (issue #177), so everything it ships is an
# ordinary file and `apt install --reinstall lunchbox-launcher` puts the lot
# back. That was not true while the four files below were conffiles: dpkg read
# a missing conffile as a deliberate admin removal and needed --force-confmiss
# to restore it. See docs/INSTALL.md.
#
# An upgraded box still answers "yes" here for the bluetoothd drop-in, which
# dpkg remembers as an *obsolete* conffile from before #177 even though the
# postinst now writes it. Leaving it to dpkg is the conservative answer there
# too: `apt remove` runs a postrm that deletes it.
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
    for binary in "${LUNCHBOX_BINARIES[@]}"; do
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
    # An install from before #177 put the rule in polkit's admin directory.
    remove_path "$destdir$POLKIT_LEGACY_RULES_DIR/$FIREWALL_RULES_NAME"

    # Reload polkit on a real (non-packaging) uninstall so the dropped rule
    # stops applying. The lunchbox-firewall system group is intentionally left
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
    remove_path "$dst_dir/$LUNCHBOX_SWAY_CONFIG"

    # Remove the site-override drop-in dir only if it is empty, to preserve any
    # files a site placed there. Also try to remove the /etc/sway dir itself if
    # it's now empty (it may be shared with a system sway, so ignore failure).
    if rmdir "$dst_dir/$LUNCHBOX_SWAY_CONFD" 2>/dev/null; then
        info "  Removed empty $dst_dir/$LUNCHBOX_SWAY_CONFD"
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
# Move a device's state back out of the custodian's directory (issue #157).
#
# The way out, and the reason `uninstall state` is not a one-way door.
# `_migrate_state_for_user` *moved* the database and the admin record in, so a
# device that goes back to keeping state in the kiosk user's home -- a rollback
# to a build older than this one, or a deliberate `--no-state-custodian` --
# finds that home empty and starts from zero: no usage history, no quota
# balances, and an absent `admin.toml`, which `ClaimMachine::load` reads as
# `Unclaimed`. A downgrade would look like a factory reset, and the next phone
# to pair would claim the device. This is what stops that.
#
# Non-destructive in the same direction as the forward migration: a file
# already in the home directory is never overwritten. On this path that copy is
# the one lunchboxd would read next, so it is the one that wins.
#
# Args:
#   $1 -- the user whose state is being restored
_restore_state_to_home() {
    local user="$1"
    local home state_dir data_dir moved=0 skipped=0
    home="$(getent passwd "$user" | cut -d: -f6)"
    if [[ -z "$home" || ! -d "$home" ]]; then
        warn "  $user has no home directory; leaving their state in $STATED_STATE_ROOT/$user"
        return 0
    fi
    state_dir="$STATED_STATE_ROOT/$user"
    [[ -d "$state_dir" ]] || return 0
    data_dir="$home/.local/share/lunchboxd"

    info "Restoring $user's state to $data_dir..."
    # Each component explicitly: `install -d` applies its mode and ownership to
    # the *last* path element only, so a missing `~/.local/share` would be
    # created root-owned and the user could then not write inside it.
    local dir
    for dir in "$home/.local" "$home/.local/share" "$data_dir"; do
        [[ -d "$dir" ]] || install -d -m 0755 -o "$user" -g "$user" "$dir"
    done

    local src dst name side
    for name in "${LUNCHBOX_MIGRATED_FILES[@]}"; do
        src="$state_dir/$name"
        dst="$data_dir/$name"
        [[ -f "$src" ]] || continue
        if [[ -e "$dst" ]]; then
            warn "  $dst already exists; leaving $src where it is"
            skipped=$((skipped + 1))
            continue
        fi
        info "  Moving $name back to $data_dir"
        install -m 0600 -o "$user" -g "$user" "$src" "$dst"
        # SQLite side files travel with the database, exactly as they do on the
        # way in: a journal left behind does not match the database it belongs
        # to, and the next open is what finds out.
        for side in "-wal" "-shm" "-journal"; do
            [[ -f "$src$side" ]] || continue
            install -m 0600 -o "$user" -g "$user" "$src$side" "$dst$side"
            rm -f "$src$side"
        done
        rm -f "$src"
        moved=$((moved + 1))
    done

    # The policy moves back over the signpost, which is the one file here that
    # is not worth preserving: it exists to say the policy went somewhere else,
    # and it is about to be wrong. A real policy at that path is a different
    # matter and is left alone like everything else.
    src="$state_dir/$LUNCHBOX_POLICY_FILE"
    dst="$home/.config/lunchbox/$LUNCHBOX_POLICY_FILE"
    if [[ -f "$src" ]]; then
        if [[ -e "$dst" ]] && ! _is_policy_placeholder "$dst"; then
            warn "  $dst is a real policy; leaving $src where it is"
            skipped=$((skipped + 1))
        else
            info "  Moving the policy back to $dst"
            install -d -m 0755 -o "$user" -g "$user" "$home/.config/lunchbox"
            install -m 0644 -o "$user" -g "$user" "$src" "$dst"
            rm -f "$src"
            moved=$((moved + 1))
        fi
    fi

    # The device's files are *copied* to each user, not moved, because before
    # the custodian every user had their own -- that is the arrangement being
    # restored. `_drop_shared_admin_files` removes the shared originals once
    # every user has taken a copy.
    local shared
    for name in "${LUNCHBOX_SYSTEM_FILES[@]}"; do
        shared="$STATED_ADMIN_DIR/$name"
        dst="$data_dir/$name"
        [[ -f "$shared" ]] || continue
        if [[ -e "$dst" ]]; then
            warn "  $dst already exists; not replacing it from $shared"
            skipped=$((skipped + 1))
            continue
        fi
        info "  Copying the device's $name to $data_dir"
        install -m 0600 -o "$user" -g "$user" "$shared" "$dst"
        moved=$((moved + 1))
    done

    if [[ "$moved" -gt 0 ]]; then
        success "  Restored $moved file(s) to $data_dir"
    fi
    if [[ "$skipped" -gt 0 ]]; then
        info "  $skipped file(s) needed a decision and were left for you (above)"
    fi
}

# Remove the device's shared files, once every user has a copy.
#
# Separate from `_restore_state_to_home` because it must happen exactly once,
# after the last user -- copying to two users and deleting after the first would
# leave the second without an admin record.
_drop_shared_admin_files() {
    [[ -d "$STATED_ADMIN_DIR" ]] || return 0
    local name
    for name in "${LUNCHBOX_SYSTEM_FILES[@]}"; do
        rm -f "$STATED_ADMIN_DIR/$name"
    done
    # The sentinel is an instruction, not state; a stale one left here would
    # factory-reset the device the next time a custodian is installed.
    rm -f "$STATED_ADMIN_DIR/${LUNCHBOX_UNMIGRATED_FILES[0]}"
    rmdir "$STATED_ADMIN_DIR" 2>/dev/null || true
}

# Stop and disable every running instance of the custodian.
#
# Before removing its unit files, or systemd keeps a socket bound to a unit that
# no longer exists. Before any restore too: the custodian holds the database
# open for its whole life, and moving a file from under a live writer is how a
# database gets a journal that no longer matches it.
_stop_stated_instances() {
    local unit
    while IFS= read -r unit; do
        [[ -n "$unit" ]] || continue
        info "Stopping $unit"
        systemctl disable --now "$unit" 2>/dev/null || true
    done < <(systemctl list-units --all --no-legend 'lunchbox-stated@*' 2>/dev/null \
        | awk '{print $1}' | grep -E '^lunchbox-stated@' || true)
}

# Put every user's state back in their home directory.
#
# Every user the custodian holds state for, not one named on the command line:
# this runs when a device is leaving the custodian behind, and a device with two
# kiosk users would otherwise have half its state moved and half left.
_restore_state_for_every_user() {
    [[ -d "$STATED_STATE_ROOT" ]] || return 0
    local state_user_dir
    for state_user_dir in "$STATED_STATE_ROOT"/*/; do
        [[ -d "$state_user_dir" ]] || continue
        state_user_dir="${state_user_dir%/}"
        _restore_state_to_home "$(basename "$state_user_dir")"
    done
    # Only now that every user has their copy.
    _drop_shared_admin_files
}

# Stop the custodian and return every user's state to their home, leaving the
# binary and units alone.
#
# The half of `uninstall state --restore-to-home` a packaged device needs. There
# the units and the binary belong to dpkg -- deleting them behind its back is
# what `uninstall` already refuses to do to a packaged file -- and `apt` is what
# removes them. What `apt` cannot do is move a device's state back out of the
# custodian first, and a downgrade that skips that starts from an empty database
# and an unclaimed device.
restore_state_to_home() {
    require_root

    _stop_stated_instances
    _restore_state_for_every_user

    success "Lunchbox's state is back in the users' home directories"
    info "  The custodian's binary and units are untouched; they belong to the"
    info "  package manager. Downgrade or remove the package to finish."
}

# Remove the state custodian.
#
# Args:
#   $1 -- "true" to move every user's state back to their home directory first
#         (default "false": the state is left where it is)
uninstall_state() {
    local restore="${1:-false}"
    local destdir="${DESTDIR:-}"

    require_root

    # Stop and disable every instance first, or systemd keeps a socket bound to
    # a unit file that no longer exists. This is also what has to happen before
    # any restore: the custodian holds the database open for its whole life, and
    # moving a file out from under a live writer is how a database gets a
    # journal that no longer matches it.
    if [[ -z "$destdir" ]]; then
        _stop_stated_instances
        [[ "$restore" == "true" ]] && _restore_state_for_every_user
    fi

    info "Removing the state custodian..."
    remove_path "$destdir$STATED_PATH"
    remove_path "$destdir$STATED_UNIT_DIR/$STATED_SOCKET_UNIT"
    remove_path "$destdir$STATED_UNIT_DIR/$STATED_SERVICE_UNIT"
    # The authority goes with the daemon that held it (issue #172). Leaving it
    # behind would be leaving a uid the right to end sessions after the only
    # thing that had a reason to do so is gone.
    remove_path "$destdir$POLKIT_RULES_DIR/$SESSION_GUARD_RULES_NAME"
    # An install from before #177 put the rule in polkit's admin directory.
    remove_path "$destdir$POLKIT_LEGACY_RULES_DIR/$SESSION_GUARD_RULES_NAME"

    if [[ -z "$destdir" ]]; then
        systemctl daemon-reload 2>/dev/null || true
        if systemctl is-active --quiet polkit 2>/dev/null; then
            systemctl reload polkit 2>/dev/null \
                || systemctl restart polkit 2>/dev/null \
                || warn "Could not reload polkit; restart it manually"
        fi
    fi

    # The state itself is deliberately left behind, and so is the system user
    # that owns it. A device's usage history, quota balances and BLE admin
    # record are the things an uninstall is least entitled to destroy, and a
    # reinstall picks them straight back up. Removing the uid would orphan them
    # to a number rather than a name, which is worse than leaving both.
    #
    # Both directories, named separately: the per-user one holds the policy and
    # the database, the shared one the admin record and the unbond queue. A
    # message naming only the first would tell an operator they still had a BLE
    # admin record and then point them at the directory it is not in.
    local left=()
    [[ -d "$destdir$STATED_STATE_ROOT" ]] && left+=("$destdir$STATED_STATE_ROOT")
    [[ -d "$destdir$STATED_ADMIN_DIR" ]] && left+=("$destdir$STATED_ADMIN_DIR")
    if [[ "${#left[@]}" -gt 0 ]]; then
        if [[ "$restore" == "true" ]]; then
            info "Left in place, owned by $STATED_USER: ${left[*]}"
            info "  Anything still in them is named above; the rest went back to the"
            info "  users' home directories. Remove them by hand once you are happy."
        else
            info "Left lunchbox's state, owned by $STATED_USER:"
            local dir
            for dir in "${left[@]}"; do
                case "$dir" in
                    *"$STATED_ADMIN_DIR") info "    $dir  (BLE admin record, unbond queue)" ;;
                    *) info "    $dir  (policy and usage database, per user)" ;;
                esac
            done
            info "  Remove them by hand if you mean to discard a child's usage history"
            info "  and this device's pairing."
            info "  To put it back where lunchboxd looks without the custodian, re-run with"
            info "  --restore-to-home (a build older than issue #157 will not find it here)."
        fi
    fi

    success "Removed the state custodian"
}

uninstall_bluetooth_dropin() {
    local destdir="${DESTDIR:-}"

    require_root

    local dropin_dst="$destdir$BLUETOOTH_DROPIN_DIR/$BLUETOOTH_DROPIN_NAME"

    info "Removing bluetoothd drop-in..."
    remove_path "$dropin_dst"
    # Leave the directory if anything else dropped files in it.
    rmdir "$destdir$BLUETOOTH_DROPIN_DIR" 2>/dev/null || true
    # The template a packaged install carries so its postinst can render the
    # drop-in (issue #177). Only staging puts one there, so on a real box this
    # is either absent or dpkg's, and remove_path leaves dpkg's alone.
    remove_path "$destdir$BLUETOOTH_DROPIN_TEMPLATE_DIR/$BLUETOOTH_DROPIN_NAME"
    rmdir "$destdir$BLUETOOTH_DROPIN_TEMPLATE_DIR" 2>/dev/null || true

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
    # An install from before #177 put the rule in udev's admin directory.
    remove_path "$destdir$UDEV_LEGACY_RULES_DIR/$UINPUT_RULES_NAME"

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
    local restore="${2:-false}"

    uninstall_bins "$prefix"
    uninstall_firewall
    uninstall_state "$restore"
    uninstall_sway_config
    uninstall_desktop_entry "$prefix"
    uninstall_udev
    uninstall_bluetooth_dropin
}

# Remove everything lunchbox installed system-wide.
uninstall_all() {
    local prefix="${1:-$DEFAULT_PREFIX}"
    local restore="${2:-false}"

    require_root

    info "Uninstalling lunchbox-launcher (prefix: $prefix)..."

    uninstall_system "$prefix" "$restore"

    success "Uninstall complete!"

    if (( UNINSTALL_SKIPPED_OWNED > 0 )); then
        info ""
        warn "Left $UNINSTALL_SKIPPED_OWNED file(s) that an installed package owns."
        warn "Deleting those would leave dpkg unable to restore them later."
        warn "To remove the packaged install too:  sudo apt purge $DISTRO_PACKAGE_NAME"
    fi

    info ""
    info "Left in place (remove by hand if you want them gone):"
    info "  - Per-user config under ~/.config/lunchbox/ (config.toml, movies.toml)"
    info "  - Group memberships (input, video, bluetooth, lunchbox-firewall)"
    info "  - The lunchbox-firewall system group"
}

# Main uninstall command dispatcher. Parallels install_main.
uninstall_main() {
    local subcmd="${1:-}"
    shift || true

    local prefix="$DEFAULT_PREFIX"
    local restore="false"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --prefix)
                prefix="$2"
                shift 2
                ;;
            --restore-to-home)
                restore="true"
                shift
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
        state)
            uninstall_state "$restore"
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
            uninstall_all "$prefix" "$restore"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: lunchbox uninstall <command> [OPTIONS]

Removes files that 'lunchbox install' placed system-wide. Per-user config
(~/.config/lunchbox) and group memberships are left untouched.

Commands:
    bins              Remove the installed binaries
    firewall          Remove the firewall helper + polkit assets
    state             Remove the state custodian's binary and units (the state
                      itself, and the system user that owns it, are kept)
    sway-config       Remove the sway configuration
    desktop-entry     Remove the display-manager desktop entry
    udev              Remove the udev rule
    all               Remove everything installed system-wide

Options:
    --prefix PREFIX   Installation prefix the files were installed under
                      (default: $DEFAULT_PREFIX)
    --restore-to-home For 'state' and 'all': move each user's database and BLE
                      admin record back to ~/.local/share/lunchboxd first, where
                      a build without the state custodian looks for them. The
                      migration that put them under the custodian moved them, so
                      without this a downgrade starts from an empty database and
                      an unclaimed device.

Environment:
    DESTDIR           Removal root, mirroring 'install' (default: empty).
                      When set, firewall/udev skip the polkit/udev reload.

Examples:
    lunchbox uninstall bins
    lunchbox uninstall bins --prefix /usr
    lunchbox uninstall all
    lunchbox uninstall state --restore-to-home
EOF
            ;;
        *)
            die "Unknown uninstall command: $subcmd (try: lunchbox uninstall help)"
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
        state)
            install_state "$user" "$release"
            ;;
        policy)
            install_policy "$user" "$source_config" "$release"
            ;;
        config)
            install_config "$user" "$source_config"
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
            install_all "$user" "$prefix"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: lunchbox install <command> [OPTIONS]

Commands:
    bins              Install release binaries
    firewall          Install the privileged firewall helper + polkit rule
    state             Install the state custodian: its binary, systemd units
                      and system user (issue #157)
    policy            Push a policy to the custodian, making it the one
                      lunchboxd reads. Takes the user's edited
                      ~/.config/lunchbox/config.toml, or any file named with
                      --source. Validated before it is installed.
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
    --source CONFIG   Source config file. For 'config', what to deploy
                      (default: config.example.toml); for 'policy', the policy
                      to push (default: the user's own config.toml)
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
    lunchbox install bins --prefix /usr/local
    lunchbox install firewall --user kiosk
    lunchbox install config --user kiosk
    lunchbox install policy --user kiosk
    lunchbox install policy --user kiosk --source ./new-config.toml
    lunchbox install groups --user kiosk
    lunchbox install udev
    lunchbox install all --user kiosk --prefix /usr
EOF
            ;;
        *)
            die "Unknown install command: $subcmd (try: lunchbox install help)"
            ;;
    esac
}
