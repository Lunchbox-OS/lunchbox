#!/usr/bin/env bash
# Web management authentication admin (issue #156).
#
# One operation, and it exists because a password can be forgotten: forget the
# credential store so the device mints a fresh setup code and shows it on its
# own screen.
#
# This is the *last* resort, not the first. A parent with the companion app
# paired can set a new password from the phone — Device controls → Web access →
# Change password — without a shell, and that path should be offered before
# this one. What this covers is a device with no paired phone, or a phone that
# is gone.
#
# Deliberately destructive about sessions: every signed-in browser goes with
# the password. A password reset the owner did not perform is exactly the case
# where the live sessions are the problem.

# Where the credential store lives on a device with the state custodian. On a
# dev checkout it is the daemon's data directory instead, and `--store` names it.
webauth_default_store() {
    echo "$STATED_ADMIN_DIR/$SHEPHERD_WEB_AUTH_FILE"
}

webauth_reset() {
    local store=""
    local force=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --store)
                store="$2"
                shift 2
                ;;
            --force)
                force=true
                shift
                ;;
            *)
                die "Unknown option: $1 (try: shepherd web-auth help)"
                ;;
        esac
    done

    [[ -n "$store" ]] || store="$(webauth_default_store)"

    # Root only when touching the custodian's directory, which is the whole
    # point of that directory. A `--store` under someone's own home (the dev
    # stack) needs nothing.
    if [[ "$store" == "$STATED_ADMIN_DIR"/* ]]; then
        require_root
    fi

    if [[ ! -f "$store" ]]; then
        warn "No credential store at $store — this device already has no web password."
        info "It will mint a setup code the next time shepherdd starts."
        return 0
    fi

    if [[ "$force" != true ]]; then
        info "This will remove the web management password and sign out every browser."
        info "A paired companion can change the password without doing this:"
        info "  Device controls -> Web access -> Change password"
        read -r -p "Remove it anyway? [y/N] " reply
        [[ "$reply" =~ ^[Yy]$ ]] || die "Cancelled."
    fi

    rm -f "$store"
    success "Removed $store"
    info "Restart shepherdd. A new setup code appears on the device's screen and in the journal."
}

webauth_main() {
    local subcmd="${1:-}"
    shift || true

    case "$subcmd" in
        reset)
            webauth_reset "$@"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd web-auth <command> [options]

Commands:
    reset     Forget the web management password and every signed-in browser,
              so the device mints a fresh setup code and shows it on screen.

              Try the companion app first: a paired phone can set a new
              password from Device controls -> Web access, with no shell and
              without signing anybody out.

Options for 'reset':
    --store PATH    Where the credential store is (default:
                    $(webauth_default_store) on a device with the state
                    custodian; on a dev checkout it is
                    dev-runtime/data/$SHEPHERD_WEB_AUTH_FILE).
    --force         Don't ask.

Examples:
    sudo shepherd web-auth reset
    shepherd web-auth reset --store dev-runtime/data/$SHEPHERD_WEB_AUTH_FILE
EOF
            ;;
        *)
            die "Unknown web-auth command: $subcmd (try: shepherd web-auth help)"
            ;;
    esac
}
