#!/usr/bin/env bash
# Distro packaging for shepherd-launcher.
#
# `shepherd package deb` produces a Debian package for the host architecture
# (amd64, arm64, …). It does NOT
# re-encode the install layout: it stages the exact tree that a from-source
# install would create by driving scripts/lib/install.sh with DESTDIR set
# (which makes those functions skip host mutation — groupadd/usermod/udev/
# polkit reload), then wraps the staged tree with dpkg-deb. The host-mutating
# steps move into the package's postinst instead.
#
# See docs/ai/history/2026-07-04 003 binary-releases.md for the design.

# Get the directory containing this script
PACKAGE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$PACKAGE_LIB_DIR/common.sh"
# shellcheck source=build.sh
source "$PACKAGE_LIB_DIR/build.sh"
# shellcheck source=install.sh
source "$PACKAGE_LIB_DIR/install.sh"
# shellcheck source=version.sh
source "$PACKAGE_LIB_DIR/version.sh"

# System package conventions: a distro package installs under /usr, not the
# /usr/local default used by a manual `shepherd install`.
PACKAGE_PREFIX="/usr"
PACKAGE_NAME="shepherd-launcher"
PACKAGE_MAINTAINER="Albert Armea <shepherd-launcher-patch@albertarmea.com>"
# Where the package drops the example config + media library. A from-source
# install copies these into the user's config dir from the repo (install_config);
# a .deb user has no repo, so they live here for the admin to copy.
PACKAGE_EXAMPLE_DIR="$PACKAGE_PREFIX/share/shepherd"

# Build the .deb for the host architecture.
package_deb() {
    local out_dir="dist/pkg"
    local do_build="true"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --out)
                out_dir="$2"
                shift 2
                ;;
            --no-build)
                do_build="false"
                shift
                ;;
            -h|--help)
                package_deb_usage
                return 0
                ;;
            *)
                die "Unknown package deb option: $1 (try: shepherd package help)"
                ;;
        esac
    done

    verify_repo

    local repo_root
    repo_root="$(get_repo_root)"
    local version
    version="$(version_read)"

    # Build the release binaries as the invoking user — cargo/npm need no
    # privileges. Do this *before* any fakeroot re-exec so the heavy build
    # runs unprivileged and only the staging + dpkg-deb run faked-root.
    if [[ "$do_build" == "true" ]]; then
        build_cargo "true"
    fi

    # Staging writes root-owned files (install.sh's firewall/udev steps use
    # `install -o root -g root`), so it needs EUID 0. When unprivileged, and
    # not already under fakeroot, re-exec the staging half under fakeroot so
    # local builds don't need sudo. CI runs as root and skips this.
    if ! is_root; then
        require_command fakeroot
        info "Re-executing staging under fakeroot..."
        exec fakeroot "$repo_root/scripts/shepherd" package deb \
            --out "$out_dir" --no-build
    fi

    require_command dpkg-deb dpkg

    # Absolute output dir so it survives the DESTDIR-relative install calls.
    case "$out_dir" in
        /*) : ;;
        *) out_dir="$repo_root/$out_dir" ;;
    esac
    ensure_dir "$out_dir" 0755

    local stage
    stage="$(mktemp -d)"
    # shellcheck disable=SC2064  # expand $stage now, not at trap time
    trap "rm -rf '$stage'" RETURN
    # mktemp -d yields 0700; the archive's root entry (and any dirs the install
    # steps create implicitly under the default umask) should be the standard
    # 0755. Normalize the root and stage everything under a 022 umask.
    chmod 0755 "$stage"
    umask 022

    info "Staging install tree into $stage..."
    # DESTDIR makes the install steps relocate under $stage and skip host
    # mutation. install_system (defined in install.sh) is the shared list of
    # system-wide components; the package intentionally omits the per-user
    # config/groups steps that install_all adds. Empty firewall user: no
    # per-user group work happens under DESTDIR anyway.
    export DESTDIR="$stage"
    install_system "$PACKAGE_PREFIX" ""
    unset DESTDIR

    # Ship the example config + media library system-wide so a .deb user (who
    # has no repo checkout) has something to copy into the kiosk user's config
    # dir. The postinst prints the exact copy/usermod commands; see also
    # docs/INSTALL.md.
    local ex_stage="$stage$PACKAGE_EXAMPLE_DIR"
    ensure_dir "$ex_stage" 0755
    install -m 0644 "$repo_root/config.example.toml" "$ex_stage/config.example.toml"
    install -m 0644 "$repo_root/movies-library.example.toml" \
        "$ex_stage/movies-library.example.toml"
    # VERSION so `shepherd-admin --version` works off get_data_dir when packaged.
    install -m 0644 "$repo_root/VERSION" "$ex_stage/VERSION"

    _package_stage_admin_cli "$stage" "$repo_root"

    # Package for the host architecture. cargo built native binaries above, so
    # the Debian arch (amd64, arm64, …) must match; `dpkg --print-architecture`
    # yields the Debian name for the running host.
    local arch
    arch="$(dpkg --print-architecture)"

    _package_write_control "$stage" "$version" "$repo_root" "$arch"

    local deb="$out_dir/${PACKAGE_NAME}_${version}_${arch}.deb"
    info "Building $deb..."
    # --root-owner-group forces root:root ownership in the archive regardless
    # of who (or what fakeroot) staged the files.
    dpkg-deb --root-owner-group --build "$stage" "$deb"

    success "Built $deb"
    echo "$deb"
}

# Stage the shepherd-admin CLI + its shared libs so an installed system has the
# post-install admin tasks (setup-user, yt-dlp, apps, harden, bluetooth) without
# a source tree. Libs go under /usr/lib/shepherd/lib; the entrypoint is
# symlinked onto PATH. shepherd-admin resolves its libs from a sibling lib/ dir
# and, finding no repo Cargo.toml above it, uses /usr/share/shepherd for data.
_package_stage_admin_cli() {
    local stage="$1" repo_root="$2"
    local libdir="$stage/usr/lib/shepherd"

    ensure_dir "$libdir/lib" 0755
    # Ship the whole lib dir so a future admin-lib dependency can't be left out;
    # the build/package/dev-only libs are inert without a source tree.
    install -m 0644 "$repo_root"/scripts/lib/*.sh "$libdir/lib/"
    install -m 0755 "$repo_root/scripts/shepherd-admin" "$libdir/shepherd-admin"

    ensure_dir "$stage/usr/bin" 0755
    ln -sf /usr/lib/shepherd/shepherd-admin "$stage/usr/bin/shepherd-admin"
}

# Write the DEBIAN control directory (control, conffiles, maintainer scripts)
# into the staged tree.
_package_write_control() {
    local stage="$1" version="$2" repo_root="$3" arch="$4"
    local debian="$stage/DEBIAN"
    ensure_dir "$debian" 0755

    # Runtime Depends stay single-sourced: derived from run.pkgs, the same list
    # `shepherd deps install run` uses. Strip comments/blanks, join with ", ".
    local depends
    depends="$(grep -vE '^[[:space:]]*(#|$)' "$repo_root/scripts/deps/run.pkgs" \
        | paste -sd, - | sed 's/,/, /g')"

    # Installed-Size in KiB (Debian policy: excludes the control area).
    local size
    size="$(du -ks "$stage" | cut -f1)"

    cat > "$debian/control" <<EOF
Package: $PACKAGE_NAME
Version: $version
Architecture: $arch
Maintainer: $PACKAGE_MAINTAINER
Section: admin
Priority: optional
Homepage: https://git.armeafamily.com/albert/shepherd-launcher
Depends: $depends
Installed-Size: $size
Description: Parent-guided kiosk desktop environment for Wayland
 shepherd-launcher provides supervised, time-scoped access to the
 applications and content a parent defines, with the ease-of-use of a game
 console. Policy lives outside the applications being run; sessions end
 predictably and enforceably.
 .
 This package installs the binaries, the privileged firewall helper and its
 polkit assets, the udev rule for the input-compat sidecars, the Sway kiosk
 session, and the display-manager session entry. After installing, deploy a
 user config and group memberships with:
 shepherd install config --user USER && shepherd install groups --user USER
EOF

    # conffiles: admin-editable files under /etc, so dpkg preserves local edits
    # across upgrades. Paths are built from install.sh's own location constants
    # (this lib sources install.sh) so they can't drift from where the install
    # steps actually placed the files. The .conf.d drop-in dir is left unmanaged
    # on purpose.
    cat > "$debian/conffiles" <<EOF
$SWAY_CONFIG_DIR/$SHEPHERD_SWAY_CONFIG
$UDEV_RULES_DIR/$UINPUT_RULES_NAME
$POLKIT_RULES_DIR/$FIREWALL_RULES_NAME
EOF

    # postinst: the host-mutating steps install.sh's install_firewall /
    # install_udev run on a real install (guarded out under DESTDIR), re-
    # expressed as POSIX sh because the maintainer script runs on the target
    # without the repo. Keep in sync with those functions. The firewall group
    # name is injected from install.sh's FIREWALL_GROUP. Per-user setup can't be
    # done at package time (unknown user), so it points at the packaged
    # shepherd-admin CLI, which shares its implementation with `shepherd`.
    {
        printf '#!/bin/sh\nset -e\n'
        printf 'group=%s\n' "$FIREWALL_GROUP"
        cat <<'EOF'
if [ "$1" = "configure" ]; then
    if ! getent group "$group" >/dev/null 2>&1; then
        groupadd --system "$group" || true
    fi
    if command -v udevadm >/dev/null 2>&1; then
        udevadm control --reload-rules || true
        udevadm trigger /dev/uinput || true
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl reload polkit 2>/dev/null \
            || systemctl restart polkit 2>/dev/null \
            || true
    fi
    cat <<'EOM'
shepherd-launcher installed. Finish setting up a kiosk user (replace USER):

  shepherd-admin setup-user USER     # deploy config + add group memberships
  shepherd-admin yt-dlp install      # only for YouTube media libraries

Then have USER log out and back in and pick the "Shepherd Kiosk" session.
Other admin tasks: shepherd-admin apps install steam|chrome, harden, bluetooth.
EOM
fi
exit 0
EOF
    } > "$debian/postinst"

    # postrm: reload udev/polkit after our rules leave. The shepherd-firewall
    # system group is intentionally left in place (leftover files may still
    # reference it, and it is harmless).
    cat > "$debian/postrm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = "remove" ] || [ "$1" = "purge" ]; then
    if command -v udevadm >/dev/null 2>&1; then
        udevadm control --reload-rules || true
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl reload polkit 2>/dev/null || true
    fi
fi
exit 0
EOF

    chmod 0755 "$debian/postinst" "$debian/postrm"
}

package_deb_usage() {
    cat <<EOF
Usage: shepherd package deb [OPTIONS]

Builds a Debian package for the host architecture by staging the install tree
(via install.sh with DESTDIR) and wrapping it with dpkg-deb.

Options:
    --out DIR      Output directory for the .deb (default: dist/pkg)
    --no-build     Skip the release build; reuse existing target/release
    -h, --help     Show this help

Notes:
    Requires root or fakeroot (staging writes root-owned files). When run
    unprivileged, re-execs the staging step under fakeroot automatically.

Examples:
    shepherd package deb
    shepherd package deb --out /tmp/out
EOF
}

# Dispatch for `shepherd package <deb>`.
package_main() {
    local subcmd="${1:-}"
    shift || true
    case "$subcmd" in
        deb)
            package_deb "$@"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd package <command> [OPTIONS]

Commands:
    deb    Build the Debian package for the host architecture

Run 'shepherd package deb --help' for options.
EOF
            ;;
        *)
            die "Unknown package command: $subcmd (try: shepherd package help)"
            ;;
    esac
}
