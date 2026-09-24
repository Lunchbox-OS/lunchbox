#!/usr/bin/env bash
# Refuse to build a release for a tag whose GitHub release was published by
# hand.
#
# The repository has release immutability on: once a release is published, its
# assets can never be added to or changed, its tag cannot move or be deleted,
# and even deleting the release does not free the tag name. release.yml
# therefore has to be the thing that publishes -- `gh release create` drafts the
# release, attaches every asset, and only then publishes it.
#
# A release published any other way -- "Draft a new release" -> "Publish" in
# the web UI, which is also a way to create the tag -- is sealed the moment it
# is published, empty. That happened to v0.6.0. Nothing can repair it, so this
# says so before the build spends twenty minutes arriving at the same place.
#
# A published release made *by this workflow* passes: that is a re-run after a
# later step (the apt deploy, say) failed, and publish-release.sh finds its
# assets already there. A draft also passes: publish-release.sh fills it and
# publishes it. No release at all is the normal case.
#
# Reads from the environment:
#   GH_TOKEN  a token that can read releases
#   REPO      owner/repo
#
# Usage: check-release-tag.sh TAG

set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: check-release-tag.sh TAG" >&2; exit 1; }
: "${REPO:?REPO (owner/repo) is required}"
tag="$1"

# The login GitHub records for a release created with the job's GITHUB_TOKEN.
WORKFLOW_AUTHOR="github-actions[bot]"

if ! state="$(gh release view "$tag" --repo "$REPO" --json isDraft,author,assets 2>/dev/null)"; then
    echo "No release for $tag yet; release.yml will create it."
    exit 0
fi

draft="$(jq -r .isDraft <<<"$state")"
author="$(jq -r .author.login <<<"$state")"
assets="$(jq '.assets | length' <<<"$state")"

if [[ "$draft" == true ]]; then
    echo "Release $tag exists as a draft (by $author); release.yml will attach the assets and publish it."
    exit 0
fi

if [[ "$author" == "$WORKFLOW_AUTHOR" ]]; then
    echo "Release $tag was published by this workflow ($assets assets); treating this as a re-run."
    exit 0
fi

cat >&2 <<MSG
::error::Release $tag was published by $author, not by this workflow, and cannot be completed.
Release immutability is on, so a published release's assets are sealed -- this
one has $assets -- and its tag can never be reused, even if the release is
deleted. Release the next version instead: bump VERSION, then push an annotated
tag with git, and let this workflow create the release. Do not create or
publish releases in the GitHub UI. See docs/release-signing.md.
MSG
exit 1
