#!/usr/bin/env bash
# Create the GitHub release for TAG with every asset attached.
#
# `gh release create` with files creates a draft, uploads to it, and only then
# publishes, so a release is never public with some of its assets missing.
# That matters more than it used to: the apt index points at these assets, and
# a download that 404s fails the install even though the index verifies.
#
# Re-runnable. If the release already exists:
#   * as a draft (a previous run died mid-upload, or someone drafted it by
#     hand): upload with --clobber, since nobody can have downloaded a draft's
#     assets, then publish it;
#   * published, with every asset already attached: nothing to do. That is a
#     re-run after a later step -- the apt deploy -- failed;
#   * published and missing anything: fail. Release immutability is on, so a
#     published release can never gain an asset; the guard job's
#     check-release-tag.sh normally refuses this before anything builds.
#
# Tags with a -suffix (v0.6.0-rc1) become prereleases, which the apt index
# skips and GitHub does not mark as latest.
#
# Reads from the environment:
#   GH_TOKEN  a token that can write releases (the job's GITHUB_TOKEN with
#             contents: write)
#   REPO      owner/repo
#
# Usage: publish-release.sh TAG FILE [FILE ...]

set -euo pipefail

die() {
    echo "::error::$*" >&2
    exit 1
}

[[ $# -ge 2 ]] || die "usage: publish-release.sh TAG FILE [FILE ...]"
: "${REPO:?REPO (owner/repo) is required}"
tag="$1"
shift
for f in "$@"; do
    [[ -f "$f" ]] || die "asset not found: $f"
done

prerelease=()
[[ "${tag#v}" == *-* ]] && prerelease=(--prerelease)

if ! state="$(gh release view "$tag" --repo "$REPO" --json isDraft,assets 2>/dev/null)"; then
    echo "Creating release $tag with $# assets"
    gh release create "$tag" "$@" --repo "$REPO" --verify-tag \
        --title "$tag" --generate-notes "${prerelease[@]}"
    exit 0
fi

if [[ "$(jq -r .isDraft <<<"$state")" == true ]]; then
    echo "Release $tag exists as a draft; completing it"
    gh release upload "$tag" "$@" --repo "$REPO" --clobber
    gh release edit "$tag" --repo "$REPO" --draft=false
    exit 0
fi

missing=()
for f in "$@"; do
    name="$(basename "$f")"
    jq -e --arg n "$name" '.assets | any(.name == $n)' <<<"$state" >/dev/null \
        || missing+=("$name")
done
if [[ ${#missing[@]} -gt 0 ]]; then
    die "Release $tag is already published without ${missing[*]}. Release immutability means it can never gain them, and the tag can never be reused: release the next version instead. See docs/release-signing.md."
fi
echo "Release $tag is already published with all $# assets; nothing to do"
