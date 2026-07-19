#!/usr/bin/env bash
# Publish .deb file(s) to Forgejo's Debian package registry, so users can
# `apt update && apt install shepherd-launcher` and get `apt upgrade` on future
# releases. This is complementary to upload-release-asset.sh: the release still
# carries the .deb + .sha256 as an offline/manual download; this adds the hosted
# apt-repository path.
#
# Each .deb is a single authenticated PUT. Forgejo reads the control metadata
# (Package/Version/Architecture/Depends) out of the .deb and builds the pool +
# signed index itself — we don't sign anything; Forgejo signs the repository
# metadata with a key it auto-provisions (served at .../debian/repository.key).
#
# Re-uploading an already-present {package,version,arch} returns 409 Conflict;
# release jobs are re-runnable, so we treat 409 as success (idempotent).
#
# See docs/ai/history/2026-07-19 002 forgejo-apt-repository.md for the design.
#
# Reads from the environment (set by the workflow):
#   SERVER_URL     e.g. https://git.armeafamily.com
#   OWNER          package owner (github.repository_owner) — packages are owned
#                  at the user/org level, not per-repo
#   PACKAGE_TOKEN  token with the write:package scope
#   APT_DIST       distribution label (default: stable)
#   APT_COMPONENT  component label (default: main)
#
# Usage: publish-apt.sh FILE.deb [FILE.deb ...]

set -euo pipefail

: "${SERVER_URL:?SERVER_URL is required}"
: "${OWNER:?OWNER is required}"
: "${PACKAGE_TOKEN:?PACKAGE_TOKEN is required}"

dist="${APT_DIST:-stable}"
component="${APT_COMPONENT:-main}"

url="${SERVER_URL}/api/packages/${OWNER}/debian/pool/${dist}/${component}/upload"
auth="Authorization: token ${PACKAGE_TOKEN}"

for f in "$@"; do
    if [[ ! -f "$f" ]]; then
        echo "::error::.deb not found: $f" >&2
        exit 1
    fi
    name="$(basename "$f")"
    echo "Publishing $name to ${dist}/${component}"
    body="$(mktemp)"
    # Send the .deb as a raw body with an explicit octet-stream Content-Type.
    # curl's --data-binary otherwise defaults to application/x-www-form-urlencoded,
    # which Forgejo's upload handler treats as a form upload and runs through the
    # multipart parser — failing with a 500 "request Content-Type isn't
    # multipart/form-data". A non-form content type takes the raw-body path.
    code="$(curl -sS -o "$body" -w '%{http_code}' -X PUT "$url" \
        -H "$auth" -H 'Content-Type: application/octet-stream' \
        --data-binary @"$f")"
    case "$code" in
        201) echo "  published $name" ;;
        409) echo "  already published $name (idempotent)" ;;
        *)
            echo "::error::PUT $url -> HTTP $code" >&2
            # Surface the server's response so a failure is diagnosable from the
            # job log (e.g. an unsupported-compression 500 from the registry).
            sed 's/^/  /' "$body" >&2 || true
            rm -f "$body"
            exit 1
            ;;
    esac
    rm -f "$body"
done
