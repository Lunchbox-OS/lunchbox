#!/usr/bin/env bash
# User hardening logic for lunchbox-launcher
# Applies and reverts kiosk-style user restrictions

# Get the directory containing this script
HARDEN_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source common utilities
# shellcheck source=common.sh
source "$HARDEN_LIB_DIR/common.sh"

# State directory for hardening rollback
HARDENING_STATE_DIR="/var/lib/lunchboxd/hardening"

# State for changes that are system-wide rather than per-user.
#
# Some of what hardening does cannot be scoped to one user: `/etc/pam.d` has no
# per-user form, so disabling `user_readenv` disables it for everyone. Backing
# such a file up per user does not compose — harden A, harden B, revert A, and
# A's backup (taken before anything was changed) puts the file back the way it
# was, silently un-hardening B. So they are applied once, tracked here, and
# restored only when the last hardened user is reverted.
#
# Dot-prefixed so the `*/` glob in `hardened_users` skips it, and because a
# username cannot collide with it.
GLOBAL_STATE_DIR="$HARDENING_STATE_DIR/.global"

# The users currently hardened, one per line.
hardened_users() {
    local dir
    shopt -s nullglob
    for dir in "$HARDENING_STATE_DIR"/*/; do
        if [[ -f "$dir/hardened" ]]; then
            basename "$dir"
        fi
    done
    shopt -u nullglob
}

# Save a system-wide file for restoration when the last user is reverted.
global_save_for_restore() {
    local file="$1"
    local backup_path="$GLOBAL_STATE_DIR/backup/${file#/}"

    mkdir -p "$(dirname "$backup_path")"
    if [[ -e "$file" ]]; then
        cp -a "$file" "$backup_path"
        echo "exists" > "$backup_path.meta"
    else
        echo "absent" > "$backup_path.meta"
    fi
}

# Restore every system-wide file saved by global_save_for_restore.
global_restore_all() {
    [[ -d "$GLOBAL_STATE_DIR/backup" ]] || return 0

    local meta backup_path file original_state
    while IFS= read -r meta; do
        [[ -n "$meta" ]] || continue
        backup_path="${meta%.meta}"
        file="/${backup_path#"$GLOBAL_STATE_DIR/backup/"}"
        original_state="$(cat "$meta")"
        if [[ "$original_state" == "exists" ]]; then
            cp -a "$backup_path" "$file"
            info "  Restored: $file"
        else
            rm -f "$file"
            info "  Removed: $file (didn't exist before)"
        fi
    done < <(find "$GLOBAL_STATE_DIR/backup" -name '*.meta' 2>/dev/null || true)
}

# Add a per-user block to a shared config file, idempotently.
#
# Shared files are appended to by every hardened user, so they cannot be
# restored wholesale on revert without discarding another user's block. The
# markers let exactly one user's contribution be taken back out.
add_marked_block() {
    local user="$1" file="$2" body="$3"

    if grep -qF "$(block_begin "$user")" "$file" 2>/dev/null; then
        return 0
    fi
    {
        echo ""
        block_begin "$user"
        printf '%s\n' "$body"
        block_end "$user"
    } >> "$file"
}

# Remove the block add_marked_block wrote, leaving every other user's alone.
remove_marked_block() {
    local user="$1" file="$2"

    [[ -f "$file" ]] || return 0
    if grep -qF "$(block_begin "$user")" "$file" 2>/dev/null; then
        sed -i "\|^$(block_begin "$user")$|,\|^$(block_end "$user")$|d" "$file"
        info "  Removed hardening block for $user from: $file"
    fi
}

block_begin() { echo "# BEGIN lunchbox hardening for user: $1"; }
block_end()   { echo "# END lunchbox hardening for user: $1"; }

# Get the state directory for a user
get_user_state_dir() {
    local user="$1"
    echo "$HARDENING_STATE_DIR/$user"
}

# Check if a user is currently hardened
is_hardened() {
    local user="$1"
    local state_dir
    state_dir="$(get_user_state_dir "$user")"
    
    [[ -f "$state_dir/hardened" ]]
}

# Save a file for later restoration
save_for_restore() {
    local user="$1"
    local file="$2"
    local state_dir
    state_dir="$(get_user_state_dir "$user")"
    
    local relative_path="${file#/}"
    local backup_path="$state_dir/backup/$relative_path"
    
    mkdir -p "$(dirname "$backup_path")"
    
    if [[ -e "$file" ]]; then
        cp -a "$file" "$backup_path"
        echo "exists" > "$backup_path.meta"
    else
        echo "absent" > "$backup_path.meta"
    fi
}

# Restore a previously saved file
restore_file() {
    local user="$1"
    local file="$2"
    local state_dir
    state_dir="$(get_user_state_dir "$user")"
    
    local relative_path="${file#/}"
    local backup_path="$state_dir/backup/$relative_path"
    local meta_file="$backup_path.meta"
    
    if [[ ! -f "$meta_file" ]]; then
        warn "No backup metadata for $file, skipping"
        return 0
    fi
    
    local original_state
    original_state="$(cat "$meta_file")"
    
    if [[ "$original_state" == "exists" ]]; then
        if [[ -e "$backup_path" ]]; then
            cp -a "$backup_path" "$file"
            info "  Restored: $file"
        else
            warn "Backup file missing for $file"
        fi
    else
        # File didn't exist originally, remove it
        rm -f "$file"
        info "  Removed: $file (didn't exist before)"
    fi
}

# Record a change action for rollback
record_action() {
    local user="$1"
    local action="$2"
    local target="$3"
    local state_dir
    state_dir="$(get_user_state_dir "$user")"
    
    echo "$action|$target" >> "$state_dir/actions.log"
}

# Apply the parts of hardening that are system-wide (issue #144).
#
# Idempotent and refcounted: the first hardened user applies these, any later
# one finds them already in place, and `revert_global_hardening_if_last` puts
# them back only when no hardened users remain.
apply_global_hardening() {
    local user="$1"
    local user_home
    user_home="$(get_user_home "$user")"

    info "Applying system-wide settings..."

    if [[ -f "$GLOBAL_STATE_DIR/applied" ]]; then
        info "  Already in place for: $(hardened_users | tr '\n' ' ')"
        return 0
    fi

    mkdir -p "$GLOBAL_STATE_DIR/backup"
    chmod 0700 "$GLOBAL_STATE_DIR"

    # -- getty auto-login override (inert; documents how to turn it on) -----
    local getty_override_dir="/etc/systemd/system/getty@tty1.service.d"
    local getty_override="$getty_override_dir/lunchbox-autologin.conf"

    global_save_for_restore "$getty_override"
    mkdir -p "$getty_override_dir"
    cat > "$getty_override" <<EOF
# Lunchbox hardening: auto-login for the kiosk user
# Uncomment the following lines to enable auto-login to tty1
# [Service]
# ExecStart=
# ExecStart=-/sbin/agetty --autologin $user --noclear %I \$TERM
EOF
    chmod 0644 "$getty_override"

    # -- stop PAM reading the user's own environment file -------------------
    #
    # `pam_env`'s `user_readenv=1` makes PAM read `~/.pam_environment` -- a file
    # the kiosk user owns -- and hand what it says to the session it is opening.
    # That is the session lunchboxd runs in, so without this the kiosk user, and
    # therefore every activity running as that uid, chooses the daemon's whole
    # environment. Ubuntu 26.04 ships it enabled on every GDM service.
    #
    # Deleting `~/.pam_environment` instead would not work: the user owns their
    # home directory, so they can remove a root-owned file there and put their
    # own back. The configuration that reads it is what has to go, and that
    # lives in root-owned `/etc/pam.d`.
    info "  Disabling PAM's reading of user environment files..."

    local pam_file changed=0
    while IFS= read -r pam_file; do
        [[ -n "$pam_file" ]] || continue
        global_save_for_restore "$pam_file"
        sed -i 's/[[:space:]]user_readenv=1//g' "$pam_file"
        info "    Disabled user_readenv in: $pam_file"
        changed=$((changed + 1))
    done < <(grep -rlE '^[^#]*user_readenv=1' /etc/pam.d/ 2>/dev/null || true)

    if [[ "$changed" -eq 0 ]]; then
        info "    No PAM service reads user environment files"
    fi

    # A rename or reformat upstream that left one enabled would ship a device
    # where the kiosk user still picks the session environment, so check rather
    # than trust the sed.
    #
    # The check globs where the loop above recursed, deliberately: `grep -r`
    # does not follow symlinks and /etc/pam.d is full of them (`gdm-smartcard`
    # -> /etc/alternatives/...). Editing through a symlink would be wrong --
    # `sed -i` replaces the link with a regular file -- so the loop is right to
    # skip them, but a symlink whose target is still enabled has to be caught
    # rather than passed over by the same blind spot.
    #
    # `|| true` because finding nothing is the success case, and `grep` exits 1
    # for it: under `set -o pipefail` that would abort the whole run, which is
    # exactly what happened the first time this was written.
    local still_enabled
    still_enabled="$(grep -lE '^[^#]*user_readenv=1' /etc/pam.d/* 2>/dev/null || true)"
    if [[ -n "$still_enabled" ]]; then
        die "Failed to disable user_readenv in $(echo "$still_enabled" | tr '\n' ' ')- the kiosk user could still set the session environment (issue #144)"
    fi

    # Its presence is not a problem once nothing reads it, but it is worth
    # saying: on a device nothing legitimate writes this file.
    if [[ -e "$user_home/.pam_environment" ]]; then
        warn "  $user_home/.pam_environment exists; nothing reads it now, but nothing on a device should have written it"
    fi

    date -Iseconds > "$GLOBAL_STATE_DIR/applied"
}

# Undo apply_global_hardening, but only once nobody is hardened any more.
revert_global_hardening_if_last() {
    local remaining
    remaining="$(hardened_users | tr '\n' ' ')"

    if [[ -n "${remaining// /}" ]]; then
        info "Leaving system-wide settings in place; still hardened: $remaining"
        return 0
    fi

    [[ -f "$GLOBAL_STATE_DIR/applied" ]] || return 0

    info "Restoring system-wide settings (no hardened users remain)..."
    global_restore_all
    rm -rf "$GLOBAL_STATE_DIR"
}

# Apply hardening to a user
harden_apply() {
    local user="$1"
    
    require_root
    validate_user "$user"
    
    local state_dir
    state_dir="$(get_user_state_dir "$user")"
    local user_home
    user_home="$(get_user_home "$user")"
    
    if is_hardened "$user"; then
        warn "User $user is already hardened. Use 'lunchbox harden revert' first."
        return 0
    fi
    
    info "Applying hardening to user: $user"
    
    # Create state directory
    mkdir -p "$state_dir/backup"
    chmod 0700 "$state_dir"
    
    # Initialize actions log
    : > "$state_dir/actions.log"
    
    # =========================================================================
    # 1. Set user shell to restricted shell or nologin for non-sway access
    # =========================================================================
    info "Configuring user shell..."
    
    local original_shell
    original_shell="$(getent passwd "$user" | cut -d: -f7)"
    echo "$original_shell" > "$state_dir/original_shell"
    
    # Keep bash for sway to work, but we'll restrict other access methods
    # The shell restriction is handled by PAM and session limits instead
    record_action "$user" "shell" "$original_shell"
    
    # =========================================================================
    # 2. Configure user's .bashrc to be restricted
    # =========================================================================
    info "Configuring shell restrictions..."
    
    local bashrc="$user_home/.bashrc"
    save_for_restore "$user" "$bashrc"
    
    # Append restriction to bashrc (if not in sway, exit)
    cat >> "$bashrc" <<'EOF'

# Lunchbox hardening: restrict to sway session only
if [[ -z "${WAYLAND_DISPLAY:-}" ]] && [[ -z "${SWAYSOCK:-}" ]]; then
    echo "This account is restricted to the Lunchbox kiosk environment."
    exit 1
fi
EOF
    chown "$user:$user" "$bashrc"
    record_action "$user" "file" "$bashrc"
    
    # =========================================================================
    # 3. Disable SSH access for this user
    # =========================================================================
    info "Restricting SSH access..."
    
    local lunchbox_sshd_config="/etc/ssh/sshd_config.d/lunchbox-$user.conf"
    
    save_for_restore "$user" "$lunchbox_sshd_config"
    
    # Create a drop-in config to deny this user
    mkdir -p /etc/ssh/sshd_config.d
    cat > "$lunchbox_sshd_config" <<EOF
# Lunchbox hardening: deny SSH access for kiosk user
DenyUsers $user
EOF
    chmod 0644 "$lunchbox_sshd_config"
    record_action "$user" "file" "$lunchbox_sshd_config"
    
    # Reload sshd if running
    if systemctl is-active --quiet sshd 2>/dev/null || systemctl is-active --quiet ssh 2>/dev/null; then
        systemctl reload sshd 2>/dev/null || systemctl reload ssh 2>/dev/null || true
    fi
    
    # =========================================================================
    # 4. Disable virtual console (TTY) access via PAM
    # =========================================================================
    info "Restricting console access..."
    
    local pam_access="/etc/security/access.conf"

    # A marked block rather than a whole-file backup: every hardened user
    # appends to this file, so restoring it wholesale on revert would discard
    # the other users' rules along with this one's.
    add_marked_block "$user" "$pam_access" \
"# Deny console login for kiosk user (allow display manager access)
-:$user:tty1 tty2 tty3 tty4 tty5 tty6 tty7"
    record_action "$user" "block" "$pam_access"
    
    # =========================================================================
    # 5. Lock down sudo access
    # =========================================================================
    info "Restricting sudo access..."
    
    local sudoers_file="/etc/sudoers.d/lunchbox-$user"
    save_for_restore "$user" "$sudoers_file"
    
    # Explicitly deny sudo for this user
    cat > "$sudoers_file" <<EOF
# Lunchbox hardening: deny sudo access for kiosk user
$user ALL=(ALL) !ALL
EOF
    chmod 0440 "$sudoers_file"
    record_action "$user" "file" "$sudoers_file"
    
    # =========================================================================
    # 6. Set restrictive file permissions on user home
    # =========================================================================
    info "Securing home directory permissions..."
    
    # Save original permissions
    stat -c "%a" "$user_home" > "$state_dir/home_perms"
    
    # Set restrictive permissions
    chmod 0700 "$user_home"
    record_action "$user" "perms" "$user_home"
    
    # =========================================================================
    # 7. System-wide settings, applied once for however many users are hardened
    # =========================================================================
    apply_global_hardening "$user"

    # =========================================================================
    # Mark as hardened
    # =========================================================================
    date -Iseconds > "$state_dir/hardened"
    echo "$user" > "$state_dir/user"
    
    success "Hardening applied to user: $user"
    info ""
    info "The following restrictions are now active:"
    info "  - SSH access denied"
    info "  - Console (TTY) login restricted"
    info "  - Sudo access denied"
    info "  - Shell restricted to Sway sessions"
    info "  - Home directory secured (mode 0700)"
    info "  - PAM no longer reads ~/.pam_environment (system-wide, issue #144)"
    info ""
    info "To revert: lunchbox harden revert --user $user"
}

# Revert hardening from a user
harden_revert() {
    local user="$1"
    
    require_root
    validate_user "$user"
    
    local state_dir
    state_dir="$(get_user_state_dir "$user")"
    local user_home
    user_home="$(get_user_home "$user")"
    
    if ! is_hardened "$user"; then
        warn "User $user is not currently hardened."
        return 0
    fi
    
    info "Reverting hardening for user: $user"
    
    # =========================================================================
    # Restore all saved files
    # =========================================================================
    if [[ -f "$state_dir/actions.log" ]]; then
        while IFS='|' read -r action target; do
            case "$action" in
                file)
                    restore_file "$user" "$target"
                    ;;
                block)
                    remove_marked_block "$user" "$target"
                    ;;
                perms)
                    if [[ -f "$state_dir/home_perms" ]]; then
                        local original_perms
                        original_perms="$(cat "$state_dir/home_perms")"
                        chmod "$original_perms" "$target"
                        info "  Restored permissions on: $target"
                    fi
                    ;;
                shell)
                    # Shell wasn't changed, nothing to revert
                    ;;
            esac
        done < "$state_dir/actions.log"
    fi
    
    # =========================================================================
    # Reload services that may have been affected
    # =========================================================================
    if systemctl is-active --quiet sshd 2>/dev/null || systemctl is-active --quiet ssh 2>/dev/null; then
        systemctl reload sshd 2>/dev/null || systemctl reload ssh 2>/dev/null || true
    fi
    
    # =========================================================================
    # Clean up state directory
    # =========================================================================
    # Before the refcount is read, so this user no longer counts as hardened.
    rm -rf "$state_dir"

    # =========================================================================
    # System-wide settings, only once nobody is hardened any more
    # =========================================================================
    revert_global_hardening_if_last

    success "Hardening reverted for user: $user"
    info ""
    info "All restrictions have been removed. The user can now:"
    info "  - Access via SSH"
    info "  - Login at console"
    info "  - Use sudo (if previously allowed)"
    info "  - Set the session environment via ~/.pam_environment, if no other"
    info "    user is still hardened (issue #144)"
}

# Show hardening status
harden_status() {
    local user="$1"
    
    validate_user "$user"
    
    local state_dir
    state_dir="$(get_user_state_dir "$user")"
    
    if is_hardened "$user"; then
        local hardened_date
        hardened_date="$(cat "$state_dir/hardened")"
        echo "User '$user' is HARDENED (since $hardened_date)"
        
        if [[ -f "$state_dir/actions.log" ]]; then
            echo ""
            echo "Applied restrictions:"
            while IFS='|' read -r action target; do
                echo "  - $action: $target"
            done < "$state_dir/actions.log"
        fi
    else
        echo "User '$user' is NOT hardened"
    fi
}

# Main harden command dispatcher
harden_main() {
    local subcmd="${1:-}"
    shift || true
    
    local user=""
    
    # Parse remaining arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --user)
                user="$2"
                shift 2
                ;;
            *)
                die "Unknown option: $1"
                ;;
        esac
    done
    
    case "$subcmd" in
        apply)
            if [[ -z "$user" ]]; then
                die "Usage: lunchbox harden apply --user USER"
            fi
            harden_apply "$user"
            ;;
        revert)
            if [[ -z "$user" ]]; then
                die "Usage: lunchbox harden revert --user USER"
            fi
            harden_revert "$user"
            ;;
        status)
            if [[ -z "$user" ]]; then
                die "Usage: lunchbox harden status --user USER"
            fi
            harden_status "$user"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: lunchbox harden <command> --user USER

Commands:
    apply     Apply kiosk hardening to a user
    revert    Revert hardening and restore original state
    status    Show hardening status for a user

Options:
    --user USER    Target user for hardening operations (required)

Hardening includes:
    - Denying SSH access
    - Restricting console (TTY) login
    - Denying sudo access
    - Restricting shell to Sway sessions only
    - Securing home directory permissions

State is preserved in: $HARDENING_STATE_DIR/<user>/

Examples:
    lunchbox harden apply --user kiosk
    lunchbox harden status --user kiosk
    lunchbox harden revert --user kiosk
EOF
            ;;
        *)
            die "Unknown harden command: $subcmd (try: lunchbox harden help)"
            ;;
    esac
}
