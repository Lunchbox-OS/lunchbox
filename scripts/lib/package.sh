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
# Single-sourced from install.sh, which this lib sources.
PACKAGE_NAME="$DISTRO_PACKAGE_NAME"
PACKAGE_MAINTAINER="Albert Armea <shepherd-launcher-patch@albertarmea.com>"
# Where the package drops the example config + media library. A from-source
# install copies these into the user's config dir from the repo (install_config);
# a .deb user has no repo, so they live here for the admin to copy. The path is
# single-sourced from install.sh, which this lib sources: the bluetoothd
# drop-in template is staged under the same root by install_bluetooth_dropin,
# and shepherd-admin reads it as SHEPHERD_DATA_DIR.
PACKAGE_EXAMPLE_DIR="$PACKAGED_DATA_DIR"

# The last released version that declared conffiles (issue #177 dropped them
# all). It is the `prior-version` argument the .deb's maintainer scripts hand
# `dpkg-maintscript-helper rm_conffile`, which is how the two rules it moved
# out of /etc get cleared from the admin directories they used to live in.
# Upgrading from anything newer finds nothing to do, so this only ever needs
# changing if a conffile is reintroduced -- read the comment above the control
# file's missing `conffiles` before doing that.
PACKAGE_LAST_CONFFILE_VERSION="0.4.1"

# Build the .deb for the host architecture.
package_deb() {
    local out_dir="dist/pkg"
    local do_build="true"
    local allow_missing_dpc="false"

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
            --allow-missing-dpc)
                allow_missing_dpc="true"
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

    # A .deb without the DPC apk silently loses `lock_mode = "locktask"` on every
    # device it installs — and the failure only shows up much later, when an
    # operator runs `apps install android` and it can't find the apk. Fail here
    # instead, before the expensive build. Checked *before* the fakeroot re-exec
    # so a local build hears about it immediately rather than after a full
    # release compile.
    if [[ ! -f "$repo_root/dpc-waydroid/$DPC_APK_NAME" ]]; then
        if [[ "$allow_missing_dpc" != "true" ]]; then
            warn "DPC apk not built: $repo_root/dpc-waydroid/$DPC_APK_NAME"
            info "Build it first (it is signed with a persistent key, so packaging"
            info "deliberately won't build it for you):"
            info "  (cd dpc-waydroid && ANDROID_SDK_ROOT=/opt/android-sdk ./build.sh)"
            info "Or, for a package that intentionally ships without Lock Task support:"
            info "  shepherd package deb --allow-missing-dpc"
            die "Refusing to build a .deb without the DPC apk."
        fi
        warn "Building without the DPC apk (--allow-missing-dpc); this package cannot offer Android's lock_mode = \"locktask\"."
    fi

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
        # Forward --allow-missing-dpc: the re-exec runs the same check again, so
        # dropping it here would turn an explicitly-allowed build into a failure.
        # (an `[[ … ]] && assign` would abort the script under `set -e` when the
        # test is false — see waydroid.sh:install_dpc for the same trap)
        local dpc_arg=()
        if [[ "$allow_missing_dpc" == "true" ]]; then
            dpc_arg=(--allow-missing-dpc)
        fi
        exec fakeroot "$repo_root/scripts/shepherd" package deb \
            --out "$out_dir" --no-build "${dpc_arg[@]}"
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

    # The prebuilt DPC apk is staged by install_system above (install_dpc_apk),
    # so the .deb and a from-source install place it identically. It has to be
    # built first — the release .deb job runs dpc-waydroid/build.sh with the
    # persistent signing key; a dev `package deb` without it just warns, and that
    # .deb can't offer the Lock Task backend.

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
    #
    # -Zxz forces xz for BOTH the control.tar and data.tar members. dpkg-deb
    # >= 1.23 (Ubuntu 26.04) defaults to zstd, but Forgejo's Debian package
    # registry (Gitea-compat 1.22) can't decompress a zstd control.tar and
    # answers the apt-registry upload with HTTP 500. xz is understood by every
    # apt tool and every Forgejo/Gitea Debian parser, so we pin it.
    dpkg-deb -Zxz --root-owner-group --build "$stage" "$deb"

    success "Built $deb"
    echo "$deb"
}

# Generate an F-Droid repository from built APKs.
#
# This is a *validation and local-testing* tool, not a publish step: the real
# repository is generated on the server that hosts git.armeafamily.com, by a
# service that downloads each release's APK assets and this repo's
# dist/fdroid/metadata/ at the release tag. See dist/fdroid/README.md.
#
# Running it in release.yml is what keeps that server-side generation from being
# the first thing to notice a broken metadata file or a wrongly-signed APK.
package_fdroid() {
    local out_dir="dist/fdroid-repo"
    local apk_dir="dist/pkg"
    local debug_keys="false"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --out)
                out_dir="$2"
                shift 2
                ;;
            --apks)
                apk_dir="$2"
                shift 2
                ;;
            --debug-keys)
                debug_keys="true"
                shift
                ;;
            -h|--help)
                package_fdroid_usage
                return 0
                ;;
            *)
                die "Unknown package fdroid option: $1 (try: shepherd package help)"
                ;;
        esac
    done

    verify_repo

    local repo_root
    repo_root="$(get_repo_root)"

    # keytool (JDK) mints the throwaway index-signing key; apksigner reports the
    # certificate the APKs actually carry, for the error message when the pin
    # rejects one. Both come with `shepherd deps install android`.
    require_command fdroid keytool
    if ! command_exists apksigner; then
        # Not fatal — only used to explain a rejection. The SDK ships one too.
        warn "apksigner not on PATH; signing-key mismatches will be less legible"
    fi

    case "$out_dir" in /*) : ;; *) out_dir="$repo_root/$out_dir" ;; esac
    case "$apk_dir" in /*) : ;; *) apk_dir="$repo_root/$apk_dir" ;; esac

    local apks=()
    local apk
    for apk in "$apk_dir"/*.apk; do
        [[ -f "$apk" ]] && apks+=("$apk")
    done
    if [[ ${#apks[@]} -eq 0 ]]; then
        die "No .apk files in $apk_dir (build them first, or pass --apks DIR)"
    fi

    info "Generating an F-Droid repo from ${#apks[@]} APK(s) in $apk_dir"

    rm -rf "$out_dir"
    ensure_dir "$out_dir/repo" 0755
    ensure_dir "$out_dir/metadata" 0755
    cp "${apks[@]}" "$out_dir/repo/"

    # Metadata is copied rather than symlinked so --debug-keys can rewrite it.
    local meta
    for meta in "$repo_root"/dist/fdroid/metadata/*.yml; do
        if [[ "$debug_keys" == "true" ]]; then
            # Drop AllowedAPKSigningKeys (the key and its indented list items)
            # so debug-signed local builds aren't rejected by the pin.
            awk '
                /^AllowedAPKSigningKeys:/ { skip = 1; next }
                skip && /^[[:space:]]/    { next }
                                          { skip = 0; print }
            ' "$meta" > "$out_dir/metadata/$(basename "$meta")"
        else
            cp "$meta" "$out_dir/metadata/"
        fi
    done
    [[ "$debug_keys" == "true" ]] && warn "--debug-keys: signing-key pin NOT enforced"

    # A throwaway index-signing key, regenerated every run. The real repository
    # is signed by a permanent key that lives on the server (its fingerprint is
    # baked into the URL every device is configured with) — nothing here should
    # ever be able to sign something a device would trust.
    local pass="fdroid-local-validation"
    keytool -genkeypair -noprompt \
        -keystore "$out_dir/throwaway.jks" -alias fdroid-index \
        -keyalg RSA -keysize 2048 -validity 30 \
        -storepass "$pass" -keypass "$pass" \
        -dname "CN=shepherd local validation, O=none, C=US" >/dev/null 2>&1 \
        || die "keytool failed to create the throwaway signing key"

    cat > "$out_dir/config.yml" <<EOF
# Generated by \`shepherd package fdroid\` — local validation only.
repo_url: https://localhost/fdroid/repo
repo_name: Shepherd (local validation)
repo_description: Throwaway repo built to validate dist/fdroid/metadata.
archive_older: 0
keystore: throwaway.jks
repo_keyalias: fdroid-index
keystorepass: $pass
keypass: $pass
EOF
    chmod 0600 "$out_dir/config.yml"   # fdroid warns about anything looser

    # fdroidserver signs the index jar with jarsigner. On Debian/Ubuntu it
    # *unconditionally* prefers /usr/lib/jvm/default-java when that directory
    # exists (fdroidserver/common.py, "always prefer the built-in"), ignoring
    # both JAVA_HOME and any config key — so a box where default-java points at
    # a JRE fails deep inside the run with an opaque OSError. Catch it here.
    local default_java="/usr/lib/jvm/default-java"
    if [[ -d "$default_java" && ! -x "$default_java/bin/jarsigner" ]]; then
        die "$default_java is a JRE (no jarsigner), and fdroidserver prefers it over every other JDK.
Install a JDK as the default: sudo apt install --no-install-recommends default-jdk-headless"
    fi

    # --delete-unknown removes anything in repo/ that didn't make it into the
    # index — including an APK rejected by AllowedAPKSigningKeys, which is what
    # the assertion below detects.
    info "Running fdroid update..."
    ( cd "$out_dir" && fdroid update --delete-unknown --pretty ) \
        || die "fdroid update failed — see the output above"

    # fdroid update reports a rejected APK as a warning and carries on with a
    # smaller repo, so a successful exit is not evidence that anything was
    # published. Assert every input APK reached the index.
    local index="$out_dir/repo/index-v2.json"
    [[ -f "$index" ]] || die "fdroid update produced no $index"

    local missing=()
    local name
    for apk in "${apks[@]}"; do
        name="$(basename "$apk")"
        grep -qF "$name" "$index" || missing+=("$name")
    done

    if [[ ${#missing[@]} -gt 0 ]]; then
        error "These APKs were built but did NOT reach the index:"
        for name in "${missing[@]}"; do
            error "  $name"
        done
        error "The usual cause is AllowedAPKSigningKeys in dist/fdroid/metadata/"
        error "not matching the key the APK is signed with. Compare:"
        for name in "${missing[@]}"; do
            if command_exists apksigner; then
                apksigner verify --print-certs "$apk_dir/$name" 2>/dev/null \
                    | grep -i 'SHA-256 digest' >&2 || true
            fi
        done
        die "F-Droid repo validation failed"
    fi

    success "Validated ${#apks[@]} APK(s) into $out_dir/repo"
    echo "$out_dir/repo"
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

# Emit the `dpkg-maintscript-helper rm_conffile` calls that retire the rules
# issue #177 moved out of the admin directories.
#
# dpkg does not remove a conffile just because a new version stopped shipping
# it: it keeps the file on disk and remembers it as obsolete. udev and polkit
# both read /etc *after* their vendor directory and let the /etc copy win, so a
# leftover would go on overriding the file that replaced it -- on exactly the
# devices that upgraded rather than installed fresh. The helper is the supported
# way to retire one; it deletes an unmodified file and preserves an edited one
# as `.dpkg-bak`.
#
# Emitted into all three maintainer scripts because the helper has to see the
# install, the configure and the abort to do its job.
#
# The session watchdog's rule (#172) is in the list even though no *release*
# ever declared it a conffile: it was added to the list after 0.4.1 was cut, so
# only a device tracking `main` has dpkg holding a record of it, and that record
# carries 0.4.1's version number like the other two. Where it was never
# registered the helper finds nothing and does nothing, which costs one no-op
# and saves an unreleased leftover from outranking the rule that replaced it.
#
# The bluetoothd drop-in is deliberately not in this list. Its path did not
# move -- the postinst writes it now -- so retiring it would rename every
# device's copy to `.dpkg-bak` a moment before the postinst wrote a fresh one
# over the top. dpkg keeps an obsolete conffile record for it instead, which
# costs one stale line in `dpkg-query -W -f='${Conffiles}'` and buys the file
# being cleaned up on purge.
_package_retired_conffiles() {
    printf 'for cf in %s %s %s; do\n' \
        "$UDEV_LEGACY_RULES_DIR/$UINPUT_RULES_NAME" \
        "$POLKIT_LEGACY_RULES_DIR/$FIREWALL_RULES_NAME" \
        "$POLKIT_LEGACY_RULES_DIR/$SESSION_GUARD_RULES_NAME"
    # shellcheck disable=SC2016  # $cf and "$@" are the *emitted* script's
    printf '    dpkg-maintscript-helper rm_conffile "$cf" %s %s -- "$@"\n' \
        "$PACKAGE_LAST_CONFFILE_VERSION" "$PACKAGE_NAME"
    printf 'done\n'
}

# Write the DEBIAN control directory (control, maintainer scripts) into the
# staged tree.
_package_write_control() {
    local stage="$1" version="$2" repo_root="$3" arch="$4"
    local debian="$stage/DEBIAN"
    ensure_dir "$debian" 0755

    # Runtime Depends stay single-sourced: derived from run.pkgs, the same list
    # `shepherd deps install run` uses. Strip comments/blanks, join with ", ".
    local depends
    depends="$(grep -vE '^[[:space:]]*(#|$)' "$repo_root/scripts/deps/run.pkgs" \
        | paste -sd, - | sed 's/,/, /g')"

    # Optional tooling for `shepherd-admin apps install companion|media`, which
    # sideloads an Android app onto a phone or TV stick: curl fetches the release
    # APK, adb installs it. Suggests rather than Depends because that command is
    # a convenience nobody needs to run a kiosk — a device that never has a
    # phone plugged into it should not carry the Android platform tools. Both are
    # checked at call time, with the apt line to fix it (see admin.sh's
    # find_adb / android_download_apk).
    local suggests="adb, curl"

    # Pre-Depends on dpkg is what Debian policy asks of a package whose
    # maintainer scripts call `dpkg-maintscript-helper` -- the scripts run
    # before an ordinary Depends is guaranteed satisfied. 1.15.7.2 is the
    # release that introduced the helper; every dpkg shepherd runs on is far
    # newer, so this is a formality rather than a real constraint.
    #
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
Pre-Depends: dpkg (>= 1.15.7.2)
Suggests: $suggests
Installed-Size: $size
Description: Parent-guided kiosk desktop environment for Wayland
 shepherd-launcher provides supervised, time-scoped access to the
 applications and content a parent defines, with the ease-of-use of a game
 console. Policy lives outside the applications being run; sessions end
 predictably and enforceably.
 .
 This package installs the binaries, the privileged firewall and Waydroid
 helpers and their polkit assets, the udev rule for the input-compat sidecars,
 the Sway kiosk session, and the display-manager session entry. The Android
 (Waydroid) backend ships dormant; enable it with
 \`shepherd-admin apps install android\`. After installing, deploy a user config
 and group memberships with:
 shepherd install config --user USER && shepherd install groups --user USER
EOF

    # There is deliberately no conffiles file (issue #177).
    #
    # `conffiles` means "the admin owns this; keep their edits and ask before
    # replacing them". Nothing this package ships is that. Everything under
    # /etc here is shepherd's, generated or verified by the install steps, and
    # each of the files that used to be listed had a reason not to be:
    #
    #   /etc/sway/shepherd.conf
    #       Generated by install_sway_config, which strips
    #       --no-harden-sway-ipc, --no-restrict-ipc-peers, --trust-environment
    #       and --no-state-custodian and rewrites `swaymsg exit` (#144, #157,
    #       #172), then `die`s if any of that failed to apply. Those checks run
    #       when the file is written and nowhere else, so an admin's preserved
    #       copy carried old, unchecked content across upgrades forever. Site
    #       config belongs in /etc/sway/shepherd.conf.d/, which is left
    #       unmanaged on purpose and is what docs/INSTALL.md has always told
    #       people to use.
    #
    #   the udev rule and the two polkit rules
    #       Vendor files that were being shipped into admin directories. They
    #       now go to /usr/lib/udev/rules.d and /usr/share/polkit-1/rules.d --
    #       where the polkit *action* already went -- so dpkg owns them
    #       outright and an admin overrides one the way each subsystem
    #       intends, with a same-named file in /etc that is read first. The
    #       session watchdog's rule (#172) joined the list after 0.4.1 and is
    #       treated the same way: it grants shepherd's own uid shepherd's own
    #       action, which is not a local decision either.
    #
    #   the bluetoothd drop-in
    #       Not shipped at all any more: the postinst renders it from the
    #       template staged at $BLUETOOTH_DROPIN_TEMPLATE_DIR. It names the
    #       *target's* bluetoothd, which a build host cannot know, so the old
    #       arrangement shipped it as a conffile and then `sed`ed it in the
    #       postinst -- leaving dpkg holding a checksum of a file that had
    #       already changed. Every later upgrade then saw a locally-modified
    #       conffile on every device and either prompted or kept the stale
    #       copy, without anyone having edited anything.
    #
    # /etc/systemd/system/shepherd-stated@.{service,socket} were never in the
    # list and stay out of it for the same reason: they are shepherd's units,
    # `crates/shepherd-state-proto/tests/units_match_the_constants.rs` pins
    # their contents against the Rust constants, and a locally-edited copy
    # surviving an upgrade would break the custodian quietly.
    #
    # Consequences worth knowing: dpkg replaces all of these on upgrade without
    # asking, and `apt install --reinstall` restores any that went missing
    # without needing --force-confmiss (see docs/INSTALL.md).

    # preinst: nothing to prepare but the retired conffiles, which the helper
    # has to see before dpkg unpacks over them.
    {
        printf '#!/bin/sh\nset -e\n'
        _package_retired_conffiles
        printf 'exit 0\n'
    } > "$debian/preinst"

    # postinst: the host-mutating steps install.sh's install_firewall /
    # install_udev run on a real install (guarded out under DESTDIR), re-
    # expressed as POSIX sh because the maintainer script runs on the target
    # without the repo. Keep in sync with those functions. The firewall group
    # name is injected from install.sh's FIREWALL_GROUP. Per-user setup can't be
    # done at package time (unknown user), so it points at the packaged
    # shepherd-admin CLI, which shares its implementation with `shepherd`.
    {
        printf '#!/bin/sh\nset -e\n'
        _package_retired_conffiles
        printf 'group=%s\n' "$FIREWALL_GROUP"
        printf 'stated_user=%s\n' "$STATED_USER"
        printf 'dropin_dir=%s\n' "$BLUETOOTH_DROPIN_DIR"
        printf 'dropin=%s\n' "$BLUETOOTH_DROPIN_DIR/$BLUETOOTH_DROPIN_NAME"
        printf 'dropin_template=%s\n' \
            "$BLUETOOTH_DROPIN_TEMPLATE_DIR/$BLUETOOTH_DROPIN_NAME"
        printf 'bluetoothd_default=%s\n' "$BLUETOOTHD_DEFAULT_PATH"
        cat <<'EOF'
if [ "$1" = "configure" ]; then
    if ! getent group "$group" >/dev/null 2>&1; then
        groupadd --system "$group" || true
    fi
    # The uid that owns shepherd's policy and state, so activities -- which run
    # as the kiosk user -- cannot reach them (issue #157). Mirrors install.sh's
    # install_state. No home, no shell: it exists to own files and answer one
    # socket. The per-user socket is enabled by `shepherd-admin setup-user`,
    # since the kiosk user is not known at package time.
    if ! getent passwd "$stated_user" >/dev/null 2>&1; then
        useradd --system --no-create-home --home-dir /nonexistent \
            --shell /usr/sbin/nologin "$stated_user" || true
    fi
    if command -v udevadm >/dev/null 2>&1; then
        udevadm control --reload-rules || true
        udevadm trigger /dev/uinput || true
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl reload polkit 2>/dev/null \
            || systemctl restart polkit 2>/dev/null \
            || true
        # Pick up shepherd-stated@.socket / @.service.
        systemctl daemon-reload || true
    fi
    # Bluetooth drop-in: rendered here from the staged template, not shipped
    # (issue #177). ExecStart has to name the bluetoothd of the machine the
    # file ends up on, and the build host is not that machine. Shipping a
    # drop-in and `sed`ing it in place left dpkg holding a checksum of a file
    # that had already changed, so every later upgrade saw a locally-modified
    # conffile on every device. Rendering it here leaves dpkg nothing to
    # checksum. Mirrors install.sh's install_bluetooth_dropin (and its
    # _bluetoothd_exec_path); keep the two in sync.
    if command -v systemctl >/dev/null 2>&1; then
        if systemctl cat bluetooth.service >/dev/null 2>&1 \
            && [ -f "$dropin_template" ]; then
            # The unit's own ExecStart precedes any drop-in's in `systemctl
            # cat` output, so the first non-empty one is bluetoothd rather
            # than the line we wrote last time.
            bluetoothd=$(systemctl cat bluetooth.service 2>/dev/null \
                | sed -n 's/^ExecStart=[-@:+!]*\([^ ]\{1,\}\).*/\1/p' \
                | head -n 1)
            [ -n "$bluetoothd" ] || bluetoothd=$bluetoothd_default
            # Non-fatal: shepherd degrades gracefully without the drop-in (it
            # says so in the log and falls back), so a write that fails here
            # should not leave the package half-configured.
            if ! { mkdir -p "$dropin_dir" \
                && sed "s|@BLUETOOTHD@|$bluetoothd|" "$dropin_template" \
                    > "$dropin.new" \
                && chmod 0644 "$dropin.new" \
                && mv -f "$dropin.new" "$dropin"; }; then
                rm -f "$dropin.new"
                echo "warning: could not write $dropin; the Bluetooth bearer pin will not apply" >&2
            fi
        else
            # No bluetooth.service to extend: leave nothing behind aimed at a
            # daemon that is not on this machine.
            rm -f "$dropin"
            rmdir "$dropin_dir" 2>/dev/null || true
        fi
        systemctl daemon-reload || true
        if systemctl is-active --quiet bluetooth.service; then
            systemctl restart bluetooth.service || true
        fi
    fi
    cat <<'EOM'
shepherd-launcher installed. Finish setting up a kiosk user (replace USER):

  shepherd-admin setup-user USER     # config, groups, and the state custodian
  shepherd-admin yt-dlp install      # only for YouTube media libraries

Then have USER log out and back in and pick the "Shepherd Kiosk" session.
Change the policy later (it lives with the state custodian, not in the home):

  shepherd-admin policy USER --source ./config.toml

Before downgrading to a release without the custodian, move the state back:

  shepherd-admin restore-state

Other admin tasks: shepherd-admin apps install steam|chrome|android, harden, bluetooth.
EOM
fi
exit 0
EOF
    } > "$debian/postinst"

    # postrm: reload udev/polkit after our rules leave, and take away the
    # bluetoothd drop-in the postinst wrote -- dpkg never unpacked that one, so
    # dpkg will not remove it either (issue #177). The shepherd-firewall system
    # group is intentionally left in place (leftover files may still reference
    # it, and it is harmless).
    {
        printf '#!/bin/sh\nset -e\n'
        _package_retired_conffiles
        printf 'dropin_dir=%s\n' "$BLUETOOTH_DROPIN_DIR"
        printf 'dropin=%s\n' "$BLUETOOTH_DROPIN_DIR/$BLUETOOTH_DROPIN_NAME"
        cat <<'EOF'
if [ "$1" = "remove" ] || [ "$1" = "purge" ]; then
    # The postinst wrote this one, so dpkg has no record of it to act on.
    rm -f "$dropin"
    # Leave the directory if anything else dropped a file in it.
    rmdir "$dropin_dir" 2>/dev/null || true
    if command -v udevadm >/dev/null 2>&1; then
        udevadm control --reload-rules || true
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl reload polkit 2>/dev/null || true
        systemctl daemon-reload || true
        if systemctl is-active --quiet bluetooth.service; then
            systemctl restart bluetooth.service || true
        fi
    fi
fi
exit 0
EOF
    } > "$debian/postrm"

    chmod 0755 "$debian/preinst" "$debian/postinst" "$debian/postrm"
}

package_deb_usage() {
    cat <<EOF
Usage: shepherd package deb [OPTIONS]

Builds a Debian package for the host architecture by staging the install tree
(via install.sh with DESTDIR) and wrapping it with dpkg-deb.

Options:
    --out DIR      Output directory for the .deb (default: dist/pkg)
    --no-build     Skip the release build; reuse existing target/release
    --allow-missing-dpc
                   Package without the Device Policy Controller apk. By
                   default a missing dpc-waydroid/$DPC_APK_NAME is a hard
                   error, because the resulting package silently cannot
                   offer Android's lock_mode = "locktask". Use this for a
                   smoke build on a host with no Android SDK.
    -h, --help     Show this help

Notes:
    Requires root or fakeroot (staging writes root-owned files). When run
    unprivileged, re-execs the staging step under fakeroot automatically.

    The DPC apk is never built here — it is signed with a persistent key that
    a provisioned device owner can only be updated from, so it is built
    out-of-band by dpc-waydroid/build.sh (the release job does it with the
    org key).

Examples:
    shepherd package deb
    shepherd package deb --out /tmp/out
    shepherd package deb --allow-missing-dpc
EOF
}

package_fdroid_usage() {
    cat <<EOF
Usage: shepherd package fdroid [OPTIONS]

Generates an F-Droid repository from built APKs and asserts that every one of
them reached the index. Validation and local testing only — the published
repository is generated on the server (see dist/fdroid/README.md).

Options:
    --apks DIR     Directory holding the .apk files (default: dist/pkg)
    --out DIR      Output directory (default: dist/fdroid-repo)
    --debug-keys   Drop AllowedAPKSigningKeys, so debug-signed local builds
                   are accepted. Never use this to validate a release.
    -h, --help     Show this help

Notes:
    Requires fdroidserver:  sudo apt install --no-install-recommends fdroidserver

Examples:
    shepherd package fdroid --debug-keys
    shepherd package fdroid --apks dist/pkg --out /tmp/fdroid
EOF
}

# Dispatch for `shepherd package <deb|fdroid>`.
package_main() {
    local subcmd="${1:-}"
    shift || true
    case "$subcmd" in
        deb)
            package_deb "$@"
            ;;
        fdroid)
            package_fdroid "$@"
            ;;
        ""|help|-h|--help)
            cat <<EOF
Usage: shepherd package <command> [OPTIONS]

Commands:
    deb       Build the Debian package for the host architecture
    fdroid    Build + validate an F-Droid repo from built APKs

Run 'shepherd package deb --help' or 'shepherd package fdroid --help'
for options.
EOF
            ;;
        *)
            die "Unknown package command: $subcmd (try: shepherd package help)"
            ;;
    esac
}
