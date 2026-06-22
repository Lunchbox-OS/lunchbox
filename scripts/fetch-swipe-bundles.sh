#!/usr/bin/env bash
# Fetch and verify the pinned shepherd-swipe signed bundles for development and tests.
#
# Downloads the adult/child bundle release artifacts, checks them against the release
# SHA256SUMS, and extracts them into a gitignored cache so the keyboard's decode tests can
# find them via $SHEPHERD_SWIPE_BUNDLE_DIR. The decoder additionally verifies each bundle's
# minisign signature + per-file hashes on load; this script's checksum step just guards the
# download. Bundles are never committed to this repo.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

# Pin: keep in lockstep with the shepherd-swipe-core tag in the workspace Cargo.toml.
SWIPE_VERSION="v0.1.0"
RELEASE_BASE="https://git.armeafamily.com/albert/shepherd-swipe/releases/download/${SWIPE_VERSION}"
PROFILES=(adult child)

CACHE_DIR="${SHEPHERD_SWIPE_BUNDLE_DIR:-$REPO_ROOT/dev-runtime/swipe-bundles}"
DL_DIR="$CACHE_DIR/.downloads"

main() {
    command -v curl >/dev/null 2>&1 || die "curl is required"
    command -v sha256sum >/dev/null 2>&1 || die "sha256sum is required"

    mkdir -p "$DL_DIR"

    info "Fetching shepherd-swipe ${SWIPE_VERSION} bundles into ${CACHE_DIR}"

    local artifacts=("SHA256SUMS" "minisign-dev.pub")
    for profile in "${PROFILES[@]}"; do
        artifacts+=("shepherd-swipe-${profile}-${SWIPE_VERSION}.tar.gz")
    done

    for name in "${artifacts[@]}"; do
        info "Downloading ${name}"
        curl -fsSL --retry 3 -o "$DL_DIR/$name" "$RELEASE_BASE/$name" \
            || die "failed to download ${name}"
    done

    info "Verifying checksums"
    (cd "$DL_DIR" && sha256sum -c SHA256SUMS) || die "checksum verification failed"

    for profile in "${PROFILES[@]}"; do
        local tarball="$DL_DIR/shepherd-swipe-${profile}-${SWIPE_VERSION}.tar.gz"
        info "Extracting ${profile} bundle"
        rm -rf "${CACHE_DIR:?}/$profile"
        tar -xzf "$tarball" -C "$CACHE_DIR"
        [[ -f "$CACHE_DIR/$profile/metadata.toml" ]] \
            || die "extracted ${profile} bundle is missing metadata.toml"
    done

    success "Bundles ready in ${CACHE_DIR} (adult, child)"
    info "Run decode tests with: SHEPHERD_SWIPE_BUNDLE_DIR='${CACHE_DIR}' cargo test -p shepherd-keyboard-core -- --include-ignored"
}

main "$@"
