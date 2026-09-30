#!/usr/bin/env bash
# Download the APKs of every published, non-prerelease release from v0.6.0 on,
# with their .sha256 sidecars, into DIR/<tag>/, which is what
# `publish-fdroid.sh --rebuild DIR` reads.
#
# This is the F-Droid repository's recovery path, and its first publish: the
# repository is normally built from the APKs already in the bucket, and this
# regenerates it from the release assets when the bucket is empty or wrong.
# It is one download by CI per rebuild, not a place users download from.
#
# v0.6.0 is the floor because it is the first release of the com.lunchboxos.*
# apps. Earlier tags are Forgejo-era, carry com.armeafamily.shepherd.* APKs
# signed by a retired key, and are not GitHub releases anyway.
#
# Reads from the environment:
#   GH_TOKEN  a token that can read releases
#   REPO      owner/repo
#
# Usage: fetch-release-apks.sh DIR

set -euo pipefail

SINCE=0.6.0

[[ $# -eq 1 ]] || { echo "usage: fetch-release-apks.sh DIR" >&2; exit 1; }
: "${REPO:?REPO (owner/repo) is required}"
dir="$1"
mkdir -p "$dir"

mapfile -t tags < <(gh release list --repo "$REPO" --limit 1000 \
    --exclude-drafts --exclude-pre-releases --json tagName --jq '.[].tagName')
fetched=0
for tag in "${tags[@]}"; do
    version="${tag#v}"
    if [[ "$(printf '%s\n' "$SINCE" "$version" | sort -V | head -n1)" != "$SINCE" ]]; then
        echo "  $tag: before v$SINCE; not indexed"
        continue
    fi
    # `gh release download` fails on a release with no matching asset rather
    # than downloading nothing. v0.6.0 is one: published by hand, sealed empty
    # by release immutability.
    apks="$(gh release view "$tag" --repo "$REPO" --json assets \
        --jq '[.assets[].name | select(endswith(".apk"))] | length')"
    if [[ "$apks" -eq 0 ]]; then
        echo "  $tag: no .apk assets; nothing to index"
        continue
    fi
    gh release download "$tag" --repo "$REPO" --dir "$dir/$tag" \
        --pattern '*.apk' --pattern '*.apk.sha256'
    echo "  $tag: $(find "$dir/$tag" -name '*.apk' | wc -l) .apk(s)"
    fetched=$((fetched + 1))
done
echo "Fetched the APKs of $fetched release(s)"
