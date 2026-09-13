#!/usr/bin/env bash
# Prove `shepherd package deb` targets the *host* when no --arch is given.
#
# Every CI and release job passes --arch explicitly, and the runner is amd64,
# so the default path -- the one a developer or a device takes with a plain
# `shepherd package deb` -- never runs in CI. A hardcoded `amd64` there passes
# every build and package job: the amd64 leg's host is amd64 anyway, and the
# arm64 leg overrides it. On an arm64 host that same line turns an unqualified
# `package deb` into a cross build towards amd64, and the result is internally
# consistent (`Architecture: amd64`, x86-64 binaries), so no label-vs-contents
# assertion would notice either. That was demonstrated on the M1 VM; see
# docs/ai/history/2026-09-13 002.
#
# So this fakes the host instead of needing one. A stub `dpkg` on PATH answers
# `--print-architecture` with an architecture the runner is not, and the real
# `package_deb` runs end to end -- argument parsing, the default, the
# host-vs-target decision in build_set_target, the control file and the .deb
# name -- with only the steps that need a Rust build stubbed out:
#
#   build_cargo     records the target triple it was asked for, builds nothing
#   install_system  stages nothing (it would copy binaries that do not exist)
#   is_root         true, so there is no fakeroot re-exec to lose the stubs
#
# dpkg-deb is the real one; it builds a small but genuine package, whose
# `Architecture:` and file name are what get checked.
#
# Two fake hosts, neither of them the runner's: arm64 is the case that matters
# (the device), and ppc64el makes sure a fix that hardcodes `arm64` instead does
# not pass. The explicit --arch path is not exercised here; the arm64 build and
# package jobs cover it with a real cross build.
#
# Needs bash, dpkg and dpkg-deb -- no Rust toolchain -- so it runs alongside
# check-arch-neutral.sh on the plain runner.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
repo_root="$PWD"

real_dpkg="$(command -v dpkg)" || { echo "ERROR: dpkg not found" >&2; exit 1; }
command -v dpkg-deb >/dev/null || { echo "ERROR: dpkg-deb not found" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir "$work/bin"
cat > "$work/bin/dpkg" <<STUB
#!/bin/sh
# Report a fake host architecture; hand everything else to the real dpkg.
if [ "\$1" = "--print-architecture" ]; then
    echo "\$FAKE_HOST_ARCH"
    exit 0
fi
exec "$real_dpkg" "\$@"
STUB
chmod +x "$work/bin/dpkg"

status=0

# check_case HOST LABEL [package deb args...]
check_case() {
    local host="$1" label="$2"
    shift 2
    local out="$work/out-$host-$label" log="$work/log-$host-$label"
    local triple_file="$work/triple-$host-$label"
    rm -f "$triple_file"

    if ! (
        export PATH="$work/bin:$PATH" FAKE_HOST_ARCH="$host"
        unset SHEPHERD_CARGO_TARGET CARGO_BUILD_TARGET
        # shellcheck source=../shepherd
        source "$repo_root/scripts/shepherd"
        # These override the real functions; package_deb calls them, which
        # the linter cannot see through the `source` above.
        # shellcheck disable=SC2329
        build_cargo() { printf '%s' "${SHEPHERD_CARGO_TARGET:-}" > "$triple_file"; }
        # shellcheck disable=SC2329
        install_system() { :; }
        # shellcheck disable=SC2329
        is_root() { return 0; }
        package_deb --out "$out" "$@"
    ) > "$log" 2>&1; then
        printf 'ERROR: [host %s, %s] package deb failed:\n' "$host" "$label"
        sed 's/^/    /' "$log"
        status=1
        return
    fi

    local problems=()
    local debs=("$out"/*.deb)
    if [[ ${#debs[@]} -ne 1 || ! -f "${debs[0]}" ]]; then
        problems+=("expected one .deb in $out, found: ${debs[*]}")
    else
        local name got_arch
        name="$(basename "${debs[0]}")"
        got_arch="$(dpkg-deb -f "${debs[0]}" Architecture)"
        [[ "$got_arch" == "$host" ]] ||
            problems+=("control says Architecture: $got_arch, expected $host")
        [[ "$name" == *"_${host}.deb" ]] ||
            problems+=("package is named $name, expected *_${host}.deb")
    fi

    if [[ ! -f "$triple_file" ]]; then
        problems+=("build_cargo was never called")
    elif [[ -s "$triple_file" ]]; then
        problems+=("built for target $(cat "$triple_file"); a host build sets no target")
    fi

    if [[ ${#problems[@]} -eq 0 ]]; then
        printf 'ok: [host %s, %s] %s, native build\n' "$host" "$label" "$name"
    else
        printf 'ERROR: [host %s, %s]\n' "$host" "$label"
        printf '    %s\n' "${problems[@]}"
        status=1
    fi
}

for host in arm64 ppc64el; do
    check_case "$host" "no --arch"
    check_case "$host" "--arch host" --arch "$host"
done

if [[ $status -ne 0 ]]; then
    cat >&2 <<'MSG'

`shepherd package deb` must default to the host architecture, and an --arch
naming the host must stay a native build. Derive the host at runtime with
`dpkg --print-architecture` rather than naming an architecture.
MSG
    exit 1
fi
echo "default-arch: OK"
