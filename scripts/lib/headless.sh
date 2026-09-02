#!/usr/bin/env bash
# Headless, agent-drivable development session for shepherd-launcher.
#
# `shepherd dev run` boots the launcher in a *nested* Sway
# (WLR_BACKENDS=wayland), which must attach to a parent Wayland compositor and
# therefore requires a graphical login session. That makes it impossible for a
# headless agent (or a developer over SSH) to bring the stack up and see it.
#
# This library boots the SAME stack (same sway.conf, same config.example.toml,
# same debug binaries) against the *headless* wlroots backend the shepherd-e2e
# harness already uses (WLR_BACKENDS=headless, pixman software renderer). That
# needs no login session, no parent compositor, and no GPU. The result is a
# virtual output that can be:
#   - screenshotted with `grim` (wlr-screencopy reads the pixman buffer),
#   - inspected with `swaymsg -t get_tree` (structural assertions),
#   - driven with `wtype` (keys/text) and `swaymsg seat cursor` (pointer).
#
# The session runs detached; connection details are written to
# dev-runtime/headless/session.env so later `shepherd dev {shot,tree,key,...}`
# invocations can reattach.

HEADLESS_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$HEADLESS_LIB_DIR/common.sh"
# shellcheck source=build.sh
source "$HEADLESS_LIB_DIR/build.sh"
# Reuse sway_setup_env / sway_kill_existing / sway_purge_stale_sockets.
# shellcheck source=sway.sh
source "$HEADLESS_LIB_DIR/sway.sh"

# Layout under the dev runtime dir.
HEADLESS_DIR_DEFAULT="./dev-runtime/headless"
HEADLESS_SIZE_DEFAULT="1280x720"
HEADLESS_OUTPUT="HEADLESS-1"

headless_session_env() { echo "$HEADLESS_DIR_DEFAULT/session.env"; }
headless_log()         { echo "$HEADLESS_DIR_DEFAULT/sway.log"; }

# Print the names (one per line) of the live wayland-N sockets in a runtime dir.
# Glob + condition rather than `ls | grep` (shellcheck SC2010).
headless_wayland_sockets() {
    local rt="$1" f base
    for f in "$rt"/wayland-[0-9]*; do
        [[ -S "$f" ]] || continue
        base="$(basename "$f")"
        [[ "$base" =~ ^wayland-[0-9]+$ ]] && printf '%s\n' "$base"
    done
}

# Run a wayland/ipc client (swaymsg/grim/wtype) against the session, as the user
# that owns it. When the session runs as a different user (`--user`), the tool is
# invoked via `sudo -u` with the session's XDG_RUNTIME_DIR/SWAYSOCK/WAYLAND_DISPLAY
# (the runtime dir is mode 0700, so the invoker cannot reach the sockets directly).
# When the session runs as the invoker, it runs the tool inline. Reads
# SWAYSOCK/WAYLAND_DISPLAY/XDG_RUNTIME_DIR/SHEPHERD_HEADLESS_USER from the env.
headless_run() {
    if [[ -n "${SHEPHERD_HEADLESS_USER:-}" && "$SHEPHERD_HEADLESS_USER" != "$(id -un)" ]]; then
        # `sudo -u` (not maybe_sudo): switching *to* a user; root can sudo too.
        sudo -u "$SHEPHERD_HEADLESS_USER" env \
            XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
            SWAYSOCK="$SWAYSOCK" \
            WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" \
            "$@"
    else
        env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" SWAYSOCK="$SWAYSOCK" \
            WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" "$@"
    fi
}

# Load a previously-written session and export SWAYSOCK/WAYLAND_DISPLAY/etc.
# Exits non-zero (quietly, when $1 == "quiet") if no live session is found.
headless_load_session() {
    local quiet="${1:-}"
    local envf; envf="$(headless_session_env)"
    if [[ ! -f "$envf" ]]; then
        [[ "$quiet" == "quiet" ]] && return 1
        die "No headless session found. Start one with: shepherd dev headless"
    fi
    # shellcheck source=/dev/null
    source "$envf"
    export SWAYSOCK WAYLAND_DISPLAY XDG_RUNTIME_DIR SHEPHERD_HEADLESS_USER
    # Liveness via /proc, not `kill -0`: the process may be owned by another user
    # (--user mode), where kill -0 fails with EPERM even though it is alive.
    if [[ -z "${SHEPHERD_HEADLESS_PID:-}" ]] || [[ ! -d "/proc/$SHEPHERD_HEADLESS_PID" ]]; then
        [[ "$quiet" == "quiet" ]] && return 1
        die "Recorded headless session (pid ${SHEPHERD_HEADLESS_PID:-?}) is not running. Re-run: shepherd dev headless"
    fi
    return 0
}

# Wait for a socket to appear, checking as its owner ($2 empty = the invoker).
#   headless_wait_socket <path> <user|""> <deciseconds>
headless_wait_socket() {
    local path="$1" who="$2" tries="${3:-100}"
    for _ in $(seq 1 "$tries"); do
        if [[ -n "$who" && "$who" != "$(id -un)" ]]; then
            sudo -u "$who" test -S "$path" && return 0
        else
            [[ -S "$path" ]] && return 0
        fi
        sleep 0.1
    done
    return 1
}

# Prefix that runs the session in a cgroup the peer check can mean something in.
#
# `PeerPolicy::restricted()` degrades wherever shepherd's cgroup is one an
# activity could join, and a stack started from a shell sits under
# `user@<uid>.service` — the user manager's delegated subtree, which anything at
# this uid can join. A **system**-manager scope owned by the same uid is not
# delegated, which is structurally what a display manager's session scope is, so
# the check arms there exactly as it does on a device.
#
# The two `--setenv`s are load-bearing rather than tidy: without them
# `systemd-run --user --scope` inside the session cannot reach the user bus, so
# activities get no cgroup of their own — and the daemon then refuses to arm the
# peer check anyway, correctly, because it would be separating nothing.
#
#   headless_scope_prefix <runtime_dir>
headless_scope_prefix() {
    local rt="$1" uid gid
    uid="$(id -u)"; gid="$(id -g)"
    printf '%s\n' \
        sudo systemd-run --uid="$uid" --gid="$gid" --scope \
        --slice="user-$uid.slice" --unit="shepherd-dev-headless-$$" --quiet --collect \
        "--setenv=XDG_RUNTIME_DIR=$rt" \
        "--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$uid/bus" \
        "--setenv=HOME=$HOME" "--setenv=USER=${USER:-$(id -un)}" "--setenv=PATH=$PATH"
}

# Wait until the compositor exists, by either name.
#
# Under `--harden-ipc` (and on a device, where hardening is the default)
# shepherdd unlinks the socket sway created as soon as it has connected, so the
# ambient name is short-lived and a harness that only watched for it would race. Either name proves sway is up, which is all this
# stage claims — whether *shepherdd* is up is the separate wait on the alias.
#   headless_wait_compositor <runtime_dir> <user|""> <alias> <deciseconds>
headless_wait_compositor() {
    local rt="$1" who="$2" alias="$3" tries="${4:-100}"
    local probe="self"; [[ -n "$who" ]] && probe="$who"
    for _ in $(seq 1 "$tries"); do
        [[ -n "$(headless_socket_as "$rt" "$probe" ipc)" ]] && return 0
        if [[ -n "$who" && "$who" != "$(id -un)" ]]; then
            sudo -u "$who" test -S "$alias" && return 0
        else
            [[ -S "$alias" ]] && return 0
        fi
        sleep 0.1
    done
    return 1
}

# Poll `swaymsg` until it answers or we time out (deciseconds in $1).
headless_wait_ipc() {
    local tries="${1:-100}"
    for _ in $(seq 1 "$tries"); do
        headless_run swaymsg -t get_version >/dev/null 2>&1 && return 0
        sleep 0.1
    done
    return 1
}

# Poll the tree until the launcher surface is mapped (deciseconds in $1).
# Wait for the launcher to map a surface, in 0.1s ticks.
#
# The budget is generous because GTK can stall long before it paints: with no
# `xdg-desktop-portal` answering — an SSH-only box with no graphical login, which
# is exactly where this harness is meant to run — each of two portal lookups
# blocks for 25s, so the UI appears about 50s in. A 20s budget reported that as
# "surface not detected", which reads as a broken stack rather than a slow one,
# and cost an afternoon of chasing a regression that was not there.
#
# `hint_after` keeps the shorter feedback: past that point it says once that it
# is still waiting and why, so a genuinely dead launcher is not a silent
# minute-and-a-half.
#   headless_wait_launcher [tries] [hint_after]
headless_wait_launcher() {
    local tries="${1:-900}" hint_after="${2:-200}" i=0
    # The launcher registers as "org.shepherd.launcher" (older builds used the
    # bare "shepherd-launcher"); accept either.
    for _ in $(seq 1 "$tries"); do
        if headless_run swaymsg -t get_tree 2>/dev/null \
            | grep -qE '"app_id": *"(org\.)?shepherd[.-]launcher"'; then
            return 0
        fi
        i=$((i + 1))
        if [[ "$i" -eq "$hint_after" ]]; then
            info "Still waiting for the launcher to paint. GTK blocks ~25s per"
            info "portal lookup when no xdg-desktop-portal answers, so this can"
            info "take about a minute on a box with no graphical login."
        fi
        sleep 0.1
    done
    return 1
}

# List the sole sway-ipc / wayland socket in a runtime dir *as its owner*
# ($2 = "self" for the invoker, else a username to sudo to). Used to discover a
# --user session's sockets, since its runtime dir is mode 0700.
#   headless_socket_as <runtime_dir> <user|self> ipc|wayland
headless_socket_as() {
    local rt="$1" who="$2" kind="$3" pat
    case "$kind" in
        ipc)     pat='sway-ipc.*.sock' ;;
        wayland) pat='wayland-[0-9]*' ;;
        *) return 1 ;;
    esac
    # Single quotes are deliberate: $1/$f expand in the child sh, not here.
    # Trailing `exit 0` so a no-match probe returns success — otherwise the sh
    # exits on the final failed `[ -S ]`, and `sock=$(...)` trips `set -e`.
    # shellcheck disable=SC2016
    local script='for f in "$1"/'"$pat"'; do [ -S "$f" ] && { printf "%s\n" "$f"; break; }; done; exit 0'
    if [[ "$who" == "self" ]]; then
        sh -c "$script" _ "$rt"
    else
        sudo -u "$who" sh -c "$script" _ "$rt"
    fi
}

# Verify a target user can actually reach the repo tree the session needs
# (readable config, readable derived sway config, executable debug binaries).
# A dev checkout under a mode-0700 $HOME is the common failure; fail early and
# actionably rather than watching sway die in the log.
headless_precheck_user() {
    local user="$1" repo_root="$2" sway_config="$3" config="$4" b
    sudo -u "$user" test -r "$sway_config" \
        || die "$user cannot read $sway_config — is the repo readable/traversable by $user? (a dev checkout under a 0700 \$HOME is not)"
    if [[ -n "$config" ]]; then
        sudo -u "$user" test -r "$config" \
            || die "$user cannot read the config $config"
    fi
    for b in shepherdd shepherd-launcher shepherd-hud; do
        sudo -u "$user" test -x "$repo_root/target/debug/$b" \
            || die "$user cannot execute $repo_root/target/debug/$b — is the repo traversable by $user?"
    done
}

# Start a detached headless session.
#   --config PATH   shepherdd config to boot
#                   (default: ./config.example.toml, or the target user's
#                    ~/.config/shepherd/config.toml under --user)
#   --user NAME     run the whole stack as NAME (via sudo) for a more realistic
#                   session — that user's groups, HOME, and default config
#   --size WxH      virtual output resolution (default 1280x720)
#   --time "..."    SHEPHERD_MOCK_TIME passthrough (e.g. "2025-12-25 21:00:00")
#   --gpu           use the GL renderer against a DRM node instead of pixman
#   --no-build      skip the cargo build (use existing target/debug binaries)
#   --harden-ipc-peers
#                   exercise the production management-socket peer check: only
#                   shepherdd's own cgroup and root may drive the daemon (issue
#                   #144). Needs sudo, because the check only means anything in
#                   a cgroup an activity cannot join, and a stack started from a
#                   shell sits in the user manager's delegated subtree; the
#                   session runs in a system-manager scope instead. Fails loudly
#                   if the daemon degrades rather than arms.
#   --harden-ipc    exercise the production sway-IPC hardening: shepherdd
#                   unlinks the compositor's socket once it has connected, so
#                   nothing else can reach it (issue #144). This is shepherdd's
#                   default; `sway.conf` opts out because it is the development
#                   config, and this flag takes that opt-out back off. The
#                   session stays drivable through the alias shepherdd creates
#                   first, which this harness always asks for and always uses.
headless_start() {
    local size="$HEADLESS_SIZE_DEFAULT" mock_time="" renderer="pixman" do_build=1
    local config="" user="" harden=0 harden_peers=0
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --config)
                # Resolve against the invoker's CWD now, before we cd to the
                # repo root — shepherdd is exec'd with CWD=repo root, so it needs
                # an absolute path.
                config="$(realpath -m "$2")"; shift 2 ;;
            --user) user="$2"; shift 2 ;;
            --size) size="$2"; shift 2 ;;
            --time) mock_time="$2"; shift 2 ;;
            --gpu)  renderer="gles2"; shift ;;
            --no-build) do_build=0; shift ;;
            --harden-ipc) harden=1; shift ;;
            --harden-ipc-peers) harden_peers=1; shift ;;
            -h|--help) headless_usage; return 0 ;;
            *) die "Unknown option for 'dev headless': $1 (try: shepherd dev headless --help)" ;;
        esac
    done

    local repo_root; repo_root="$(get_repo_root)"
    verify_repo
    cd "$repo_root" || die "Failed to cd to $repo_root"

    require_command sway
    require_command grim

    # Resolve the target user (default: the invoker), and, under --user with no
    # explicit --config, default to that user's regular config path in their home
    # directory (what shepherdd's default_config_path() would pick).
    local user_mode=0 target_home=""
    if [[ -n "$user" ]]; then
        user_mode=1
        id "$user" >/dev/null 2>&1 || die "No such user: $user"
        require_command sudo
        target_home="$(getent passwd "$user" | cut -d: -f6)"
        [[ -n "$target_home" ]] || die "Could not resolve home directory for $user"
        if [[ -z "$config" ]]; then
            config="$target_home/.config/shepherd/config.toml"
        fi
    fi

    # The runtime dir has to be known before the sway config is written, because
    # the config carries the alias path shepherdd will create inside it.
    local rt
    if [[ "$user_mode" -eq 1 ]]; then
        # Dedicated, short-pathed, target-owned runtime dir (the target user may
        # have no /run/user/<uid> without a login session).
        rt="/run/shepherd-headless/$(id -u "$user")"
    else
        rt="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
        [[ -d "$rt" ]] || die "XDG_RUNTIME_DIR ($rt) does not exist; a headless session still needs a short-pathed runtime dir for its wayland socket"
    fi

    # Where shepherdd will hard-link sway's IPC socket. Every `swaymsg` this
    # harness runs goes through this path rather than the one sway chose, so the
    # hardened and unhardened sessions are driven identically — and `--harden-ipc`
    # changes only whether the original name survives.
    #
    # Named for this shell so two concurrent sessions in one runtime dir do not
    # collide, and inside $rt because a hard link cannot cross filesystems.
    local sway_alias="$rt/shepherd-dev-sway.$$.sock"

    # Default sway config boots ./config.example.toml (hard-coded in sway.conf).
    # The derived copy rewrites the shepherdd exec line: its `-c` token when a
    # config is chosen (--config, or the --user default), always the alias, and
    # the hardening opt-out sway.conf carries when --harden-ipc says to drop it.
    # Every other kiosk rule is preserved.
    mkdir -p "$HEADLESS_DIR_DEFAULT"
    local sway_config="$HEADLESS_DIR_DEFAULT/sway.headless.conf"
    cp "$repo_root/sway.conf" "$sway_config"
    if [[ -n "$config" ]]; then
        # Check existence as the user who will actually read it: under --user the
        # config lives in that user's home, which the invoker may not traverse.
        local config_exists=1
        if [[ "$user_mode" -eq 1 ]]; then
            sudo -u "$user" test -f "$config" || config_exists=0
        else
            [[ -f "$config" ]] || config_exists=0
        fi
        if [[ "$config_exists" -eq 0 ]]; then
            if [[ "$user_mode" -eq 1 ]]; then
                die "Config not found: $config — deploy it first (e.g. shepherd install config --user $user), or pass --config PATH"
            fi
            die "Config not found: $config"
        fi
        # Match `shepherdd -c <token>` regardless of the default path/spacing.
        sed -E -i "s#(target/debug/shepherdd -c )[^ ]+#\1$config#" "$sway_config"
        if ! grep -qF "shepherdd -c $config" "$sway_config"; then
            die "Failed to inject --config into a derived sway config (sway.conf's shepherdd exec line may have changed; expected 'target/debug/shepherdd -c <path>')"
        fi
        info "Booting config: $config${user:+ (as $user)}"
    fi

    sed -E -i "s#(target/debug/shepherdd -c [^ ]+)#\1 --sway-ipc-alias $sway_alias#" "$sway_config"
    if ! grep -qF -- "--sway-ipc-alias $sway_alias" "$sway_config"; then
        die "Failed to inject the sway-IPC flags into the derived sway config (expected 'target/debug/shepherdd -c <path>')"
    fi

    # shepherdd hardens by default; `sway.conf` opts out because it is the
    # development config. `--harden-ipc` therefore takes the opt-out back off
    # rather than adding a flag, so a dev session that does not ask for it keeps
    # a compositor `swaymsg` can reach.
    # Scoped to the exec line: sway.conf's comment names the flag too, so a
    # whole-file grep answers the wrong question in both directions.
    local exec_line
    exec_line="$(grep -E "^exec .*shepherdd -c [^ ]+" "$sway_config" || true)"
    if [[ "$harden" -eq 1 ]]; then
        sed -i "/^exec .*shepherdd -c /s# --no-harden-sway-ipc##g" "$sway_config"
        exec_line="$(grep -E "^exec .*shepherdd -c [^ ]+" "$sway_config" || true)"
        if [[ "$exec_line" == *--no-harden-sway-ipc* ]]; then
            die "Failed to strip --no-harden-sway-ipc from the derived sway config, so --harden-ipc would not have hardened anything"
        fi
        info "Hardening sway IPC: shepherdd will unlink the compositor socket"
    elif [[ "$exec_line" != *--no-harden-sway-ipc* ]]; then
        # Without the opt-out the session would harden itself and every later
        # `swaymsg` would fail, which reads as a broken harness rather than a
        # missing flag.
        die "sway.conf no longer passes --no-harden-sway-ipc on its shepherdd exec line, so a plain 'dev headless' would unlink the compositor socket (issue #144)"
    fi

    # The management socket's peer check stays off by default, for the reason
    # `sway.conf` gives: it accepts peers in shepherdd's own cgroup, and
    # everything a plain dev session starts shares the cgroup of the shell that
    # launched it, so arming it would only refuse clients run from another
    # terminal without separating anything.
    #
    # `--harden-ipc-peers` takes the opt-out back off, the same way
    # `--harden-ipc` does — and, because stripping the flag is not by itself
    # enough, `headless_scope_prefix` puts the session somewhere the check can
    # mean something. See the recipe in `crates/shepherd-ipc/README.md`.
    if [[ "$harden_peers" -eq 1 ]]; then
        sed -i "/^exec .*shepherdd -c /s# --no-restrict-ipc-peers##g" "$sway_config"
        exec_line="$(grep -E "^exec .*shepherdd -c [^ ]+" "$sway_config" || true)"
        if [[ "$exec_line" == *--no-restrict-ipc-peers* ]]; then
            die "Failed to strip --no-restrict-ipc-peers from the derived sway config, so --harden-ipc-peers would not have armed anything"
        fi
        info "Arming the management-socket peer check: only this session and root may drive the daemon"
    elif [[ "$exec_line" != *--no-restrict-ipc-peers* ]]; then
        die "sway.conf no longer passes --no-restrict-ipc-peers on its shepherdd exec line, so a dev session would refuse clients started from any other terminal (issue #144)"
    fi

    if [[ "$do_build" -eq 1 ]]; then
        info "Building shepherd binaries..."
        build_cargo false
    fi

    local log; log="$(headless_log)"
    : > "$log"  # invoker owns the log fd; sway (any user) writes into it

    # Shared headless env for sway. No parent compositor, no login session, no
    # GPU. WLR_SCENE_DISABLE_DIRECT_SCANOUT keeps fullscreen activities
    # compositing so screencopy always has a frame (as in scripts/lib/sway.sh).
    # GSK_RENDERER=cairo forces GTK4 onto its software renderer — the default GL
    # renderer needs EGL, absent on the pixman/no-GPU path.
    local -a sway_env=(
        WLR_BACKENDS=headless
        WLR_LIBINPUT_NO_DEVICES=1
        "WLR_RENDERER=$renderer"
        WLR_RENDERER_ALLOW_SOFTWARE=1
        WLR_SCENE_DISABLE_DIRECT_SCANOUT=1
        LIBGL_ALWAYS_SOFTWARE=1
        "GSK_RENDERER=${GSK_RENDERER:-cairo}"
        XDG_SESSION_TYPE=wayland
    )
    [[ -n "$mock_time" ]] && sway_env+=("SHEPHERD_MOCK_TIME=$mock_time")

    local pid swaysock wd
    if [[ "$user_mode" -eq 1 ]]; then
        # Fresh each start so socket discovery is unambiguous.
        maybe_sudo rm -rf "$rt"
        maybe_sudo mkdir -p "$rt"
        maybe_sudo chown "$user" "$rt"
        maybe_sudo chmod 700 "$rt"

        if [[ "$harden_peers" -eq 1 ]]; then
            # The scope would have to run as root and drop to `$user` through
            # the existing `sudo -u`, and that path uses `env -i`, which would
            # wipe the bus address the arming depends on. Refusing beats
            # handing back a session that quietly degraded.
            die "--harden-ipc-peers is not supported with --user yet; run it without --user"
        fi

        headless_precheck_user "$user" "$repo_root" "$sway_config" "$config"

        # Clear any prior session this user has for our config.
        maybe_sudo pkill -u "$user" -f "sway -c $sway_config" 2>/dev/null || true

        info "Starting headless Sway ($renderer renderer, $size) as $user..."
        # shepherdd/launcher/HUD get no SHEPHERD_SOCKET, so they agree on the
        # default $XDG_RUNTIME_DIR/shepherdd/shepherdd.sock under this runtime dir.
        setsid sudo -u "$user" env -i \
            HOME="$target_home" USER="$user" LOGNAME="$user" \
            "PATH=$repo_root/target/debug:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/snap/bin" \
            "XDG_RUNTIME_DIR=$rt" \
            "${sway_env[@]}" \
            sway -c "$sway_config" --unsupported-gpu \
            >"$log" 2>&1 &
        disown 2>/dev/null || true

        # Wait for sway itself. Under --harden-ipc the ambient socket is
        # short-lived, so accept either name — this stage only answers "did the
        # compositor start", which is the failure `dev headless` most often
        # needs to distinguish.
        if ! headless_wait_compositor "$rt" "$user" "$sway_alias" 100; then
            error "Headless Sway (as $user) did not create its IPC socket within 10s. Last log lines:"
            tail -n 20 "$log" >&2 || true
            maybe_sudo pkill -u "$user" -f "sway -c $sway_config" 2>/dev/null || true
            die "Failed to start headless session"
        fi
        # From the process, not the socket name: under --harden-ipc the
        # `sway-ipc.<uid>.<pid>.sock` name we used to parse may already be gone.
        pid="$(maybe_sudo pgrep -u "$user" -f "sway -c $sway_config" | head -1)"
        [[ -n "$pid" ]] || die "Headless Sway (as $user) answered but could not be found in the process table"
        wd="$(headless_socket_as "$rt" "$user" wayland)"; wd="${wd##*/}"
    else
        # Invoker-owned session: reuse the shared dev runtime env + media seeding,
        # and clear any prior instance first.
        sway_setup_env
        sway_ensure_dev_media_library
        headless_stop quiet || true
        sway_kill_existing
        # Aliases from a session that died without running `dev stop` point at
        # a socket that no longer exists; nothing can connect through them.
        rm -f "$rt"/shepherd-dev-sway.*.sock

        # Snapshot wayland-N sockets to identify the one sway creates (sway
        # ignores a preset WAYLAND_DISPLAY and picks the lowest free slot).
        local before; before="$(headless_wayland_sockets "$rt")"

        info "Starting headless Sway ($renderer renderer, $size)..."
        local -a scope=()
        if [[ "$harden_peers" -eq 1 ]]; then
            mapfile -t scope < <(headless_scope_prefix "$rt")
        fi
        setsid "${scope[@]}" env "${sway_env[@]}" \
            sway -c "$sway_config" --unsupported-gpu \
            >"$log" 2>&1 &
        pid=$!
        disown "$pid" 2>/dev/null || true

        if ! headless_wait_compositor "$rt" "" "$sway_alias" 100; then
            error "Headless Sway did not create its IPC socket within 10s. Last log lines:"
            tail -n 20 "$log" >&2 || true
            die "Failed to start headless session"
        fi
        local after
        after="$(headless_wayland_sockets "$rt")"
        wd="$(comm -13 <(printf '%s\n' "$before" | sort) <(printf '%s\n' "$after" | sort) | head -1)"
    fi

    export SHEPHERD_HEADLESS_USER="$user"
    export XDG_RUNTIME_DIR="$rt"

    # Second stage: the alias exists only once shepherdd has connected to the
    # compositor, so waiting for it distinguishes "sway did not start" (above)
    # from "shepherdd did not" — the two used to be one indistinguishable
    # timeout, and the second is by far the more common dev failure.
    if ! headless_wait_socket "$sway_alias" "$user" 300; then
        error "shepherdd did not connect to the compositor within 30s (no $sway_alias). Last log lines:"
        tail -n 20 "$log" >&2 || true
        headless_stop quiet || true
        die "Failed to start headless session"
    fi
    swaysock="$sway_alias"
    export SWAYSOCK="$swaysock"

    if ! headless_wait_ipc 100; then
        error "Headless Sway did not answer IPC within 10s. Last log lines:"
        tail -n 20 "$log" >&2 || true
        headless_stop quiet || true
        die "Failed to start headless session"
    fi

    # For the invoker path the wayland name comes from the before/after diff;
    # fall back to whatever socket exists if the diff was empty (e.g. a race).
    [[ -z "$wd" ]] && wd="$(headless_wayland_sockets "$rt" | tail -1)"
    export WAYLAND_DISPLAY="$wd"

    # Pin the virtual output to the requested resolution.
    headless_run swaymsg "output $HEADLESS_OUTPUT mode $size" >/dev/null 2>&1 || \
        warn "Could not set mode $size on $HEADLESS_OUTPUT (continuing at default)"

    # Persist the connection details for later dev subcommands.
    cat > "$(headless_session_env)" <<EOF
# Written by 'shepherd dev headless'. Source to reattach.
SHEPHERD_HEADLESS_PID=$pid
SHEPHERD_HEADLESS_USER=$user
XDG_RUNTIME_DIR=$rt
SWAYSOCK=$swaysock
WAYLAND_DISPLAY=$wd
SHEPHERD_HEADLESS_SIZE=$size
SHEPHERD_HEADLESS_OUTPUT=$HEADLESS_OUTPUT
EOF

    # Asking for the armed check and getting a degraded one is the failure this
    # flag exists to prevent, and it is invisible unless someone reads the log:
    # the session comes up and every client still connects, because they all
    # share shepherdd's cgroup either way. So confirm it, and hand back the
    # daemon's own reason when it did not arm.
    if [[ "$harden_peers" -eq 1 ]]; then
        if grep -aq "accepts only this session and root" "$log"; then
            success "Peer check armed: only shepherdd's own cgroup and root may drive the daemon"
        else
            local why
            why="$(grep -aoE "(delegated cgroup subtree|Activities will share shepherd's own cgroup)[^\"]*" "$log" | head -1)"
            error "--harden-ipc-peers did not arm the peer check. The daemon said:"
            printf '  %s\n' "${why:-<no reason logged; see $log>}" >&2
            die "Refusing to hand back a session that looks hardened and is not (issue #144)"
        fi
    fi

    success "Headless session up (pid $pid${user:+, user=$user}, WAYLAND_DISPLAY=$wd, SWAYSOCK=$swaysock)"

    if headless_wait_launcher 900 200; then
        success "Launcher surface is mapped and ready to screenshot."
    else
        warn "Launcher surface not detected within 90s — the stack is up but the"
        warn "GTK UI may not have painted. Check $log and 'shepherd dev tree'."
    fi

    cat >&2 <<EOF

Next:
  shepherd dev shot [out.png]   # screenshot the virtual output
  shepherd dev tree             # dump the window tree (app_id/focus/fullscreen)
  shepherd dev key Down         # inject a keystroke; also: dev type "text"
  shepherd dev click X Y        # move+click the virtual pointer
  shepherd dev stop             # tear the session down
EOF
}

# Screenshot the headless virtual output to a PNG (default: timestamped file
# under dev-runtime/headless/). Prints the path it wrote to stdout.
headless_shot() {
    headless_load_session
    require_command grim
    local out="${1:-}"
    if [[ -z "$out" ]]; then
        out="$HEADLESS_DIR_DEFAULT/shot-$(date +%Y%m%d-%H%M%S).png"
    fi
    # Defensive: swayidle may have blanked the output (DPMS off) after idle,
    # which screenshots as solid black. Force it back on before grabbing.
    headless_run swaymsg "output * dpms on" >/dev/null 2>&1 || true
    # grim writes the PNG to stdout (`-`), which the invoker's shell redirects to
    # $out. Under --user, grim runs as the session's user (its runtime dir is
    # 0700), but the *invoker* owns $out — so an agent can always read it back.
    headless_run grim -o "${SHEPHERD_HEADLESS_OUTPUT:-$HEADLESS_OUTPUT}" - > "$out" \
        || die "grim failed (is the session still up? try: shepherd dev tree)"
    success "Wrote $out"
    echo "$out"
}

# Dump the window tree (piped through jq to a focus/fullscreen summary if jq is
# present; raw JSON otherwise). Pass 'raw' to force full JSON.
headless_tree() {
    headless_load_session
    if [[ "${1:-}" != "raw" ]] && command_exists jq; then
        headless_run swaymsg -t get_tree | jq -r '
            [recurse(.nodes[]?, .floating_nodes[]?)
             | select(.app_id != null or (.window_properties?.class != null))
             | {app_id, class: .window_properties?.class, focused, fullscreen_mode, visible}]'
    else
        headless_run swaymsg -t get_tree
    fi
}

# Inject a keystroke (keysym, e.g. Return, Down, Escape) into the session.
headless_key() {
    headless_load_session
    require_command wtype
    [[ $# -ge 1 ]] || die "Usage: shepherd dev key <keysym> [<keysym>...]"
    local k
    for k in "$@"; do
        headless_run wtype -k "$k" || die "wtype failed for key '$k'"
    done
}

# Type literal text into the focused surface.
headless_type() {
    headless_load_session
    require_command wtype
    [[ $# -ge 1 ]] || die "Usage: shepherd dev type <text>"
    headless_run wtype "$*" || die "wtype failed"
}

# Move the virtual pointer to X Y and click (button in $3, default left=1).
# Uses sway's IPC seat cursor commands — no pointer daemon or root needed.
headless_click() {
    headless_load_session
    [[ $# -ge 2 ]] || die "Usage: shepherd dev click <x> <y> [button]"
    local x="$1" y="$2" btn="${3:-1}"
    local name; name="button${btn}"
    case "$btn" in 1) name=button1 ;; 2) name=button3 ;; 3) name=button2 ;; *) name="button${btn}" ;; esac
    headless_run swaymsg "seat - cursor set $x $y" >/dev/null \
        || die "cursor move failed"
    headless_run swaymsg "seat - cursor press $name" >/dev/null || true
    headless_run swaymsg "seat - cursor release $name" >/dev/null || true
}

# Tear the session down and clean up sockets.
headless_stop() {
    local quiet="${1:-}"
    if ! headless_load_session quiet; then
        [[ "$quiet" == "quiet" ]] && return 0
        warn "No live headless session to stop."
        return 0
    fi
    local u="${SHEPHERD_HEADLESS_USER:-}"
    info "Stopping headless session (pid $SHEPHERD_HEADLESS_PID${u:+, user=$u})..."
    headless_run swaymsg exit >/dev/null 2>&1 || true
    # Liveness via /proc (the process may be another user's under --user).
    for _ in $(seq 1 20); do
        [[ -d "/proc/$SHEPHERD_HEADLESS_PID" ]] || break
        sleep 0.1
    done
    if [[ -n "$u" && "$u" != "$(id -un)" ]]; then
        # Owned by another user — signal + reap via sudo, scoped to that user.
        maybe_sudo kill -KILL "$SHEPHERD_HEADLESS_PID" 2>/dev/null || true
        maybe_sudo pkill -u "$u" -x shepherdd 2>/dev/null || true
        maybe_sudo pkill -u "$u" -x shepherd-launcher 2>/dev/null || true
        maybe_sudo pkill -u "$u" -x shepherd-hud 2>/dev/null || true
        maybe_sudo pkill -u "$u" -x shepherd-media 2>/dev/null || true
        maybe_sudo pkill -u "$u" -x sway 2>/dev/null || true
        # Remove the dedicated runtime dir we created for this user.
        maybe_sudo rm -rf "$XDG_RUNTIME_DIR"
    else
        kill -KILL "$SHEPHERD_HEADLESS_PID" 2>/dev/null || true
        pkill -x shepherdd 2>/dev/null || true
        pkill -x shepherd-launcher 2>/dev/null || true
        pkill -x shepherd-hud 2>/dev/null || true
        pkill -x shepherd-media 2>/dev/null || true
        [[ -n "${SHEPHERD_SOCKET:-}" ]] && rm -f "$SHEPHERD_SOCKET"
        # The alias outlives sway: it is a second name for a socket whose
        # inode is now gone, so it would sit there dangling.
        [[ -n "${SWAYSOCK:-}" ]] && rm -f "$SWAYSOCK"
        shopt -s extglob
        sway_purge_stale_sockets
    fi
    rm -f "$(headless_session_env)"
    [[ "$quiet" == "quiet" ]] || success "Headless session stopped."
}

headless_usage() {
    cat <<EOF
Usage: shepherd dev headless [options]   # start a detached headless session
       shepherd dev shot [out.png]       # screenshot the virtual output
       shepherd dev tree [raw]           # dump the window tree (jq summary)
       shepherd dev key <keysym>...      # inject keystroke(s) (e.g. Down Return)
       shepherd dev type <text>          # type literal text
       shepherd dev click <x> <y> [btn]  # move + click the virtual pointer
       shepherd dev stop                 # tear the session down

Start options:
    --config PATH  shepherdd config to boot (default: ./config.example.toml,
                   or the target user's ~/.config/shepherd/config.toml under --user)
    --user NAME    Run the whole stack as NAME (via sudo) for a more realistic
                   session: that user's groups, HOME, and default config. The
                   repo must be readable/executable by NAME (a checkout under a
                   0700 \$HOME is not). shot/tree/key/... reattach automatically.
    --size WxH     Virtual output resolution (default $HEADLESS_SIZE_DEFAULT)
    --time "..."   SHEPHERD_MOCK_TIME for the session (e.g. "2025-12-25 21:00:00")
    --gpu          Use the GL renderer against a DRM node instead of pixman
    --no-build     Skip the cargo build; use existing target/debug binaries
    --harden-ipc   Exercise the production sway-IPC hardening (issue #144):
                   shepherdd unlinks the compositor's socket once it has
                   connected, so no other process can reach it. That is
                   shepherdd's default; sway.conf opts out because it is the
                   development config, and this flag takes the opt-out back off.
                   The session stays drivable through the alias shepherdd
                   creates first, which this harness always uses either way.
    --harden-ipc-peers
                   Exercise the production management-socket peer check (issue
                   #144): only shepherdd's own cgroup and root may drive the
                   daemon. Needs sudo. The check only means anything in a cgroup
                   an activity cannot join, and a stack started from a shell
                   sits in the user manager's delegated subtree, so the session
                   is run in a system-manager scope instead. Fails loudly if the
                   daemon degrades rather than arms, since a degraded session
                   looks identical from the outside.

The session runs with no login session, no parent compositor, and no GPU, so an
agent can drive it over SSH. Connection details live in
dev-runtime/headless/session.env.
EOF
}

# Dispatcher used by `shepherd dev <subcmd>`.
headless_dev_dispatch() {
    local sub="$1"; shift || true
    case "$sub" in
        headless) headless_start "$@" ;;
        shot)     headless_shot  "$@" ;;
        tree)     headless_tree  "$@" ;;
        key)      headless_key   "$@" ;;
        type)     headless_type  "$@" ;;
        click)    headless_click "$@" ;;
        stop)     headless_stop  "$@" ;;
        *) return 2 ;;  # not ours; let the caller handle it
    esac
}
