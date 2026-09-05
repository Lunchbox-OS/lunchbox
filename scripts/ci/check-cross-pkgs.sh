#!/usr/bin/env bash
# Keep scripts/deps/cross.pkgs in step with scripts/deps/build.pkgs.
#
# A cross build needs the target architecture's half of every development
# library the native build links against. Those two lists are separate files --
# cross.pkgs also carries the host-side toolchain, and a few of build.pkgs'
# -dev packages are deliberately host-only -- so nothing but this check stops
# them drifting apart.
#
# The failure it prevents is quiet and remote: someone adds a dependency, the
# native build goes green everywhere, and the cross job fails much later with a
# pkg-config error naming a library nobody connected to the commit.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

build_pkgs=scripts/deps/build.pkgs
cross_pkgs=scripts/deps/cross.pkgs

# build.pkgs entries that are host-only by design, with the reason. A -dev
# package listed here needs no target-architecture half.
declare -A host_only=(
    [llvm-20-dev]="host LLVM for clang; nothing links it for the target"
    [libpolly-20-dev]="ships with llvm-20-dev, same reason"
)

read_pkgs() {
    grep -v '^\s*#' "$1" | grep -v '^\s*$' | sed 's/#.*//' | tr -d '[:blank:]' \
        | grep -v '^$'
}

status=0

# Every -dev package the native build installs must have a target half.
while IFS= read -r pkg; do
    case "$pkg" in
        *-dev) ;;
        *) continue ;;
    esac
    if [[ -n "${host_only[$pkg]:-}" ]]; then
        continue
    fi
    if ! read_pkgs "$cross_pkgs" | grep -qx -- "$pkg:@ARCH@"; then
        printf 'ERROR: %s has %s but %s has no "%s:@ARCH@"\n' \
            "$build_pkgs" "$pkg" "$cross_pkgs" "$pkg"
        status=1
    fi
done < <(read_pkgs "$build_pkgs")

# ...and nothing in the target half is there without a native counterpart,
# which would mean the two builds link different libraries.
while IFS= read -r pkg; do
    case "$pkg" in
        *:@ARCH@) ;;
        *) continue ;;
    esac
    base="${pkg%:@ARCH@}"
    if ! read_pkgs "$build_pkgs" | grep -qx -- "$base"; then
        printf 'ERROR: %s has "%s" but %s does not install %s natively\n' \
            "$cross_pkgs" "$pkg" "$build_pkgs" "$base"
        status=1
    fi
done < <(read_pkgs "$cross_pkgs")

if [[ $status -ne 0 ]]; then
    cat >&2 <<'EOF'

scripts/deps/cross.pkgs must carry the target-architecture half of every
development library in scripts/deps/build.pkgs. Add the missing "<pkg>:@ARCH@"
line, or -- if the package is genuinely host-only -- record it with its reason
in the host_only map at the top of this script.
EOF
    exit 1
fi

echo "cross-pkgs: OK (cross.pkgs covers every -dev package in build.pkgs)"
