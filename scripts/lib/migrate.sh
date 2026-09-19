#!/usr/bin/env bash
# Migrate a device installed as "shepherd" to the "lunchbox" names.
#
# The rename (Shepherd -> Lunchbox) changed every installed path, unit and
# group name. A device that was installed before it has its config, its state
# and the custodian's database under the old names, and its units still
# running under the old names. Nothing in the new tree looks there: the rename
# was a clean break, not a fallback. This is what closes that gap.
#
# It runs from `lunchbox install all` and from the .deb's postinst, so an
# operator upgrading in place does not have to know the rename happened.
#
# Three rules shape it:
#
#   * Idempotent. It reports nothing and changes nothing on a device that was
#     never a shepherd, and running it twice is the same as running it once.
#   * Never clobber. A legacy path is moved only when the new path does not
#     already exist. If both exist the new one wins and the legacy one is left
#     on disk for the operator to look at, because only they can know which
#     copy is the real one.
#   * Stop before moving. The custodian units are stopped and disabled before
#     their state moves, so nothing is writing to a directory being renamed.
#
# Deliberately NOT here: /etc/apt/sources.list.d/shepherd.list and the Chrome
# policy under /etc/opt/chrome. Those are written by the operator following
# docs/INSTALL.md, not by `install`, so removing them is not ours to do --
# `migrate_report_manual_leftovers` names them instead.

MIGRATE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$MIGRATE_LIB_DIR/common.sh"

# Legacy spellings, paired with the constant that replaced each one. Kept as
# literals rather than derived from the new names: a future rename of a
# lunchbox path must not silently re-point what this migration looks for.
LEGACY_STATE_ROOT="/var/lib/shepherdd"
LEGACY_PACKAGED_DATA_DIR="/usr/share/shepherd"
LEGACY_FIREWALL_GROUP="shepherd-firewall"
LEGACY_STATED_USER="shepherd-state"

MIGRATE_DID_ANYTHING=0

# Note that a step ran, so the caller can tell "migrated" from "nothing to do"
# and only print the closing advice when something actually moved.
_migrated() {
    MIGRATE_DID_ANYTHING=1
    info "  $*"
}

# Move $1 to $2 unless $2 already exists.
#
# The "both exist" case is the one worth being careful about: it means the
# device has been installed under both names, and picking for the operator
# risks throwing away the copy they care about.
_migrate_path() {
    local from="$1" to="$2" what="$3"

    [[ -e "$from" ]] || return 0

    if [[ -e "$to" ]]; then
        warn "  Not moving $what: $to already exists."
        warn "    The legacy copy is still at $from -- compare them and"
        warn "    remove the one you do not want."
        return 0
    fi

    ensure_dir "$(dirname "$to")" 0755
    mv "$from" "$to"
    _migrated "$what: $from -> $to"
}

# Stop and disable every legacy custodian unit, then drop the unit files.
#
# Done first: the units hold the state directory this migration is about to
# move, and a running shepherd-stated would recreate paths underneath it.
_migrate_stated_units() {
    local unit had=0

    while IFS= read -r unit; do
        [[ -n "$unit" ]] || continue
        systemctl disable --now "$unit" >/dev/null 2>&1 || true
        _migrated "Stopped legacy unit $unit"
        had=1
    done < <(systemctl list-units --all --no-legend 'shepherd-stated@*' 2>/dev/null \
        | awk '{print $1}' | grep -E '^shepherd-stated@' || true)

    local f
    for f in "$STATED_UNIT_DIR"/shepherd-stated@.socket \
             "$STATED_UNIT_DIR"/shepherd-stated@.service; do
        [[ -e "$f" ]] || continue
        rm -f "$f"
        _migrated "Removed legacy unit file $f"
        had=1
    done

    if [[ "$had" -eq 1 ]]; then
        systemctl daemon-reload 2>/dev/null || true
    fi
    return 0
}

# The custodian's own tree: per-user state, the admin directory, hardening
# records, and the database inside each user's directory.
_migrate_state_tree() {
    _migrate_path "$LEGACY_STATE_ROOT" "/var/lib/lunchboxd" "state custodian tree"

    # The database is named after the daemon, so it changes inside whichever
    # tree survived the move above.
    local user_dir
    for user_dir in /var/lib/lunchboxd/state/*/; do
        [[ -d "$user_dir" ]] || continue
        _migrate_path "$user_dir/shepherdd.db" "$user_dir/lunchboxd.db" \
            "state database for $(basename "$user_dir")"
    done
}

# Host-global files installed by the old `install system`.
#
# Each is removed rather than moved: `install` has already written the
# lunchbox-named replacement by the time this runs, so the legacy file is a
# stale duplicate, and leaving a second polkit rule or udev rule in place
# would keep granting the old action name.
_migrate_system_files() {
    local f
    for f in \
        "$POLKIT_ACTIONS_DIR/org.shepherd.firewall.policy" \
        "$POLKIT_RULES_DIR/50-shepherd-firewall.rules" \
        "$POLKIT_RULES_DIR/50-shepherd-session-guard.rules" \
        "$POLKIT_LEGACY_RULES_DIR/50-shepherd-firewall.rules" \
        "$POLKIT_LEGACY_RULES_DIR/50-shepherd-session-guard.rules" \
        "$UDEV_RULES_DIR/71-shepherd-uinput.rules" \
        "$UDEV_LEGACY_RULES_DIR/71-shepherd-uinput.rules" \
        "$BLUETOOTH_DROPIN_DIR/10-shepherd-bluetooth-experimental.conf" \
        "/usr/libexec/shepherd-firewall-helper" \
        "/usr/libexec/shepherd-stated"
    do
        [[ -e "$f" ]] || continue
        if path_owned_by_dpkg "$f"; then
            # Only reachable on a from-source install over a packaged one. The
            # `lunchbox` package declares Conflicts/Replaces on
            # `shepherd-launcher`, so an apt upgrade has already removed these.
            warn "  Leaving $f (owned by an installed package)."
            warn "    Remove the old package with: sudo apt purge shepherd-launcher"
            continue
        fi
        rm -f "$f"
        _migrated "Removed legacy $f"
    done

    _migrate_path "$SWAY_CONFIG_DIR/shepherd.conf" \
        "$SWAY_CONFIG_DIR/$LUNCHBOX_SWAY_CONFIG" "sway config"
    _migrate_path "$SWAY_CONFIG_DIR/shepherd.conf.d" \
        "$SWAY_CONFIG_DIR/$LUNCHBOX_SWAY_CONFD" "sway config drop-ins"
    _migrate_path "$LEGACY_PACKAGED_DATA_DIR" "$PACKAGED_DATA_DIR" "packaged data"
}

# Old binaries left behind by a source install.
#
# A packaged install is left alone and reported: those files belong to the
# shepherd-launcher package, and `apt purge` is the right way to remove them.
_migrate_old_binaries() {
    local prefix="$1" f
    for f in shepherdd shepherd-admin shepherd-lock shepherd-launcher \
             shepherd-hud shepherd-pairing-display shepherd-media \
             shepherd-validate-config shepherd-touch-bridge \
             shepherd-tablet-bridge shepherd-gamepad-bridge
    do
        local p="$prefix/$DEFAULT_BINDIR/$f"
        [[ -e "$p" ]] || continue
        if path_owned_by_dpkg "$p"; then
            warn "  Leaving $p (owned by an installed package)."
            continue
        fi
        rm -f "$p"
        _migrated "Removed legacy binary $p"
    done

    _migrate_path "$prefix/$DESKTOP_ENTRY_DIR/shepherd.desktop" \
        "$prefix/$DESKTOP_ENTRY_DIR/$DESKTOP_ENTRY_NAME" "session entry"
}

# Per-user directories, for the user being installed for.
#
# XDG dirs are named after the product, the two daemon-owned ones after the
# daemon, which is why the targets are not spelled the same way.
migrate_user_dirs() {
    local user="$1" home
    home="$(getent passwd "$user" | cut -d: -f6)"
    [[ -n "$home" && -d "$home" ]] || return 0

    _migrate_path "$home/.config/shepherd"      "$home/.config/lunchbox"      "config for $user"
    _migrate_path "$home/.cache/shepherd"       "$home/.cache/lunchbox"       "cache for $user"
    _migrate_path "$home/.local/share/shepherdd" "$home/.local/share/lunchboxd" "data for $user"
    _migrate_path "$home/.local/state/shepherdd" "$home/.local/state/lunchboxd" "state for $user"
}

# Rename the firewall group and the custodian's system user in place, so that
# existing memberships and file ownership survive.
#
# groupmod/usermod keep the gid/uid, which is what makes this safe: every file
# already owned by the old group stays owned by the renamed one, so no chown
# sweep over the state tree is needed.
_migrate_accounts() {
    if getent group "$LEGACY_FIREWALL_GROUP" >/dev/null 2>&1; then
        if getent group "$FIREWALL_GROUP" >/dev/null 2>&1; then
            warn "  Both $LEGACY_FIREWALL_GROUP and $FIREWALL_GROUP exist;"
            warn "    leaving them alone. Move members over and delete the old one."
        elif groupmod -n "$FIREWALL_GROUP" "$LEGACY_FIREWALL_GROUP" 2>/dev/null; then
            _migrated "Renamed group $LEGACY_FIREWALL_GROUP -> $FIREWALL_GROUP"
        else
            warn "  Could not rename group $LEGACY_FIREWALL_GROUP"
        fi
    fi

    if getent passwd "$LEGACY_STATED_USER" >/dev/null 2>&1 \
       && ! getent passwd "$STATED_USER" >/dev/null 2>&1; then
        if usermod -l "$STATED_USER" "$LEGACY_STATED_USER" 2>/dev/null; then
            _migrated "Renamed user $LEGACY_STATED_USER -> $STATED_USER"
        else
            warn "  Could not rename user $LEGACY_STATED_USER"
        fi
        if getent group "$LEGACY_STATED_USER" >/dev/null 2>&1 \
           && ! getent group "$STATED_USER" >/dev/null 2>&1; then
            groupmod -n "$STATED_USER" "$LEGACY_STATED_USER" 2>/dev/null || true
        fi
    fi
}

# Files this migration will not touch, named so the operator can.
migrate_report_manual_leftovers() {
    local -a leftovers=()
    local f
    for f in /etc/apt/sources.list.d/shepherd.list \
             /etc/opt/chrome/policies/managed/shepherd.json \
             /opt/shepherd-deps \
             /opt/shepherd
    do
        [[ -e "$f" ]] && leftovers+=("$f")
    done

    [[ ${#leftovers[@]} -gt 0 ]] || return 0

    info ""
    info "Left in place (yours, not the installer's) -- remove when ready:"
    for f in "${leftovers[@]}"; do
        info "    $f"
    done
    info "  The apt source in particular still points at the shepherd package;"
    info "  see docs/INSTALL.md for the lunchbox one."
}

# Entry point. Safe on a device that was never a shepherd.
migrate_from_shepherd() {
    local user="${1:-}" prefix="${2:-$DEFAULT_PREFIX}"

    # Packaging builds a tree for a .deb; there is no live device to migrate.
    [[ -z "${DESTDIR:-}" ]] || return 0

    require_root

    MIGRATE_DID_ANYTHING=0

    _migrate_stated_units
    _migrate_state_tree
    _migrate_system_files
    _migrate_old_binaries "$prefix"
    _migrate_accounts
    [[ -n "$user" ]] && migrate_user_dirs "$user"

    if [[ "$MIGRATE_DID_ANYTHING" -eq 1 ]]; then
        info ""
        success "Migrated this device from the shepherd names to lunchbox."
        migrate_report_manual_leftovers
        info ""
    fi
    return 0
}
