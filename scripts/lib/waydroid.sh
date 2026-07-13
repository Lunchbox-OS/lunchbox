#!/usr/bin/env bash
# Engine-side Waydroid (Android activity kind) provisioning: GApps image init,
# libndk ARM translation (amd64 only; arm64 runs ARM natively), and the DPC
# device-owner install. These sit on top of an operator-installed Waydroid
# *engine* (the `apps install android` guidance prints how to install it) and
# are invoked from admin.sh's `apps install android` path.
#
# Sourced by scripts/lib/admin.sh. Relies on common.sh (info/warn/die/success,
# require_root/require_command, maybe_sudo, ensure_dir, validate_user,
# get_user_home, get_data_dir, get_repo_root) and install.sh (WAYDROID_* +
# provision_waydroid_host), both sourced by the entrypoints before admin.sh.

# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

# --- constants ---------------------------------------------------------------

WAYDROID_CFG="/var/lib/waydroid/waydroid.cfg"
WAYDROID_OVERLAY="/var/lib/waydroid/overlay"

# libndk ARM-translation payload for Android 13 / LineageOS 20. Pinned to the
# exact upstream commit + md5 that casualsnek/waydroid_script ships, so the
# proprietary blob is auditable and reproducible. amd64 only — arm64 hosts run
# ARM apps natively and need no translation.
LIBNDK_URL="https://github.com/supremegamers/vendor_google_proprietary_ndk_translation-prebuilt/archive/68734c52556d3d7a6db34c603dd9276915c29f2f.zip"
LIBNDK_MD5="0b2207c490fcb400aa5c87fcf0d52d38"
# Props libndk needs, added to waydroid.cfg's [properties] (applied at boot).
# Same set waydroid_script applies; with overlayfs no system.img edit is needed.
LIBNDK_PROPS=(
    "ro.product.cpu.abilist=x86_64,x86,arm64-v8a,armeabi-v7a,armeabi"
    "ro.product.cpu.abilist32=x86,armeabi-v7a,armeabi"
    "ro.product.cpu.abilist64=x86_64,arm64-v8a"
    "ro.dalvik.vm.native.bridge=libndk_translation.so"
    "ro.enable.native.bridge.exec=1"
    "ro.vendor.enable.native.bridge.exec=1"
    "ro.vendor.enable.native.bridge.exec64=1"
)

# The Device Policy Controller app shipped in dpc-waydroid/.
DPC_PACKAGE="com.armeafamily.shepherd.dpc"
DPC_ADMIN="$DPC_PACKAGE/.AdminReceiver"
DPC_APK_NAME="shepherd-dpc.apk"

# --- helpers -----------------------------------------------------------------

# True if the initialized system image is the GApps variant.
waydroid_is_gapps() {
    grep -qiE '^[[:space:]]*system_ota[[:space:]]*=.*GAPPS' "$WAYDROID_CFG" 2>/dev/null
}

# True if a Waydroid session (Android user 0) is currently running.
waydroid_session_running() {
    waydroid status 2>/dev/null | tr -d '\r' | grep -qiE '^Session:[[:space:]]*RUNNING'
}

# Root `waydroid shell` with stdout captured (the global --details-to-stdout is
# required for output; `waydroid shell` itself needs root). CRLF stripped.
waydroid_shell() {
    # Strip waydroid's own `[HH:MM:SS]`-prefixed status lines (lxc-info /
    # RUNNING, and the lxc-freeze / lxc-unfreeze / FROZEN chatter emitted when
    # the idle-suspended container is thawed to run the command). Real command
    # output never carries that prefix.
    waydroid --details-to-stdout shell -- "$@" 2>&1 | tr -d '\r' | grep -vE '^\[[0-9:]+\]'
}

# Upsert keys into waydroid.cfg's [properties] section. Uses python3 (guaranteed
# on any Waydroid host — Waydroid is a python app) with configparser, exactly as
# waydroid_script does, preserving key case.
waydroid_cfg_set_props() {
    python3 - "$WAYDROID_CFG" "$@" <<'PY'
import configparser, sys
cfg_path, props = sys.argv[1], sys.argv[2:]
cfg = configparser.ConfigParser()
cfg.optionxform = str  # preserve key case
cfg.read(cfg_path)
if not cfg.has_section("properties"):
    cfg.add_section("properties")
for kv in props:
    k, _, v = kv.partition("=")
    cfg.set("properties", k, v)
with open(cfg_path, "w") as f:
    cfg.write(f)
PY
}

# --- provisioning steps ------------------------------------------------------

# Initialize (or re-init) the Waydroid system image as GApps. Offline + root; no
# session needed; takes effect on the next container start. Downloads ~2.4 GB
# when it actually (re-)inits.
#
# Args:
#   $1 -- kiosk user (for --clean's per-user data path)
#   $2 -- "--clean" to ALSO wipe ~user/.local/share/waydroid/data (destroys any
#         existing device owner + user data). Never the default; `waydroid
#         init -f` does NOT wipe that dir, so a truly fresh device needs this.
provision_waydroid_gapps() {
    local user="${1:-}" clean="${2:-}"
    require_root
    require_command waydroid

    if waydroid_is_gapps; then
        info "Waydroid system image is already GApps."
    else
        info "Initializing the Waydroid GApps image (downloads ~2.4 GB)..."
        waydroid session stop >/dev/null 2>&1 || true
        maybe_sudo systemctl stop waydroid-container >/dev/null 2>&1 || true
        local force=""
        [[ -f "$WAYDROID_CFG" ]] && force="-f"  # already initialized -> force re-init
        # shellcheck disable=SC2086  # $force is a single optional flag
        waydroid init $force -s GAPPS
    fi

    if [[ "$clean" == "--clean" && -n "$user" ]]; then
        local data; data="$(get_user_home "$user")/.local/share/waydroid/data"
        warn "Wiping $data for a clean device (removes any device owner + user data)..."
        rm -rf "$data"
    fi
}

# Install the libndk ARM translation layer so ARM-only apps run on amd64 (arm64
# hosts run them natively, so this is a no-op there).
# Overlay-based (no system.img surgery): drops the prebuilt libs into the
# Waydroid system overlay and adds the native-bridge props to waydroid.cfg.
# Offline + root; takes effect on the next session start. amd64-only (arm64 is
# native) + idempotent.
install_libndk() {
    require_root
    local arch; arch="$(dpkg --print-architecture 2>/dev/null || echo unknown)"
    if [[ "$arch" != "amd64" ]]; then # arm64/aarch64 hosts run ARM apps natively
        info "libndk ARM translation is only needed on amd64 (host is $arch; arm64 runs ARM natively); skipping."
        return 0
    fi
    require_command waydroid
    require_command unzip
    if ! grep -qiE '^[[:space:]]*mount_overlays[[:space:]]*=[[:space:]]*True' "$WAYDROID_CFG" 2>/dev/null; then
        die "libndk install requires Waydroid overlayfs (mount_overlays = True in $WAYDROID_CFG)."
    fi

    if [[ -f "$WAYDROID_OVERLAY/system/lib64/libndk_translation.so" ]] \
        && grep -qiE 'native\.bridge[[:space:]]*=[[:space:]]*libndk_translation' "$WAYDROID_CFG" 2>/dev/null; then
        info "libndk ARM translation already installed."
        return 0
    fi

    local tmp_zip tmp_extract
    tmp_zip="$(mktemp --suffix=.zip)"
    tmp_extract="$(mktemp -d)"

    info "Downloading the libndk ARM-translation payload..."
    curl -fSL "$LIBNDK_URL" -o "$tmp_zip"
    if ! echo "$LIBNDK_MD5  $tmp_zip" | md5sum -c - >/dev/null 2>&1; then
        rm -rf "$tmp_zip" "$tmp_extract"
        die "libndk payload checksum mismatch (expected $LIBNDK_MD5)"
    fi
    unzip -q "$tmp_zip" -d "$tmp_extract"

    local prebuilts
    prebuilts="$(echo "$tmp_extract"/*/prebuilts)"
    if [[ ! -d "$prebuilts" ]]; then
        rm -rf "$tmp_zip" "$tmp_extract"
        die "unexpected libndk archive layout (no prebuilts/ dir)"
    fi

    info "Installing libndk libraries into the Waydroid system overlay..."
    ensure_dir "$WAYDROID_OVERLAY/system" 0755
    cp -a "$prebuilts/." "$WAYDROID_OVERLAY/system/"

    info "Applying ARM-translation properties to waydroid.cfg..."
    waydroid_cfg_set_props "${LIBNDK_PROPS[@]}"

    rm -rf "$tmp_zip" "$tmp_extract"
    success "libndk ARM translation installed (takes effect on the next session start)."
}

# True if the device is NOT Play-certified. GMS caches an `uncertified_status`
# GServices flag (1 = uncertified, 0 = certified); absent/unknown is treated as
# uncertified so we never skip a needed certification step. Requires a session.
waydroid_is_uncertified() {
    local status
    status="$(waydroid_shell sqlite3 /data/data/com.google.android.gsf/databases/gservices.db \
        "select value from main where name='uncertified_status';" 2>/dev/null | tr -d '[:space:]')"
    [[ "$status" != "0" ]]
}

# The GSF (Google Services Framework) Android ID — the value to register at
# google.com/android/uncertified. Empty until GMS has generated it.
waydroid_gsf_android_id() {
    waydroid_shell sqlite3 /data/data/com.google.android.gsf/databases/gservices.db \
        "select value from main where name='android_id';" 2>/dev/null | tr -d '[:space:]'
}

# On an uncertified device, print the GSF Android ID and how to register it; on a
# certified device this is a quiet no-op. Requires a running session.
waydroid_report_certification() {
    if ! waydroid_is_uncertified; then
        info "Device is Play-certified."
        return 0
    fi
    warn "Device is NOT Play-certified — Play sign-in / app installs won't work until you register it:"
    local gsf; gsf="$(waydroid_gsf_android_id)"
    if [[ -n "$gsf" ]]; then
        info "  1. Register this GSF Android ID at https://www.google.com/android/uncertified"
        info "       $gsf"
    else
        info "  1. Open the Play Store once (to generate the GSF ID), then re-run to print it;"
        info "     register it at https://www.google.com/android/uncertified"
    fi
    info "  2. Wait a few minutes, then restart the Waydroid session."
}

# Install the DPC apk and set it as device owner. Needs a RUNNING session
# (Android booted) because pm/dpm run inside the container, and must happen
# BEFORE any Google sign-in (the only device-owner gate is accounts=0).
#
# Args: $1 -- kiosk user (the session/data owner)
install_dpc() {
    local user="${1:-}"
    require_root
    [[ -n "$user" ]] || die "install_dpc requires the kiosk user"
    validate_user "$user"

    if ! waydroid_session_running; then
        die "The DPC install needs a running Waydroid session (Android booted). Start one as $user (e.g. 'waydroid show-full-ui' in their graphical session, or scripts/integration-tests/test-waydroid.sh), then re-run. IMPORTANT: set the device owner BEFORE signing into any Google account."
    fi

    # (Re)install the apk unless the same DPC version is already installed. The
    # apk's version comes from the `.version` sidecar build.sh writes next to it
    # (a kiosk has no Android SDK / aapt to read the apk's manifest). Skipping a
    # same-version reinstall avoids the GApps registration lag and needlessly
    # reinstalling a device-owner app. A different (newer) version still installs;
    # an apk whose version we can't read (an old build with no sidecar) never
    # clobbers an already-installed DPC.
    local data_dir apk apk_ver installed_ver
    data_dir="$(get_data_dir)"
    apk="$data_dir/$DPC_APK_NAME"
    [[ -f "$apk" ]] || apk="$data_dir/dpc-waydroid/$DPC_APK_NAME"
    [[ -f "$apk" ]] && apk_ver="$(head -n1 "$apk.version" 2>/dev/null | tr -d '[:space:]')"
    installed_ver="$(waydroid_shell dumpsys package "$DPC_PACKAGE" 2>/dev/null \
        | grep -oE 'versionName=[^[:space:]]+' | head -n1 | cut -d= -f2)"

    if [[ -n "$installed_ver" && ( -z "${apk_ver:-}" || "$apk_ver" == "$installed_ver" ) ]]; then
        info "DPC $installed_ver is already installed; skipping the reinstall."
    else
        [[ -f "$apk" ]] || die "DPC apk not found (looked in $data_dir). From source, build it first: (cd dpc-waydroid && ANDROID_SDK_ROOT=/opt/android-sdk ./build.sh)"

        # Push into the session user's /data and pm install (waydroid app install
        # is unreliable headless).
        local dst; dst="$(get_user_home "$user")/.local/share/waydroid/data/local/tmp/$DPC_APK_NAME"
        install -D -m 0644 "$apk" "$dst"

        # GApps Play Protect gates pm install: before the device has checked in
        # (no certification / no account yet) it can stall or fail verification,
        # so the install never registers. Skip verification for our own trusted
        # apk, restoring the prior setting after.
        info "Installing the DPC${installed_ver:+ (updating $installed_ver -> ${apk_ver:-?})} (Play Protect verification disabled for this sideload)..."
        local prev_verify
        prev_verify="$(waydroid_shell settings get global verifier_verify_adb_installs | tr -d '[:space:]')"
        [[ "$prev_verify" =~ ^[01]$ ]] || prev_verify=1  # default is verify-on
        waydroid_shell settings put global verifier_verify_adb_installs 0 >/dev/null 2>&1 || true
        local install_out
        install_out="$(waydroid_shell pm install -r -g "/data/local/tmp/$DPC_APK_NAME")"
        waydroid_shell settings put global verifier_verify_adb_installs "$prev_verify" >/dev/null 2>&1 || true

        # Even with verification disabled, GApps lags registering the package in
        # `pm list packages` (GMS VerifyApps runs a separate ~15 s pass), so poll
        # generously — the install itself has already returned Success by here.
        local ok=false _
        for _ in $(seq 1 30); do
            if waydroid_shell pm list packages | grep -qx "package:$DPC_PACKAGE"; then
                ok=true
                break
            fi
            sleep 2
        done
        [[ "$ok" == "true" ]] || die "DPC package did not register within 60s. pm install said: ${install_out:-<no output>}"
    fi

    # Updating an already-provisioned device keeps the DPC as owner — done.
    if waydroid_shell dpm list-owners | grep -qi "$DPC_PACKAGE"; then
        success "DPC ${apk_ver:-$installed_ver} present; already device owner."
        return 0
    fi

    # Device owner requires no Google account yet. A real signed-in account shows
    # as `Account {name=…, type=com.google}`; the always-present GMS
    # `AuthenticatorDescription {type=com.google}` is NOT an account, so match the
    # former only (a loose `type=com.google` false-positives on a fresh device).
    if waydroid_shell dumpsys account | grep -qE 'Account \{[^}]*type=com\.google'; then
        die "A Google account is already present; set-device-owner requires accounts=0. Provision the DPC on a fresh device BEFORE signing in (or reset the device)."
    fi

    info "Setting the DPC as device owner..."
    waydroid_shell dpm set-device-owner "$DPC_ADMIN" || true
    if waydroid_shell dpm list-owners | grep -qi "$DPC_PACKAGE"; then
        success "DPC installed and set as device owner."
    else
        die "set-device-owner did not take — see the output above."
    fi
}
