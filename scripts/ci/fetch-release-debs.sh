#!/usr/bin/env bash
# Download every published, non-prerelease release's .debs and their .asc
# sidecars into DIR/<tag>/, which is what `publish-apt.sh --rebuild DIR` reads.
#
# This is the apt repository's recovery path: the index is normally appended
# to, and this regenerates it from the release assets when the published copy
# is missing (the first publish) or wrong. Every release is downloaded, so it
# costs a few tens of MiB per release -- which is why it is not the default.
#
# Reads from the environment:
#   GH_TOKEN  a token that can read releases
#   REPO      owner/repo
#
# Usage: fetch-release-debs.sh DIR

set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: fetch-release-debs.sh DIR" >&2; exit 1; }
: "${REPO:?REPO (owner/repo) is required}"
dir="$1"
mkdir -p "$dir"

mapfile -t tags < <(gh release list --repo "$REPO" --limit 1000 \
    --exclude-drafts --exclude-pre-releases --json tagName --jq '.[].tagName')
echo "Fetching the .debs of ${#tags[@]} release(s)"
for tag in "${tags[@]}"; do
    # A release with no .deb has nothing to index, and `gh release download`
    # fails on it ("no assets to download") rather than downloading nothing.
    # v0.6.0 is one: published by hand, sealed empty by release immutability,
    # and never deletable in a way that frees its tag. Skipping it is right --
    # there was never a v0.6.0 package for apt to offer.
    debs="$(gh release view "$tag" --repo "$REPO" --json assets \
        --jq '[.assets[].name | select(endswith(".deb"))] | length')"
    if [[ "$debs" -eq 0 ]]; then
        echo "  $tag: no .deb assets; nothing to index"
        continue
    fi
    gh release download "$tag" --repo "$REPO" --dir "$dir/$tag" \
        --pattern '*.deb' --pattern '*.deb.asc'
    echo "  $tag: $(find "$dir/$tag" -name '*.deb' | wc -l) .deb(s)"
done
