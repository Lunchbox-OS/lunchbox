#!/usr/bin/env bash
# Build the Shepherd DPC APK with the plain SDK toolchain (no Gradle).
#
#   aapt2 (compile+link resources) -> javac -> d8 (dex) -> zipalign -> apksigner
#
# Requires an Android SDK with build-tools + a platform jar. Override the SDK
# path / versions via env vars below. Outputs ./shepherd-dpc.apk.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SDK="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-/opt/android-sdk}}"
BT_VER="${ANDROID_BUILD_TOOLS:-35.0.0}"
PLATFORM_VER="${ANDROID_PLATFORM:-android-35}"
BT="$SDK/build-tools/$BT_VER"
ANDROID_JAR="$SDK/platforms/$PLATFORM_VER/android.jar"
OUT="$HERE/build"
APK="$HERE/shepherd-dpc.apk"
# Keystore lives OUTSIDE $OUT (which is wiped each build) so the signing key is
# stable across rebuilds — otherwise an update fails with a signature mismatch,
# and (once it's device owner) the app can't be updated at all. Gitignored.
#
# CI overrides these to sign with the persistent release keystore (a
# device-owner app can ONLY be updated with the same key, so that key must never
# change): DPC_KEYSTORE (path), DPC_KEYSTORE_PASS, DPC_KEY_PASS, DPC_KEY_ALIAS.
# Unset -> a local dev keystore (auto-generated below), which is fine for dev but
# must never sign a released apk.
KS="${DPC_KEYSTORE:-$HERE/dpc.keystore}"
KS_PASS="${DPC_KEYSTORE_PASS:-android}"
KEY_PASS="${DPC_KEY_PASS:-android}"
KEY_ALIAS="${DPC_KEY_ALIAS:-dpc}"

for tool in "$BT/aapt2" "$BT/d8" "$BT/zipalign" "$BT/apksigner"; do
    [ -x "$tool" ] || { echo "missing $tool (set ANDROID_SDK_ROOT / ANDROID_BUILD_TOOLS)"; exit 1; }
done
[ -f "$ANDROID_JAR" ] || { echo "missing $ANDROID_JAR (set ANDROID_PLATFORM)"; exit 1; }

rm -rf "$OUT"; mkdir -p "$OUT/gen" "$OUT/classes"

echo "[1/6] aapt2 compile resources"
"$BT/aapt2" compile --dir "$HERE/res" -o "$OUT/res.zip"

echo "[2/6] aapt2 link"
"$BT/aapt2" link \
    -o "$OUT/base.apk" \
    -I "$ANDROID_JAR" \
    --manifest "$HERE/AndroidManifest.xml" \
    -R "$OUT/res.zip" \
    --java "$OUT/gen" \
    --version-code "${DPC_VERSION_CODE:-1}" --version-name "${DPC_VERSION_NAME:-1.0}" \
    --min-sdk-version 28 --target-sdk-version 33

echo "[3/6] javac"
mapfile -t SRCS < <(find "$HERE/java" "$OUT/gen" -name '*.java')
javac -source 8 -target 8 -Xlint:-options -cp "$ANDROID_JAR" -d "$OUT/classes" "${SRCS[@]}"

echo "[4/6] d8 -> classes.dex"
mapfile -t CLASSES < <(find "$OUT/classes" -name '*.class')
"$BT/d8" --lib "$ANDROID_JAR" --min-api 28 --output "$OUT" "${CLASSES[@]}"
( cd "$OUT" && zip -qj base.apk classes.dex )

echo "[5/6] zipalign"
"$BT/zipalign" -f -p 4 "$OUT/base.apk" "$OUT/aligned.apk"

echo "[6/6] sign"
if [ ! -f "$KS" ]; then
    keytool -genkeypair -keystore "$KS" -storepass "$KS_PASS" -keypass "$KEY_PASS" \
        -alias "$KEY_ALIAS" -keyalg RSA -keysize 2048 -validity 10000 \
        -dname "CN=Shepherd DPC" >/dev/null 2>&1
fi
"$BT/apksigner" sign --ks "$KS" --ks-pass "pass:$KS_PASS" --key-pass "pass:$KEY_PASS" \
    --ks-key-alias "$KEY_ALIAS" --out "$APK" "$OUT/aligned.apk"

echo "OK -> $APK"
