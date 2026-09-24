#!/usr/bin/env bash
# Build the static apt repository served at https://apt.lunchbox-os.com.
#
# The output is a directory for `wrangler pages deploy`, and it holds only the
# index -- a few kilobytes:
#
#   dists/stable/InRelease                         clearsigned
#   dists/stable/Release, Release.gpg              detached-signed
#   dists/stable/main/binary-{amd64,arm64}/Packages{,.gz}
#   repository.key                                 the public half, from the repo
#   _redirects                                     /pool/<v>/<file>.deb -> GitHub
#
# The .deb files themselves are never uploaded. Each Packages stanza's
# `Filename:` is pool/<version>/<file>.deb, and _redirects answers that path
# with a 302 to the release asset of the same name, so Cloudflare Pages' 25 MiB
# per-file cap never applies to a package. apt follows the redirect and checks
# what it downloads against the SHA256 in the signed index, so the trust chain
# does not care where the bytes live. See
# docs/ai/history/2026-09-19 005 self-hosted-apt-repo (#204).md.
#
# Every non-prerelease release stays indexed. There are two ways to get there,
# and exactly one must be chosen:
#
#   --previous URL   Append. Fetch the index published at URL, verify its
#                    InRelease against repository.key and each Packages file
#                    against the SHA256 that InRelease signed, then append the
#                    stanzas for the .debs given. Costs the same every release.
#   --rebuild DIR    Start from nothing and index every *.deb under DIR (as
#                    fetch-release-debs.sh lays them out) plus the .debs given.
#                    Each one must carry a .asc that verifies against
#                    repository.key -- they are about to be re-signed into the
#                    index, so they have to be ours. This is the recovery path
#                    for a lost or wrong index, and the first publish.
#
# A .deb whose pool path is already indexed is skipped if its SHA256 matches
# (a re-run) and is an error if it does not: release assets are immutable, and
# an index that points at bytes other than the ones it describes fails every
# install of that version.
#
# Reads from the environment:
#   REPO                         owner/repo whose releases hold the .debs
#   APT_ASSET_BASE               where _redirects points (default:
#                                https://github.com/$REPO/releases/download);
#                                the tag is appended as v<version>
#   APT_PUBLIC_KEY               the key the index must verify against
#                                (default: dist/apt/repository.key)
#   LUNCHBOX_APT_KEY_PASSPHRASE  passphrase for the signing key, if it has one
#   GNUPGHOME                    the keyring holding the signing (sub)key
#
# Usage:
#   publish-apt.sh --out DIR (--previous URL | --rebuild DIR) [FILE.deb ...]

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

SUITE=stable
COMPONENT=main
ARCHES=(amd64 arm64)
# Cloudflare Pages allows 2,000 static redirects. Two per release, so this is
# the far future -- but it fails at publish time, so say so clearly.
MAX_REDIRECTS=2000

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

if [[ -z "${APT_ASSET_BASE:-}" ]]; then
    : "${REPO:?REPO (owner/repo) is required unless APT_ASSET_BASE is set}"
    APT_ASSET_BASE="https://github.com/${REPO}/releases/download"
fi
APT_ASSET_BASE="${APT_ASSET_BASE%/}"
public_key="${APT_PUBLIC_KEY:-$repo_root/dist/apt/repository.key}"
[[ -f "$public_key" ]] || die "public key not found: $public_key"

if [[ -e "$out" ]] && [[ -n "$(ls -A "$out")" ]]; then
    die "--out must be empty or absent: $out"
fi

for cmd in apt-ftparchive dpkg-deb gpg gpgv curl; do
    command -v "$cmd" >/dev/null || die "$cmd is required (apt-ftparchive is in apt-utils)"
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# gpgv wants a binary keyring, and the committed key is armoured.
keyring="$work/repository.gpg"
gpg --batch --quiet --dearmor < "$public_key" > "$keyring"

# Verify a signature with nothing but repository.key, the way apt does.
verify() {
    gpgv --keyring "$keyring" "$@" 2>"$work/gpgv.log" || {
        sed 's/^/  /' "$work/gpgv.log" >&2
        return 1
    }
}

site="$work/site"
dists="$site/dists/$SUITE"
for arch in "${ARCHES[@]}"; do
    mkdir -p "$dists/$COMPONENT/binary-$arch"
    : > "$dists/$COMPONENT/binary-$arch/Packages"
done

# --- The index to append to ---------------------------------------------------

if [[ -n "$previous" ]]; then
    echo "Fetching the published index from $previous"
    curl -fsSL --retry 3 -o "$work/InRelease.prev" "$previous/dists/$SUITE/InRelease"
    # --output writes only the signed text, so what is parsed below is exactly
    # what the signature covered -- never text appended around it.
    verify --output "$work/Release.prev" "$work/InRelease.prev" \
        || die "$previous/dists/$SUITE/InRelease does not verify against $public_key"

    for arch in "${ARCHES[@]}"; do
        rel="$COMPONENT/binary-$arch/Packages"
        want="$(awk -v f="$rel" '
            /^SHA256:/ { in_sha = 1; next }
            /^[^ ]/    { in_sha = 0 }
            in_sha && $3 == f { print $1 }
        ' "$work/Release.prev")"
        [[ -n "$want" ]] || die "the published Release has no SHA256 for $rel"
        curl -fsSL --retry 3 -o "$dists/$rel" "$previous/dists/$SUITE/$rel"
        got="$(sha256sum "$dists/$rel" | cut -d' ' -f1)"
        [[ "$got" == "$want" ]] \
            || die "$previous/dists/$SUITE/$rel has SHA256 $got, but InRelease signed $want"
        echo "  $rel: $(grep -c '^Package:' "$dists/$rel" || true) packages, verified"
    done
fi

# pool path -> SHA256 of everything already indexed.
declare -A indexed=()
for arch in "${ARCHES[@]}"; do
    while read -r file sha; do
        indexed["$file"]="$sha"
    done < <(awk '
        /^Filename: / { f = $2 }
        /^SHA256: /   { s = $2 }
        /^$/          { if (f != "") print f, s; f = s = "" }
        END           { if (f != "") print f, s }
    ' "$dists/$COMPONENT/binary-$arch/Packages")
done

# --- The .debs to add ---------------------------------------------------------

debs=("$@")
if [[ -n "$rebuild" ]]; then
    while IFS= read -r -d '' deb; do
        verify "$deb.asc" "$deb" 2>/dev/null \
            || die "$deb has no .asc that verifies against $public_key; refusing to re-sign it into the index"
        debs+=("$deb")
    done < <(find "$rebuild" -name '*.deb' -type f -print0 | sort -z)
fi

# Staged at their pool paths, so apt-ftparchive writes the right Filename:
# without any rewriting.
stage="$work/stage"
mkdir -p "$stage"
added=0
for deb in "${debs[@]}"; do
    [[ -f "$deb" ]] || die ".deb not found: $deb"
    arch="$(dpkg-deb -f "$deb" Architecture)"
    version="$(dpkg-deb -f "$deb" Version)"
    [[ " ${ARCHES[*]} " == *" $arch "* ]] || die "$deb: architecture $arch is not one of: ${ARCHES[*]}"

    file="pool/$version/$(basename "$deb")"
    sha="$(sha256sum "$deb" | cut -d' ' -f1)"
    if [[ -n "${indexed[$file]:-}" ]]; then
        [[ "${indexed[$file]}" == "$sha" ]] \
            || die "$file is already indexed with SHA256 ${indexed[$file]}, but $deb is $sha. Release assets must not change once published."
        echo "  $file: already indexed"
        continue
    fi
    indexed["$file"]="$sha"
    mkdir -p "$stage/pool/$version"
    cp "$deb" "$stage/$file"
    echo "  $file: adding"
    added=$((added + 1))
done

if [[ "$added" -gt 0 ]]; then
    for arch in "${ARCHES[@]}"; do
        packages="$dists/$COMPONENT/binary-$arch/Packages"
        new="$(cd "$stage" && apt-ftparchive --arch "$arch" packages pool)"
        [[ -n "$new" ]] || continue
        # Stanzas are blank-line separated; keep one between the old and new.
        if [[ -s "$packages" && -n "$(tail -n1 "$packages")" ]]; then
            echo >> "$packages"
        fi
        printf '%s\n' "$new" >> "$packages"
    done
fi

# --- Release, signatures, and the rest of the site ----------------------------

for arch in "${ARCHES[@]}"; do
    gzip -9nkf "$dists/$COMPONENT/binary-$arch/Packages"
done

# Written outside dists/ and then moved in: apt-ftparchive would otherwise hash
# the half-written Release into itself.
(
    cd "$dists"
    apt-ftparchive \
        -o APT::FTPArchive::Release::Origin=Lunchbox \
        -o APT::FTPArchive::Release::Label=Lunchbox \
        -o APT::FTPArchive::Release::Suite="$SUITE" \
        -o APT::FTPArchive::Release::Codename="$SUITE" \
        -o APT::FTPArchive::Release::Architectures="${ARCHES[*]}" \
        -o APT::FTPArchive::Release::Components="$COMPONENT" \
        -o APT::FTPArchive::Release::Description="Lunchbox - https://lunchbox-os.com" \
        release .
) > "$work/Release"
mv "$work/Release" "$dists/Release"

# The passphrase goes in on stdin rather than argv, where `ps` would show it.
sign() {
    gpg --batch --yes --pinentry-mode loopback --passphrase-fd 0 "$@" \
        <<<"${LUNCHBOX_APT_KEY_PASSPHRASE:-}"
}
sign --clearsign -o "$dists/InRelease" "$dists/Release"
sign --armor --detach-sign -o "$dists/Release.gpg" "$dists/Release"

# The signing key in GNUPGHOME and the committed public key are two separate
# things that have to agree, and nothing else checks that they do before every
# device's `apt update` would.
verify "$dists/InRelease" \
    || die "the new InRelease does not verify against $public_key -- is the signing key in GNUPGHOME the one it publishes?"
verify "$dists/Release.gpg" "$dists/Release" \
    || die "the new Release.gpg does not verify against $public_key"

cp "$public_key" "$site/repository.key"

# pool/<version>/<file> -> <base>/v<version>/<file>. The tag is derived rather
# than recorded: release.yml refuses a tag that is not v$(VERSION), and VERSION
# is what the package declares.
cat "$dists/$COMPONENT"/binary-*/Packages \
    | awk -v base="$APT_ASSET_BASE" '
        /^Filename: pool\// {
            split($2, p, "/")
            printf "/%s %s/v%s/%s 302\n", $2, base, p[2], p[3]
        }' \
    | sort -u > "$site/_redirects"
redirects="$(wc -l < "$site/_redirects")"
[[ "$redirects" -le "$MAX_REDIRECTS" ]] \
    || die "$redirects redirects exceeds Cloudflare Pages' limit of $MAX_REDIRECTS; compact _redirects to a placeholder rule (see note 005)"

mkdir -p "$out"
cp -a "$site/." "$out/"

echo "Wrote $out: $added package(s) added, ${#indexed[@]} indexed, $redirects redirect(s)"
