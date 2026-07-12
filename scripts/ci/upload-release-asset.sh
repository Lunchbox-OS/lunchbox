#!/usr/bin/env bash
# Upload file(s) — plus a generated <file>.sha256 for each — to a Forgejo
# release. Used by the parallel deb/apk jobs in .github/workflows/release.yml:
# Forgejo can't pass built files between jobs via actions/upload-artifact (it
# doesn't implement the v4 artifact protocol), so each build job publishes its
# own asset directly. Per-asset .sha256 sidecars replace a combined SHA256SUMS
# so no job needs the others' files.
#
# Reads from the environment (set by the workflow):
#   SERVER_URL     e.g. https://git.armeafamily.com
#   REPO           owner/repo (github.repository)
#   RELEASE_TOKEN  repo-write token
#   RELEASE_ID     numeric release id (from the create-release job)
#
# Usage: upload-release-asset.sh FILE [FILE ...]

set -euo pipefail

: "${SERVER_URL:?SERVER_URL is required}"
: "${REPO:?REPO is required}"
: "${RELEASE_TOKEN:?RELEASE_TOKEN is required}"
: "${RELEASE_ID:?RELEASE_ID is required}"

api="${SERVER_URL}/api/v1/repos/${REPO}"
auth="Authorization: token ${RELEASE_TOKEN}"

# POST one file as a named release attachment.
upload() {
    local file="$1" name
    name="$(basename "$file")"
    echo "Uploading $name"
    curl -fsS -X POST "${api}/releases/${RELEASE_ID}/assets?name=${name}" \
        -H "$auth" -F "attachment=@${file}" >/dev/null
}

for f in "$@"; do
    if [[ ! -f "$f" ]]; then
        echo "::error::asset not found: $f" >&2
        exit 1
    fi
    # sha256sum records the basename only (run from the file's dir) so the
    # sidecar verifies cleanly wherever it's downloaded.
    ( cd "$(dirname "$f")" && sha256sum "$(basename "$f")" > "$(basename "$f").sha256" )
    upload "$f"
    upload "$f.sha256"
done
