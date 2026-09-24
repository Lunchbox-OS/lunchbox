#!/usr/bin/env bash
# Give each release asset the sidecars it is published with:
#
#   <file>.sha256   integrity, for a scripted download to check cheaply
#   <file>.asc      .deb only: a detached signature by the archive signing key
#
# The .asc lets someone who downloads a .deb by hand check it against the same
# repository.key the apt instructions install, which is the path a repository
# signature does not cover. It is checked against the committed public key as
# soon as it is made, for the same reason publish-apt.sh checks the index: a
# CI secret that is not the key users trust should fail here, not in their
# hands. APKs get no .asc -- they are signed with the Android release key, which
# is what Android and F-Droid check. See docs/release-signing.md.
#
# Reads from the environment:
#   APT_PUBLIC_KEY               the key the .asc must verify against
#                                (default: dist/apt/repository.key)
#   LUNCHBOX_APT_KEY_PASSPHRASE  passphrase for the signing key, if it has one
#   GNUPGHOME                    the keyring holding the signing (sub)key
#
# Usage: sign-release-assets.sh FILE [FILE ...]

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
public_key="${APT_PUBLIC_KEY:-$repo_root/dist/apt/repository.key}"

die() {
    echo "::error::$*" >&2
    exit 1
}

[[ $# -gt 0 ]] || die "usage: sign-release-assets.sh FILE [FILE ...]"
[[ -f "$public_key" ]] || die "public key not found: $public_key"

keyring="$(mktemp)"
trap 'rm -f "$keyring"' EXIT
gpg --batch --quiet --dearmor < "$public_key" > "$keyring"

for f in "$@"; do
    [[ -f "$f" ]] || die "asset not found: $f"
    dir="$(dirname "$f")"
    name="$(basename "$f")"
    # Basename only, run from the file's directory, so the sidecar verifies
    # wherever it is downloaded to.
    (cd "$dir" && sha256sum "$name" > "$name.sha256")
    echo "  $name.sha256"

    [[ "$name" == *.deb ]] || continue
    # Passphrase on stdin, not argv, where `ps` would show it.
    gpg --batch --yes --pinentry-mode loopback --passphrase-fd 0 \
        --armor --detach-sign -o "$f.asc" "$f" <<<"${LUNCHBOX_APT_KEY_PASSPHRASE:-}"
    gpgv --keyring "$keyring" "$f.asc" "$f" 2>/dev/null \
        || die "$name.asc does not verify against $public_key -- is the signing key in GNUPGHOME the one it publishes?"
    echo "  $name.asc"
done
