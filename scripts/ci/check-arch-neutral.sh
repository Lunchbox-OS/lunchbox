#!/usr/bin/env bash
# Guard the build & packaging path against amd64/x86-specific literals.
#
# shepherd-launcher builds native binaries and packages them for the *host*
# architecture: `shepherd package deb` derives the Debian arch from
# `dpkg --print-architecture` (see scripts/lib/package.sh). A hardcoded `amd64`
# or `x86_64` in that path silently mislabels — or outright breaks — a non-amd64
# build (e.g. the arm64 .deb), which is exactly the regression this check
# prevents. It runs on a plain runner with no build or container, so it stays
# cheap even though CI itself currently only executes on amd64.
#
# Rule: a line in a scanned file may not contain an x86/amd64 arch literal
# UNLESS it also names an arm arch — that marks it as an illustrative
# "amd64, arm64, …" comment rather than a hardcode.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# The architecture-neutral surface: the unified script system, its shared libs
# (which also ship inside the .deb as the packaged shepherd-admin CLI), the
# dependency manifests, and the CI helper scripts.
files=(scripts/shepherd scripts/shepherd-admin scripts/dev scripts/admin run-dev)
while IFS= read -r f; do files+=("$f"); done < <(
    ls scripts/lib/*.sh scripts/ci/*.sh scripts/deps/*.pkgs 2>/dev/null
)

# x86/amd64 literals that would break a non-amd64 build or package. Also matches
# x86_64 inside a cargo target triple (x86_64-unknown-linux-gnu).
forbidden='amd64|x86[_-]64'
# arm arch indicators that mark a line as illustrative rather than a hardcode.
arm='arm64|aarch64|armeabi|armv'

status=0
for f in "${files[@]}"; do
    [[ -f "$f" ]] || continue
    # Skip this checker itself: it necessarily spells out the forbidden literals.
    [[ "$f" == scripts/ci/check-arch-neutral.sh ]] && continue
    while IFS= read -r hit; do
        num="${hit%%:*}"
        content="${hit#*:}"
        # Exempt lines that also name an arm arch (e.g. "amd64, arm64, …").
        shopt -s nocasematch
        if [[ "$content" =~ $arm ]]; then
            shopt -u nocasematch
            continue
        fi
        shopt -u nocasematch
        printf 'ERROR: %s:%s has an amd64/x86-specific literal:\n    %s\n' \
            "$f" "$num" "${content#"${content%%[![:space:]]*}"}"
        status=1
    done < <(grep -nEi "$forbidden" "$f" || true)
done

if [[ $status -ne 0 ]]; then
    cat >&2 <<'EOF'

The build/packaging path must stay architecture-neutral so that
`shepherd package deb` produces a correct package on any host (amd64, arm64, …).
Derive the architecture at runtime — e.g. `dpkg --print-architecture` — instead
of hardcoding `amd64`/`x86_64`. If a mention is genuinely illustrative, name
both arches (e.g. "amd64, arm64, …") so it reads as an example, not a hardcode.
EOF
    exit 1
fi

echo "arch-neutral: OK (no amd64/x86-specific literals in the build/packaging path)"
