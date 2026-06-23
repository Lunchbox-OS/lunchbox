#!/usr/bin/env bash
# Install the GNOME swipe keyboard for the gdm login screen (reproducible, idempotent).
#
# The greeter runs as the `gdm` user with its own session bus and no logged-in identity, so a
# login-screen keyboard needs four things, which this script sets up:
#   1. the extension installed system-wide (gdm doesn't read ~/.local/share),
#   2. the extension enabled in gdm's dconf,
#   3. the decode daemon reachable on the greeter's session bus, and
#   4. a signed bundle readable by the `gdm` user.
#
# Design notes:
#   - The daemon is wired as a **D-Bus activated** service, so it starts on demand on whichever
#     session bus calls it (the greeter's, and any user session that hasn't already started its
#     own). The greeter has no user, so it uses the **adult** profile; password fields are still
#     tap-only via the safety gate, so no password text is ever decoded.
#   - For a logged-in CHILD session, shepherd-launcher should start the daemon with
#     `--profile child` at session start (owning the bus name first), so the adult activation
#     here never triggers there. That user-session launch is separate (Phase 5).
#   - Ubuntu's gdm uses a `file-db:` greeter dconf db and has no `system-db:gdm`, so we add a
#     `system-db:gdm` line via an /etc/dconf/profile override (which wins over /usr/share) and
#     drop a keyfile in /etc/dconf/db/gdm.d, preserving the existing greeter defaults.
#
# Usage: sudo scripts/install-keyboard-gnome-gdm.sh [install|uninstall|status] [--dry-run]
#        (omit the subcommand to default to install). --dry-run prints actions without doing them.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

UUID="shepherd-swipe@armeafamily.com"
DBUS_NAME="com.armeafamily.ShepherdSwipe"

EXT_SRC="$REPO_ROOT/crates/shepherd-keyboard-gnome-daemon/extension"
EXT_DST="/usr/share/gnome-shell/extensions/$UUID"
DAEMON_DST="/usr/libexec/shepherd-keyboard-gnome-daemon"
BUNDLE_SRC="${SHEPHERD_SWIPE_BUNDLE_DIR:-$REPO_ROOT/dev-runtime/swipe-bundles}"
BUNDLE_DST="/var/lib/shepherd/swipe-bundles"
DBUS_SERVICE="/usr/share/dbus-1/services/${DBUS_NAME}.service"
DCONF_KEYFILE="/etc/dconf/db/gdm.d/90-shepherd-swipe"
GDM_GREETER_DB="/var/lib/gdm3/greeter-dconf-defaults"
GDM_PROFILES=(/etc/dconf/profile/gdm /etc/dconf/profile/Debian-gdm)
GDM_PROFILE="adult" # greeter has no user identity; password fields stay tap-only regardless

DRY_RUN=false
CMD="install"
for arg in "$@"; do
    case "$arg" in
        install|uninstall|status) CMD="$arg" ;;
        --dry-run) DRY_RUN=true ;;
        *) die "unknown argument: $arg" ;;
    esac
done

run() {
    if $DRY_RUN; then echo "  RUN  $*"; else "$@"; fi
}

# Write stdin to a file (root-owned, 0644). Honors --dry-run.
emit() {
    local path="$1"
    if $DRY_RUN; then
        echo "  WRITE $path:"
        sed 's/^/        | /'
    else
        mkdir -p "$(dirname "$path")"
        cat >"$path"
        chmod 0644 "$path"
    fi
}

require_root() {
    $DRY_RUN && return 0
    [[ $EUID -eq 0 ]] || die "must run as root (use sudo); or pass --dry-run to preview"
}

find_daemon() {
    for cand in "$REPO_ROOT/target/release/shepherd-keyboard-gnome-daemon" \
                "$REPO_ROOT/target/debug/shepherd-keyboard-gnome-daemon"; do
        [[ -x "$cand" ]] && {
            echo "$cand"
            return 0
        }
    done
    return 1
}

do_install() {
    require_root

    local daemon_bin
    daemon_bin="$(find_daemon)" || die "daemon not built: cargo build --release -p shepherd-keyboard-gnome-daemon"
    [[ "$daemon_bin" == *"/release/"* ]] || warn "using a debug daemon build ($daemon_bin); prefer a release build for deployment"
    [[ -d "$EXT_SRC" ]] || die "extension source missing: $EXT_SRC"
    [[ -d "$BUNDLE_SRC/adult" ]] || die "no adult bundle at $BUNDLE_SRC/adult; run scripts/fetch-swipe-bundles.sh first"

    info "Installing extension to $EXT_DST"
    run rm -rf "$EXT_DST"
    run mkdir -p "$EXT_DST"
    run cp -rT "$EXT_SRC" "$EXT_DST"
    run chmod -R a+rX "$EXT_DST"

    info "Installing daemon to $DAEMON_DST"
    run install -D -m 0755 "$daemon_bin" "$DAEMON_DST"

    info "Staging bundle to $BUNDLE_DST (readable by the gdm user)"
    run mkdir -p "$BUNDLE_DST"
    run cp -rT "$BUNDLE_SRC" "$BUNDLE_DST"
    run chown -R root:root "$BUNDLE_DST"
    run chmod -R a+rX "$BUNDLE_DST"

    info "Writing D-Bus activation service $DBUS_SERVICE"
    emit "$DBUS_SERVICE" <<EOF
[D-BUS Service]
Name=$DBUS_NAME
Exec=$DAEMON_DST --profile $GDM_PROFILE --bundle-dir $BUNDLE_DST
EOF

    info "Enabling the extension for gdm via dconf"
    for prof in "${GDM_PROFILES[@]}"; do
        # Override the stock profile (under /etc, which wins over /usr/share) to add a
        # system-db, while preserving the greeter's existing file-db defaults.
        emit "$prof" <<EOF
user-db:user
system-db:gdm
file-db:$GDM_GREETER_DB
EOF
    done
    emit "$DCONF_KEYFILE" <<EOF
# Enables the Shepherd swipe keyboard on the GNOME login screen. NOTE: this sets the whole
# enabled-extensions array for gdm; merge in any other gdm extensions here if you add them.
[org/gnome/shell]
enabled-extensions=['$UUID']
EOF
    run dconf update

    success "gdm packaging installed."
    cat <<EOF

Apply it by restarting the greeter (this logs out the current graphical session!):
    sudo systemctl restart gdm

Then at the login screen, focusing a text field shows the keyboard; the password field is
tap-only (no swipe, no suggestions). Watch the greeter logs with:
    sudo journalctl -u gdm -b   /   journalctl _UID=$(id -u gdm) -b
EOF
}

do_uninstall() {
    require_root
    info "Removing gdm packaging"
    run rm -rf "$EXT_DST"
    run rm -f "$DAEMON_DST"
    run rm -f "$DBUS_SERVICE"
    run rm -f "$DCONF_KEYFILE"
    for prof in "${GDM_PROFILES[@]}"; do
        # Only remove the /etc override we created (falls back to the stock /usr/share profile).
        run rm -f "$prof"
    done
    run rm -rf "$BUNDLE_DST"
    run dconf update
    success "gdm packaging removed. Restart gdm to apply: sudo systemctl restart gdm"
}

do_status() {
    local rows=(
        "extension|$EXT_DST"
        "daemon|$DAEMON_DST"
        "bundle (adult)|$BUNDLE_DST/adult"
        "dbus service|$DBUS_SERVICE"
        "dconf keyfile|$DCONF_KEYFILE"
        "dconf profile (gdm)|/etc/dconf/profile/gdm"
    )
    for row in "${rows[@]}"; do
        local label="${row%%|*}" path="${row#*|}"
        if [[ -e "$path" ]]; then printf '  [ok]      %-18s %s\n' "$label" "$path"; else printf '  [missing] %-18s %s\n' "$label" "$path"; fi
    done
}

case "$CMD" in
    install) do_install ;;
    uninstall) do_uninstall ;;
    status) do_status ;;
esac
