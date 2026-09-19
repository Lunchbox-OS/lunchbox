#!/usr/bin/env bash
# Guard the scripts against amd64/x86-specific literals.
#
# What this is for, now that CI cross-builds arm64: the arm64 build and package
# jobs prove the *--arch* path directly -- aarch64 binaries, `Architecture:
# arm64`, every file inside matching -- so a hardcoded triple or label on that
# path already fails a real build. Two things no CI job executes remain, and
# this lexical check is what covers them:
#
#   1. The shipped scripts' runtime behaviour on a device. scripts/lib/*.sh and
#      lunchbox-admin are installed by the .deb and run on the target machine;
#      CI only stages them. A `/usr/lib/x86_64-linux-gnu/...` path in admin.sh
#      would pass every job and break on an arm64 device.
#   2. The default (no --arch) path. Every job passes --arch on an amd64 runner.
#      check-default-arch.sh now tests that path behaviourally with a faked
#      host, which is stronger; this check still flags the literal earlier.
#
# It is lexical, so it is also weak: it sees the words amd64/x86_64, not
# assumptions that never spell them (`$(uname -m)-linux-gnu`, which is wrong on
# armhf), and not Rust, workflows, Dockerfiles or dist/.
#
# Rule: a line in a scanned file may not contain an x86/amd64 arch literal
# UNLESS it also names an arm arch -- that marks it as an illustrative
# "amd64, arm64, ..." comment rather than a hardcode.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# The architecture-neutral surface: the unified script system, its shared libs
# (which also ship inside the .deb as the packaged lunchbox-admin CLI), the
# dependency manifests, and the CI helper scripts.
files=(scripts/lunchbox scripts/lunchbox-admin scripts/dev scripts/admin run-dev)
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
    # Skip this checker, and the default-arch test, which exists to catch a
    # hardcoded amd64 and so has to describe one: both necessarily spell out the
    # forbidden literals, and neither is on the path they guard.
    [[ "$f" == scripts/ci/check-arch-neutral.sh ]] && continue
    [[ "$f" == scripts/ci/check-default-arch.sh ]] && continue
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

These scripts ship in the .deb and run on the target machine, and CI never runs
them on anything but an amd64 runner, so a literal here goes unnoticed until it
breaks a device (amd64, arm64, …).
Derive the architecture at runtime — e.g. `dpkg --print-architecture` — instead
of hardcoding `amd64`/`x86_64`. If a mention is genuinely illustrative, name
both arches (e.g. "amd64, arm64, …") so it reads as an example, not a hardcode.
EOF
    exit 1
fi

echo "arch-neutral: OK (no amd64/x86-specific literals in the scanned scripts)"
