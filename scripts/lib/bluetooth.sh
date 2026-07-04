#!/usr/bin/env bash
# Bluetooth admin operations for shepherd-launcher.
#
# Currently one subcommand: `clear`, which force-disconnects and unpairs
# every BLE peer recorded in a user's shepherdd admin record, then
# deletes the admin record + factory-reset sentinel so the user's next
# kiosk session comes up in an unclaimed state.

# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

# Default location of shepherdd's admin record under a user's home.
# Must match shepherd_management::DefaultAdminRecordPath, i.e. the
# default `<data_dir>/admin.toml` where data_dir defaults to
# ~/.local/share/shepherdd (`APP_DIR = "shepherdd"` in shepherd-util).
SHEPHERD_DEFAULT_ADMIN_REL=".local/share/shepherdd/admin.toml"

# Same idea for the factory-reset sentinel.
SHEPHERD_DEFAULT_SENTINEL_REL=".local/share/shepherdd/.factory-reset-ble"

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

    if [[ -z "$user" ]]; then
        die "Usage: shepherd bluetooth clear --user USER"
    fi

    require_root
    validate_user "$user"

    if user_has_active_session "$user" && [[ "$force" != "true" ]]; then
        die "User '$user' currently has an active login session; pass --force to override (this will race with their running shepherdd)"
    fi

    local home
    home="$(get_user_home "$user")"
    if [[ -z "$home" ]]; then
        die "Could not determine home directory for '$user'"
    fi

    if [[ -z "$admin_record" ]]; then
        admin_record="$home/$SHEPHERD_DEFAULT_ADMIN_REL"
    fi
    if [[ -z "$sentinel" ]]; then
        sentinel="$home/$SHEPHERD_DEFAULT_SENTINEL_REL"
    fi

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
        info "No admin record at $admin_record (user is already unclaimed); skipping BlueZ unpair step"
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

    success "Bluetooth state cleared for user '$user'"
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
    clear     Force-disconnect + unpair the bonded BLE peer for a user
              and delete their admin record so the next session starts
              unclaimed.

Options for 'clear':
    --user USER             Target user (required).
    --admin-record PATH     Override admin.toml location
                            (default: ~USER/$SHEPHERD_DEFAULT_ADMIN_REL).
    --sentinel PATH         Override reset-sentinel location
                            (default: ~USER/$SHEPHERD_DEFAULT_SENTINEL_REL).
    --force                 Proceed even if the user is currently logged in.

Examples:
    sudo shepherd bluetooth clear --user shepherd-kiosk
    sudo shepherd bluetooth clear --user shepherd-kiosk --force
EOF
            ;;
        *)
            die "Unknown bluetooth command: $subcmd (try: shepherd bluetooth help)"
            ;;
    esac
}
