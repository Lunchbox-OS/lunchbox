#!/usr/bin/env bash
# Upload a tree built by publish-fdroid.sh to the R2 bucket behind
# https://fdroid.lunchbox-os.com, in the order that keeps the live repository
# consistent.
#
# R2 has no atomic deploy, so order is what stands in for one:
#
#   1. APKs.            Nothing refers to them until the index does.
#   2. Everything else  the index files, icons and landing page.
#      but entry.*
#   3. entry.json, then entry.jar, last.
#
# A client reads entry.jar first and checks the index it names against the
# SHA-256 inside it, so a phone that refreshes mid-upload sees an old entry.jar
# that no longer matches, reports a failed refresh, and gets the complete set
# next time. It never installs anything the index did not sign.
#
# An APK is never overwritten. One already in the bucket is skipped if the
# SHA-256 it was uploaded with (x-amz-meta-sha256) matches, which is a re-run,
# and is an error if not: a phone holding the old index would reject the new
# bytes. Nothing is ever deleted.
#
# Cache-Control is set per object, and the custom domain's cache honours it:
# no-cache for everything that changes between releases, a year and immutable
# for an APK, whose file name is never reused.
#
# Requests go straight to R2's S3 API, signed by curl (--aws-sigv4), so this
# needs nothing the runner does not already have.
#
# Reads from the environment:
#   FDROID_S3_ENDPOINT           the S3 API endpoint, e.g.
#                                https://<account id>.r2.cloudflarestorage.com
#   FDROID_BUCKET                bucket name (default: lunchbox-fdroid)
#   FDROID_S3_ACCESS_KEY_ID      an R2 API token's access key id
#   FDROID_S3_SECRET_ACCESS_KEY  and its secret
#
# Usage: upload-fdroid.sh DIR

set -euo pipefail

die() {
    echo "::error::$*" >&2
    exit 1
}

[[ $# -eq 1 ]] || { echo "usage: upload-fdroid.sh DIR" >&2; exit 1; }
dir="${1%/}"
[[ -d "$dir" ]] || die "not a directory: $dir"
: "${FDROID_S3_ENDPOINT:?FDROID_S3_ENDPOINT is required}"
: "${FDROID_S3_ACCESS_KEY_ID:?FDROID_S3_ACCESS_KEY_ID is required}"
: "${FDROID_S3_SECRET_ACCESS_KEY:?FDROID_S3_SECRET_ACCESS_KEY is required}"
base="${FDROID_S3_ENDPOINT%/}/${FDROID_BUCKET:-lunchbox-fdroid}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The credentials go to curl in a config file rather than argv, where `ps`
# would show them.
(
    umask 077
    printf 'user = "%s:%s"\n' "$FDROID_S3_ACCESS_KEY_ID" "$FDROID_S3_SECRET_ACCESS_KEY" > "$work/curlrc"
)

# Percent-encode a key the way SigV4 canonicalises it: everything but the
# unreserved characters and the path separators. curl signs the path as it is
# given, so an unencoded `=` (fdroidserver's icon names have one) would be
# signed as `=` and checked by the server as `%3D`.
encode() {
    python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe="/-_.~"))' "$1"
}

content_type() {
    case "$1" in
        *.apk) echo application/vnd.android.package-archive ;;
        *.jar) echo application/java-archive ;;
        *.json) echo application/json ;;
        *.xml) echo application/xml ;;
        *.html) echo 'text/html; charset=utf-8' ;;
        *.css) echo text/css ;;
        *.png) echo image/png ;;
        *) echo application/octet-stream ;;
    esac
}

EMPTY_SHA256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

# HEAD KEY -> prints the HTTP status; headers land in $work/head.
head_object() {
    curl -sS -K "$work/curlrc" --aws-sigv4 "aws:amz:auto:s3" \
        -H "x-amz-content-sha256: $EMPTY_SHA256" \
        -I -o /dev/null -D "$work/head" -w '%{http_code}' \
        "$base/$(encode "$1")"
}

put_object() {
    local file="$1" key="$2" cache="$3" sha
    sha="$(sha256sum "$file" | cut -d' ' -f1)"
    # The payload hash is signed, so R2 checks the bytes it stored are these.
    curl -sS --fail-with-body -K "$work/curlrc" --aws-sigv4 "aws:amz:auto:s3" \
        -H "x-amz-content-sha256: $sha" \
        -H "x-amz-meta-sha256: $sha" \
        -H "Content-Type: $(content_type "$key")" \
        -H "Cache-Control: $cache" \
        --retry 3 --upload-file "$file" -o "$work/put" \
        "$base/$(encode "$key")" \
        || { cat "$work/put" >&2 2>/dev/null; echo >&2; die "uploading $key failed"; }
}

mapfile -d '' files < <(cd "$dir" && find . -type f -print0 | sed -z 's|^\./||' | sort -z)
[[ ${#files[@]} -gt 0 ]] || die "nothing to upload in $dir"

apks=()
rest=()
entry=()
for key in "${files[@]}"; do
    case "$key" in
        *.apk) apks+=("$key") ;;
        */entry.json|entry.json) entry=("$key" "${entry[@]}") ;;
        */entry.jar|entry.jar) entry+=("$key") ;;
        *) rest+=("$key") ;;
    esac
done
[[ ${#entry[@]} -eq 2 ]] || die "$dir must hold exactly one entry.json and one entry.jar"
[[ "${entry[1]}" == *entry.jar ]] || die "internal: entry.jar must be uploaded last"

echo "1. APKs"
uploaded=0
for key in "${apks[@]}"; do
    sha="$(sha256sum "$dir/$key" | cut -d' ' -f1)"
    code="$(head_object "$key")" || die "could not check whether $key exists"
    case "$code" in
        200)
            existing="$(awk 'tolower($1) == "x-amz-meta-sha256:" { print $2 }' "$work/head" | tr -d '\r')"
            [[ "$existing" == "$sha" ]] \
                || die "$key is already in the bucket with SHA-256 ${existing:-unknown}, but this one is $sha. Published APKs are never replaced."
            echo "  $key: already uploaded"
            ;;
        404)
            put_object "$dir/$key" "$key" "public, max-age=31536000, immutable"
            echo "  $key: uploaded"
            uploaded=$((uploaded + 1))
            ;;
        *)
            die "checking $key returned HTTP $code"
            ;;
    esac
done

echo "2. Index, icons and landing page"
for key in "${rest[@]}"; do
    put_object "$dir/$key" "$key" "no-cache"
    echo "  $key"
done

echo "3. Entry point"
for key in "${entry[@]}"; do
    put_object "$dir/$key" "$key" "no-cache"
    echo "  $key"
done

echo "Uploaded to $base: $uploaded new APK(s), $(( ${#rest[@]} + ${#entry[@]} )) other file(s)"
