#!/usr/bin/env bash
# Build the F-Droid repository served at https://fdroid.lunchbox-os.com/fdroid/repo.
#
# The output is a tree laid out as the bucket's keys, for upload-fdroid.sh:
#
#   fdroid/repo/entry.jar, entry.json          signed with the index key
#   fdroid/repo/index-v2.json, index-v1.jar, … the catalogue
#   fdroid/repo/index.html, index.png          landing page + QR code
#   fdroid/repo/icons/…, <appId>/en-US/…       repository and listing icons
#   fdroid/repo/lunchbox-*_X.Y.Z.apk           every indexed release's APKs
#
# Unlike the apt repository, the payload is in the bucket too. The F-Droid
# client does not follow redirects, so an APK has to be served from the
# repository's own address. See
# docs/ai/history/2026-09-29 1940 self-hosted-fdroid-repo (#205).md.
#
# `fdroid update` builds its index from the APKs in front of it and drops any
# version whose file is missing, so every indexed APK has to be here for every
# publish. There are two ways to get them, and exactly one must be chosen:
#
#   --previous URL   Fetch the index published at URL, verify it against the
#                    index key's fingerprint (fetch-fdroid-index.sh), then
#                    download every APK it lists from URL and check each
#                    against the SHA-256 it signed.
#   --rebuild DIR    Start from nothing and index every *.apk under DIR (as
#                    fetch-release-apks.sh lays them out). Each must carry a
#                    .sha256 sidecar that matches. This is the first publish,
#                    and the recovery path for a lost or wrong bucket.
#
# Either way everything is re-signed into a new index, so it all has to be
# ours: the APK signing-key pin in dist/fdroid/metadata is what refuses one
# that is not, and fdroid-update.sh turns that refusal into a failure.
#
# An APK whose file name is already indexed is fine if its SHA-256 matches (a
# re-run) and an error if it does not: release assets are immutable, and a
# phone that cached the old index would reject the new bytes.
#
# Reads from the environment:
#   FDROID_URL                repository URL (default:
#                             https://fdroid.lunchbox-os.com/fdroid/repo)
#   FDROID_INDEX_FINGERPRINT  the index key's certificate SHA-256
#                             (default: dist/fdroid/index-key.fingerprint)
#   FDROID_KEYSTORE           keystore holding the index signing key
#   FDROID_KEYSTORE_PASSWORD  its password
#   FDROID_KEY_ALIAS          the key's alias (default: fdroid-index)
#   FDROID_METADATA           app metadata (default: dist/fdroid/metadata)
#
# Usage:
#   publish-fdroid.sh --out DIR (--previous URL | --rebuild DIR) [FILE.apk ...]

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

NAME="Lunchbox Apps"
DESCRIPTION="The Android apps for Lunchbox, a child-friendly, parent-guided desktop environment: the companion app for managing devices, and the media player."

die() {
    echo "::error::$*" >&2
    exit 1
}

usage() {
    sed -n '/^# Usage:/,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

out=""
previous=""
rebuild=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --previous) previous="${2%/}"; shift 2 ;;
        --rebuild) rebuild="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) usage >&2; die "unknown option: $1" ;;
        *) break ;;
    esac
done

[[ -n "$out" ]] || { usage >&2; die "--out is required"; }
if [[ -n "$previous" && -n "$rebuild" ]] || [[ -z "$previous" && -z "$rebuild" ]]; then
    usage >&2
    die "give exactly one of --previous URL or --rebuild DIR"
fi
[[ -z "$rebuild" || -d "$rebuild" ]] || die "--rebuild: not a directory: $rebuild"
if [[ -e "$out" ]] && [[ -n "$(ls -A "$out")" ]]; then
    die "--out must be empty or absent: $out"
fi

url="${FDROID_URL:-https://fdroid.lunchbox-os.com/fdroid/repo}"
url="${url%/}"
# The bucket key prefix is the URL's path: fdroid/repo.
prefix="$(printf '%s' "$url" | sed -E 's|^[a-z]+://[^/]+/?||')"
[[ "$prefix" == */repo || "$prefix" == repo ]] || die "FDROID_URL must end in /repo: $url"

if [[ -z "${FDROID_INDEX_FINGERPRINT:-}" ]]; then
    file="$repo_root/dist/fdroid/index-key.fingerprint"
    [[ -f "$file" ]] || die "$file is missing: the F-Droid index key has not been made yet (see docs/release-signing.md)"
    FDROID_INDEX_FINGERPRINT="$(tr -d '[:space:]' < "$file")"
fi
export FDROID_INDEX_FINGERPRINT
want_fpr="$(printf '%s' "$FDROID_INDEX_FINGERPRINT" | tr 'A-F' 'a-f')"
[[ "$want_fpr" =~ ^[0-9a-f]{64}$ ]] || die "not a SHA-256 fingerprint: $FDROID_INDEX_FINGERPRINT"

: "${FDROID_KEYSTORE:?FDROID_KEYSTORE (the index signing keystore) is required}"
: "${FDROID_KEYSTORE_PASSWORD:?FDROID_KEYSTORE_PASSWORD is required}"
for cmd in keytool curl python3 sha256sum; do
    command -v "$cmd" >/dev/null || die "$cmd is required"
done

# The keystore in the environment and the fingerprint every device trusts are
# two separate things that have to agree. Check before any work is done, so a
# wrong secret fails here rather than as an index no phone accepts.
got_fpr="$(keytool -list -v -keystore "$FDROID_KEYSTORE" -storepass:env FDROID_KEYSTORE_PASSWORD \
    -alias "${FDROID_KEY_ALIAS:-fdroid-index}" 2>/dev/null \
    | awk '/SHA256:/ { gsub(":", "", $2); print tolower($2); exit }')" || true
[[ -n "$got_fpr" ]] || die "could not read the key ${FDROID_KEY_ALIAS:-fdroid-index} from $FDROID_KEYSTORE"
[[ "$got_fpr" == "$want_fpr" ]] \
    || die "the index key in FDROID_KEYSTORE has fingerprint $got_fpr, but devices trust $want_fpr"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
apks="$work/apks"
mkdir -p "$apks"

sha() { sha256sum "$1" | cut -d' ' -f1; }

# file name -> SHA-256 of every APK gathered so far.
declare -A have=()
add() {
    local apk="$1" want="$2" name
    name="$(basename "$apk")"
    [[ "$name" == *.apk ]] || die "not an .apk: $apk"
    local got
    got="$(sha "$apk")"
    [[ -z "$want" || "$got" == "$want" ]] || die "$apk has SHA-256 $got, but $want was expected"
    if [[ -n "${have[$name]:-}" ]]; then
        [[ "${have[$name]}" == "$got" ]] \
            || die "$name is already indexed with SHA-256 ${have[$name]}, but $apk is $got. Release assets must not change once published."
        echo "  $name: already indexed"
        return
    fi
    have["$name"]="$got"
    [[ "$apk" -ef "$apks/$name" ]] || cp "$apk" "$apks/$name"
    echo "  $name: adding"
}

# --- The APKs already published -----------------------------------------------

if [[ -n "$previous" ]]; then
    echo "Fetching the published index from $previous"
    "$here/fetch-fdroid-index.sh" "$previous" "$work/previous.json"
    while read -r name want; do
        [[ "$name" =~ ^[A-Za-z0-9._+-]+\.apk$ ]] || die "the published index names an unexpected file: $name"
        curl -fsSL --retry 3 -o "$apks/$name" "$previous/$name" \
            || die "could not download $previous/$name, which the published index lists"
        add "$apks/$name" "$want"
    done < <(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
for p in d["packages"].values():
    for v in p["versions"].values():
        print(v["file"]["name"].lstrip("/"), v["file"]["sha256"])
' "$work/previous.json" | sort)
fi

if [[ -n "$rebuild" ]]; then
    echo "Indexing every APK under $rebuild"
    while IFS= read -r -d '' apk; do
        [[ -f "$apk.sha256" ]] || die "$apk has no .sha256 sidecar; refusing to index it"
        add "$apk" "$(cut -d' ' -f1 < "$apk.sha256")"
    done < <(find "$rebuild" -name '*.apk' -type f -print0 | sort -z)
fi

# --- This release's APKs ------------------------------------------------------

for apk in "$@"; do
    [[ -f "$apk" ]] || die ".apk not found: $apk"
    add "$apk" ""
done

[[ ${#have[@]} -gt 0 ]] || die "nothing to index"

# --- Generate -----------------------------------------------------------------

meta_args=()
[[ -n "${FDROID_METADATA:-}" ]] && meta_args=(--metadata "$FDROID_METADATA")
"$here/fdroid-update.sh" --out "$work/fdroid" --url "$url" \
    --name "$NAME" --description "$DESCRIPTION" "${meta_args[@]}" "$apks"/*.apk

# The same check a phone makes, on what is about to be uploaded rather than on
# the keystore: the index must verify against the fingerprint devices trust.
(
    cd "$work"
    python3 - "$work/fdroid/repo/entry.jar" "$want_fpr" <<'EOF'
import sys
from fdroidserver import common, index
common.config = common.read_config()
try:
    index.get_index_from_jar(sys.argv[1], sys.argv[2])
except Exception as e:
    sys.exit(f"::error::the new entry.jar does not verify against {sys.argv[2]}: {e}")
EOF
)

# status/ is fdroidserver's report on the run (the runner's OS, tool paths):
# nothing a client reads.
mkdir -p "$out/$prefix"
(cd "$work/fdroid/repo" && find . -mindepth 1 -maxdepth 1 ! -name status -exec cp -a {} "$out/$prefix/" \;)

echo "Wrote $out/$prefix: ${#have[@]} APK(s) indexed"
