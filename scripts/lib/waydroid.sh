#!/usr/bin/env bash
# Engine-side Waydroid (Android activity kind) provisioning: GApps image init,
# libndk ARM translation (amd64 only), and the DPC device-owner install. These
# sit on top of an operator-installed Waydroid *engine* (the `apps install
# android` guidance prints how to install it) and are invoked from admin.sh's
# `apps install android` path.
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
    waydroid --details-to-stdout shell -- "$@" 2>&1 | tr -d '\r' \
        | grep -vE '^\[[0-9:]+\][[:space:]]*%[[:space:]]*lxc-info|^\[[0-9:]+\][[:space:]]*RUNNING$'
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

# Install the libndk ARM translation layer so ARM-only apps run on an amd64 host.
# Overlay-based (no system.img surgery): drops the prebuilt libs into the
# Waydroid system overlay and adds the native-bridge props to waydroid.cfg.
# Offline + root; takes effect on the next session start. amd64-only + idempotent.
install_libndk() {
    require_root
    local arch; arch="$(dpkg --print-architecture 2>/dev/null || echo unknown)"
    if [[ "$arch" != "amd64" ]]; then
        info "libndk ARM translation is only needed on amd64 (host is $arch); skipping — ARM apps run natively here."
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

    # Resolve the apk: packaged at /usr/share/shepherd/, else the source tree's
    # dpc-waydroid/ (built out-of-band by dpc-waydroid/build.sh).
    local data_dir apk
    data_dir="$(get_data_dir)"
    apk="$data_dir/$DPC_APK_NAME"
    [[ -f "$apk" ]] || apk="$data_dir/dpc-waydroid/$DPC_APK_NAME"
    if [[ ! -f "$apk" ]]; then
        die "DPC apk not found (looked in $data_dir). From source, build it first: (cd dpc-waydroid && ANDROID_SDK_ROOT=/opt/android-sdk ./build.sh)"
    fi

    if ! waydroid_session_running; then
        die "The DPC install needs a running Waydroid session (Android booted). Start one as $user (e.g. 'waydroid show-full-ui' in their graphical session, or scripts/integration-tests/test-waydroid.sh), then re-run. IMPORTANT: set the device owner BEFORE signing into any Google account."
    fi

    # Push into the session user's /data and pm install (waydroid app install is
    # unreliable headless). Play Protect delays registration ~15 s on GApps, so
    # poll rather than trust the immediate "Success".
    local dst; dst="$(get_user_home "$user")/.local/share/waydroid/data/local/tmp/$DPC_APK_NAME"
    install -D -m 0644 "$apk" "$dst"
    info "Installing the DPC (Play Protect may delay registration ~15 s on GApps)..."
    waydroid_shell pm install -r -g "/data/local/tmp/$DPC_APK_NAME" >/dev/null 2>&1 || true

    local ok=false _
    for _ in $(seq 1 15); do
        if waydroid_shell pm list packages | grep -qx "package:$DPC_PACKAGE"; then
            ok=true
            break
        fi
        sleep 2
    done
    [[ "$ok" == "true" ]] || die "DPC package did not register after install (pm install failed?)."

    if waydroid_shell dumpsys account | grep -qi 'type=com.google'; then
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
