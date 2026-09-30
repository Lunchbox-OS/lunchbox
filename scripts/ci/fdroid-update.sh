#!/usr/bin/env bash
# Generate an F-Droid repository from APKs, and check that every one of them
# made it in.
#
# This is the one place the repository is generated. `lunchbox package fdroid`
# runs it with a throwaway index key to validate dist/fdroid/metadata against
# built APKs; publishing runs it with the real key. Keeping both on the same
# code is what makes the validation mean something.
#
# DIR becomes fdroidserver's working directory:
#
#   DIR/config.yml      generated here
#   DIR/metadata/       copied from --metadata
#   DIR/repo/           the APKs, and the repository fdroid update writes
#
# `fdroid update` reports an APK rejected by AllowedAPKSigningKeys as a warning,
# drops it, signs a smaller index and exits 0. So a successful exit is not
# evidence that anything was published: every APK given must be named in
# index-v2.json afterwards, or this fails.
#
# Reads from the environment:
#   FDROID_KEYSTORE           keystore holding the index signing key (required)
#   FDROID_KEYSTORE_PASSWORD  its password; PKCS12 uses it for the key too
#   FDROID_KEY_ALIAS          the key's alias (default: fdroid-index)
#
# Usage:
#   fdroid-update.sh --out DIR --url URL --name NAME --description TEXT
#                    [--metadata DIR] [--debug-keys] FILE.apk [...]

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() {
    echo "::error::$*" >&2
    exit 1
}

usage() {
    sed -n '/^# Usage:/,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

out=""
url=""
name=""
description=""
metadata="$repo_root/dist/fdroid/metadata"
debug_keys=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --url) url="$2"; shift 2 ;;
        --name) name="$2"; shift 2 ;;
        --description) description="$2"; shift 2 ;;
        --metadata) metadata="$2"; shift 2 ;;
        --debug-keys) debug_keys=true; shift ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) usage >&2; die "unknown option: $1" ;;
        *) break ;;
    esac
done

# An empty description writes `repo_description:` as null, and fdroid update
# then dies building index.xml with a bare TypeError.
[[ -n "$out" && -n "$url" && -n "$name" && -n "$description" ]] \
    || { usage >&2; die "--out, --url, --name and --description are required"; }
# fdroidserver refuses any other repository URL.
[[ "$url" == */repo ]] || die "--url must end in /repo: $url"
[[ $# -gt 0 ]] || die "no .apk files given"
: "${FDROID_KEYSTORE:?FDROID_KEYSTORE (the index signing keystore) is required}"
: "${FDROID_KEYSTORE_PASSWORD:?FDROID_KEYSTORE_PASSWORD is required}"
[[ -f "$FDROID_KEYSTORE" ]] || die "keystore not found: $FDROID_KEYSTORE"
[[ -d "$metadata" ]] || die "metadata directory not found: $metadata"

command -v fdroid >/dev/null \
    || die "fdroid is required: sudo apt install --no-install-recommends fdroidserver default-jdk-headless"

# fdroidserver signs the index jar with jarsigner. On Debian/Ubuntu it
# *unconditionally* prefers /usr/lib/jvm/default-java when that directory
# exists (fdroidserver/common.py, "always prefer the built-in"), ignoring both
# JAVA_HOME and any config key -- so a box where default-java points at a JRE
# fails deep inside the run with an opaque OSError. Catch it here.
default_java="/usr/lib/jvm/default-java"
if [[ -d "$default_java" && ! -x "$default_java/bin/jarsigner" ]]; then
    die "$default_java is a JRE (no jarsigner), and fdroidserver prefers it over every other JDK. Install a JDK as the default: sudo apt install --no-install-recommends default-jdk-headless"
fi

apks=()
for apk in "$@"; do
    [[ -f "$apk" ]] || die ".apk not found: $apk"
    apks+=("$apk")
done

rm -rf "$out"
mkdir -p "$out/repo" "$out/metadata"
cp "${apks[@]}" "$out/repo/"

for meta in "$metadata"/*.yml; do
    if [[ "$debug_keys" == true ]]; then
        # Drop AllowedAPKSigningKeys (the key and its indented list items)
        # so debug-signed local builds aren't rejected by the pin.
        awk '
            /^AllowedAPKSigningKeys:/ { skip = 1; next }
            skip && /^[[:space:]]/    { next }
                                      { skip = 0; print }
        ' "$meta" > "$out/metadata/$(basename "$meta")"
    else
        cp "$meta" "$out/metadata/"
    fi
done
[[ "$debug_keys" == true ]] && echo "::warning::--debug-keys: signing-key pin NOT enforced" >&2

# Absolute, because fdroid update runs from $out.
keystore="$(realpath "$FDROID_KEYSTORE")"
config="$out/config.yml"
# Every value double-quoted: a JSON string is a valid YAML one, and a colon or
# a # in the description or the password would otherwise break the file, or
# quietly truncate the value.
q() { python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$1"; }
# Written with the password in it, so readable by the owner only from the start
# (fdroid warns about anything looser, too).
(
    umask 077
    cat > "$config" <<EOF
repo_url: $(q "$url")
repo_name: $(q "$name")
repo_description: $(q "$description")
# 0 = never archive: every version stays in repo/, so every version the index
# was built from stays installable.
archive_older: 0
keystore: $(q "$keystore")
repo_keyalias: $(q "${FDROID_KEY_ALIAS:-fdroid-index}")
keystorepass: $(q "$FDROID_KEYSTORE_PASSWORD")
keypass: $(q "$FDROID_KEYSTORE_PASSWORD")
EOF
)

# --delete-unknown removes anything in repo/ that didn't make it into the
# index -- including an APK rejected by AllowedAPKSigningKeys, which is what
# the check below detects.
echo "Running fdroid update on ${#apks[@]} APK(s)"
(cd "$out" && fdroid update --delete-unknown --pretty) \
    || die "fdroid update failed -- see the output above"

index="$out/repo/index-v2.json"
[[ -f "$index" ]] || die "fdroid update produced no $index"

missing=()
for apk in "${apks[@]}"; do
    grep -qF "\"/$(basename "$apk")\"" "$index" || missing+=("$apk")
done
if [[ ${#missing[@]} -gt 0 ]]; then
    for apk in "${missing[@]}"; do
        echo "::error::$(basename "$apk") was given but did NOT reach the index" >&2
        if command -v apksigner >/dev/null; then
            apksigner verify --print-certs "$apk" 2>/dev/null \
                | grep -i 'SHA-256 digest' | sed 's/^/  signed by: /' >&2 || true
        fi
    done
    die "The usual cause is AllowedAPKSigningKeys in $metadata not matching the key the APK is signed with"
fi

echo "Indexed ${#apks[@]} APK(s) into $out/repo"
