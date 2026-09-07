#!/usr/bin/env bash
# Bluetooth admin operations for shepherd-launcher.
#
# Currently one subcommand: `clear`, which force-disconnects and unpairs
# every BLE peer recorded in shepherd's admin record, then deletes the record
# and any factory-reset sentinel so the next kiosk session comes up unclaimed.
#
# Since issue #157 that record is the *device's*, not a user's: there is one
# Bluetooth adapter and one BlueZ bond table, so there is one admin record,
# shared by every kiosk user on the machine. `--user` still names whose session
# to check and whose home to fall back to, but on a device with the custodian
# what this clears belongs to all of them.

# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

# For STATED_STATE_ROOT and the protected file names (issue #157). Sourced
# rather than restated: this file used to carry its own copy of the custodian's
# directory with a comment saying it "must match install.sh's", which is the
# arrangement that has to be checked by hand and therefore is not.
# shellcheck source=install.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/install.sh"

# Where shepherdd's admin record and factory-reset sentinel live.
#
# Two possible homes since issue #157. On a device with the state custodian they
# are in its shared directory, at a uid the kiosk user does not have; before it,
# and on a stack running with `--no-state-custodian`, they are under the user's
# own. `bluetooth_clear` picks between them, and that choice is also what decides
# whether `--user` was needed -- see the comment there.
#
# `<data_dir>/<name>`, where data_dir defaults to ~/.local/share/shepherdd
# (`APP_DIR = "shepherdd"` in shepherd-util).
SHEPHERD_DEFAULT_DATA_REL=".local/share/shepherdd"

# Refuse to operate on a user who currently has an active login
# session. The whole point of this command is to clean up *after*
# logout — running it on a live session would race with shepherdd
# (which holds the admin record open and could rewrite it after the
# delete) and could yank a phone out from under a parent who's mid-
# action in the companion app.
user_has_active_session() {
    local user="$1"
    if command_exists loginctl; then
        # `loginctl list-sessions --no-legend` columns: SESSION UID USER SEAT TTY STATE IDLE SINCE
        loginctl list-sessions --no-legend 2>/dev/null \
            | awk -v u="$user" '$3 == u { found=1 } END { exit !found }'
    else
        # Fallback when loginctl is unavailable (it shouldn't be on a
        # systemd system, but be defensive).
        who 2>/dev/null | awk -v u="$user" '$1 == u { found=1 } END { exit !found }'
    fi
}

# Every kiosk user the custodian holds state for.
#
# The admin record is the device's, so any of them could have a shepherdd
# holding it open -- not just the one named on the command line. Empty on a
# device without a custodian, where the record really is per-user and the named
# one is the only session that matters.
_users_with_custodian_state() {
    [[ -d "$STATED_STATE_ROOT" ]] || return 0
    local dir
    for dir in "$STATED_STATE_ROOT"/*/; do
        [[ -d "$dir" ]] || continue
        basename "${dir%/}"
    done
}

# Read the `identity_address` field out of an admin TOML file.
# Tolerates whitespace and either single or double quotes.
admin_record_identity_address() {
    local path="$1"
    if [[ ! -r "$path" ]]; then
        return 1
    fi
    # admin.toml is small and our schema is fixed (`identity_address`
    # appears once under `[admin]`), so a grep+sed extraction is
    # safe; no need to drag a TOML parser into the script.
    grep -E '^[[:space:]]*identity_address[[:space:]]*=' "$path" \
        | head -n1 \
        | sed -E 's/.*=[[:space:]]*["'"'"']([^"'"'"']*)["'"'"'].*/\1/'
}

# Disconnect + unpair a single BLE peer via bluetoothctl. Both calls
# are best-effort: `disconnect` is a no-op if the peer isn't currently
# connected, `remove` is a no-op if no bond exists. We swallow stderr
# but echo our own status lines so the operator sees one consistent
# stream.
bluetooth_clear_peer() {
    local address="$1"
    if ! command_exists bluetoothctl; then
        die "bluetoothctl not found; install the 'bluez' package"
    fi

    info "Disconnecting $address (no-op if not currently connected)..."
    if bluetoothctl disconnect "$address" >/dev/null 2>&1; then
        success "  Disconnected $address"
    else
        info "  $address was not connected"
    fi

    info "Removing bond for $address..."
    if bluetoothctl remove "$address" >/dev/null 2>&1; then
        success "  Removed bond for $address"
    else
        info "  No bond present for $address"
    fi
}

# Main entrypoint for `shepherd bluetooth clear --user USER`.
#
# Workflow:
#   1. Validate user exists.
#   2. Require root (BlueZ ops + reading the target user's home).
#   3. Refuse if the user is logged in (unless --force).
#   4. Read the user's admin.toml; if missing, treat the bond cleanup
#      as already done — but still clear the sentinel and report.
#   5. Force-disconnect + unpair every recorded peer in BlueZ.
#   6. Delete the admin record and any factory-reset sentinel.
bluetooth_clear() {
    local user=""
    local admin_record=""
    local sentinel=""
    local force=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --user)
                user="$2"
                shift 2
                ;;
            --admin-record)
                admin_record="$2"
                shift 2
                ;;
            --sentinel)
                sentinel="$2"
                shift 2
                ;;
            --force)
                force=true
                shift
                ;;
            *)
                die "Unknown option: $1 (try: shepherd bluetooth help)"
                ;;
        esac
    done

    require_root
    [[ -z "$user" ]] || validate_user "$user"

    # Where the record lives decides whether `--user` was needed at all.
    #
    # With the custodian it is the device's, at a path with no user in it, so
    # naming one adds nothing: the session guard below checks every kiosk user
    # regardless, and the home directory is not consulted. Without one the
    # record really is per-user and there is nothing else to go on, so it is
    # required -- and the error says which of the two this device is.
    if [[ -z "$admin_record" || -z "$sentinel" ]]; then
        if [[ -d "$STATED_ADMIN_DIR" ]]; then
            [[ -n "$admin_record" ]] \
                || admin_record="$STATED_ADMIN_DIR/$SHEPHERD_ADMIN_RECORD_FILE"
            [[ -n "$sentinel" ]] \
                || sentinel="$STATED_ADMIN_DIR/$SHEPHERD_RESET_SENTINEL_FILE"
        else
            [[ -n "$user" ]] || die "This device has no state custodian, so the admin record is one user's rather than the device's; pass --user USER"
            local home
            home="$(get_user_home "$user")"
            [[ -n "$home" ]] || die "Could not determine home directory for '$user'"
            [[ -n "$admin_record" ]] \
                || admin_record="$home/$SHEPHERD_DEFAULT_DATA_REL/$SHEPHERD_ADMIN_RECORD_FILE"
            [[ -n "$sentinel" ]] \
                || sentinel="$home/$SHEPHERD_DEFAULT_DATA_REL/$SHEPHERD_RESET_SENTINEL_FILE"
        fi
    fi

    # The record this deletes is the device's, so the race is with *any* kiosk
    # user's shepherdd, not only the named one. Checking just `--user` would let
    # a second child's live session have the record pulled out from under it --
    # the thing this guard exists to prevent, one user over.
    local busy=()
    local candidate
    for candidate in ${user:+"$user"} $(_users_with_custodian_state); do
        user_has_active_session "$candidate" || continue
        [[ " ${busy[*]-} " == *" $candidate "* ]] && continue
        busy+=("$candidate")
    done
    if [[ "${#busy[@]}" -gt 0 && "$force" != "true" ]]; then
        die "Active login session for: ${busy[*]}. shepherd's admin record is the device's, so clearing it races with any running shepherdd; log them out, or pass --force"
    fi

    info "Admin record: $admin_record"
    info "Sentinel:     $sentinel"

    # Collect peer addresses from the admin record (currently only one
    # admin peer is supported, but loop in case the schema grows).
    local addresses=()
    if [[ -f "$admin_record" ]]; then
        local addr
        addr="$(admin_record_identity_address "$admin_record")"
        if [[ -n "$addr" ]]; then
            addresses+=("$addr")
        else
            warn "Admin record $admin_record exists but has no identity_address; nothing to unpair in BlueZ"
        fi
    else
        info "No admin record at $admin_record (already unclaimed); skipping BlueZ unpair step"
    fi

    if [[ "${#addresses[@]}" -gt 0 ]]; then
        for addr in "${addresses[@]}"; do
            bluetooth_clear_peer "$addr"
        done
    fi

    if [[ -f "$admin_record" ]]; then
        rm -f -- "$admin_record"
        success "Deleted admin record $admin_record"
    fi

    if [[ -e "$sentinel" ]]; then
        rm -f -- "$sentinel"
        success "Removed leftover reset sentinel $sentinel"
    fi

    if [[ "$admin_record" == "$STATED_ADMIN_DIR/"* ]]; then
        success "Bluetooth state cleared for this device"
        info "  The admin record is shared, so every kiosk user is now unclaimed."
    else
        success "Bluetooth state cleared for user '$user'"
    fi
}

# Subcommand dispatcher.
bluetooth_main() {
    local subcmd="${1:-}"
    shift || true

    case "$subcmd" in
        clear)
            bluetooth_clear "$@"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd bluetooth <command> [options]

Commands:
    clear     Force-disconnect + unpair the bonded BLE peer and delete
              shepherd's admin record, so the next session starts unclaimed.
              With the state custodian that record is the device's, so this
              unclaims it for every kiosk user on the machine.

Options for 'clear':
    --user USER             Whose record to clear. Only needed on a device
                            without the state custodian, where the admin record
                            is one user's; with the custodian it is the
                            device's and there is nothing to name.
    --admin-record PATH     Override admin.toml location (default:
                            $STATED_ADMIN_DIR/$SHEPHERD_ADMIN_RECORD_FILE on a
                            device with the state custodian, else
                            ~USER/$SHEPHERD_DEFAULT_DATA_REL/$SHEPHERD_ADMIN_RECORD_FILE).
    --sentinel PATH         Override reset-sentinel location (default:
                            $STATED_ADMIN_DIR/$SHEPHERD_RESET_SENTINEL_FILE on a
                            device with the state custodian, else
                            ~USER/$SHEPHERD_DEFAULT_DATA_REL/$SHEPHERD_RESET_SENTINEL_FILE).
    --force                 Proceed even if the user is currently logged in.

Examples:
    sudo shepherd bluetooth clear                             # with a custodian
    sudo shepherd bluetooth clear --user kiosk                # without one
    sudo shepherd bluetooth clear --force                     # ignore live sessions
EOF
            ;;
        *)
            die "Unknown bluetooth command: $subcmd (try: shepherd bluetooth help)"
            ;;
    esac
}
