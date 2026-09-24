#!/usr/bin/env bash
# Exercise publish-apt.sh end to end, offline, against a real apt.
#
# publish-apt.sh only runs for real on a release tag, where a mistake costs a
# broken `apt update` on every device. So this stands the whole arrangement up
# locally instead:
#
#   * a throwaway signing key, in its own GNUPGHOME, with a passphrase;
#   * tiny synthetic `lunchbox` .debs for two versions and both architectures,
#     each with the .asc the release would carry, laid out <tag>/<file> the way
#     fetch-release-debs.sh lays out GitHub releases;
#   * one HTTP server playing Cloudflare Pages -- static files, plus the
#     `_redirects` rules answered with a 302 -- and a second playing GitHub's
#     release downloads, so apt really does follow a cross-host redirect;
#   * apt-get with every Dir:: pointed into the scratch tree, so nothing on the
#     host is read or written.
#
# It checks both publish modes and that they agree: --rebuild for the first
# release, --previous to append the second, and a --rebuild of both producing
# the same index the append did. Then it checks the refusals, each of which
# stands between a mistake and every device: a changed release asset, a
# tampered published index, a signing key that is not repository.key, and a
# .deb with no valid .asc in a rebuild.
#
# Needs apt-utils (apt-ftparchive), gpg, python3 and curl. No root.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
publish="$here/publish-apt.sh"

work="$(mktemp -d)"
pids=()
cleanup() {
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    gpgconf --homedir "$work/gnupg" --kill gpg-agent 2>/dev/null || true
    gpgconf --homedir "$work/other-gnupg" --kill gpg-agent 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

pass=0
ok() { echo "  ok: $*"; pass=$((pass + 1)); }
fail() { echo "FAIL: $*" >&2; exit 1; }

# --- A throwaway key ----------------------------------------------------------

newkey() {
    local home="$1"
    install -d -m 700 "$home"
    printf 'allow-loopback-pinentry\n' > "$home/gpg-agent.conf"
    GNUPGHOME="$home" gpg --batch --quiet --pinentry-mode loopback \
        --passphrase "$PASSPHRASE" \
        --quick-generate-key 'Lunchbox Test Key <test@example.invalid>' ed25519 sign never
}
export PASSPHRASE="correct horse"
export GNUPGHOME="$work/gnupg"
export LUNCHBOX_APT_KEY_PASSPHRASE="$PASSPHRASE"
newkey "$GNUPGHOME"
gpg --armor --export > "$work/repository.key"
export APT_PUBLIC_KEY="$work/repository.key"

# --- Synthetic releases -------------------------------------------------------

# The two architectures publish-apt.sh indexes. The test treats the first as
# apt's native one and the second as foreign; nothing depends on which is which,
# or on the architecture of the machine running it.
arches=(amd64 arm64)
a1="${arches[0]}"
a2="${arches[1]}"

releases="$work/releases"
mkdeb() {
    local version="$1" arch="$2" marker="${3:-}"
    local root="$work/build/${version}_${arch}"
    mkdir -p "$root/DEBIAN" "$root/usr/share/doc/lunchbox" "$releases/v$version"
    cat > "$root/DEBIAN/control" <<EOF
Package: lunchbox
Version: $version
Architecture: $arch
Maintainer: Test <test@example.invalid>
Description: publish-apt.sh test package
EOF
    echo "lunchbox $version $arch $marker" > "$root/usr/share/doc/lunchbox/marker"
    local deb="$releases/v$version/lunchbox_${version}_${arch}.deb"
    dpkg-deb --root-owner-group --build "$root" "$deb" >/dev/null
    gpg --batch --yes --pinentry-mode loopback --passphrase "$PASSPHRASE" \
        --armor --detach-sign -o "$deb.asc" "$deb"
    rm -rf "$root"
}
for v in 0.1.0 0.2.0; do
    for a in "${arches[@]}"; do mkdeb "$v" "$a"; done
done

# --- Two servers: "Pages" and "GitHub releases" -------------------------------

cat > "$work/serve.py" <<'EOF'
import http.server, os, sys

root, portfile = sys.argv[1], sys.argv[2]

class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=root, **kw)

    def log_message(self, *a):
        pass

    def redirect(self):
        # Cloudflare Pages' _redirects, as far as publish-apt.sh uses it:
        # `from to status` lines, exact-path matches.
        try:
            with open(os.path.join(root, "_redirects")) as f:
                for line in f:
                    parts = line.split()
                    if len(parts) == 3 and parts[0] == self.path:
                        return parts[1], int(parts[2])
        except FileNotFoundError:
            pass
        return None

    def do_GET(self):
        r = self.redirect()
        if r:
            self.send_response(r[1])
            self.send_header("Location", r[0])
            self.end_headers()
            return
        super().do_GET()

    do_HEAD = do_GET

srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(portfile + ".tmp", "w") as f:
    f.write(str(srv.server_address[1]))
os.rename(portfile + ".tmp", portfile)
srv.serve_forever()
EOF

# Started directly rather than inside $(...): a command substitution waits for
# every process holding its stdout, and the server never exits.
serve() {
    local dir="$1" portfile="$work/$2.port"
    mkdir -p "$dir"
    python3 "$work/serve.py" "$dir" "$portfile" >"$work/$2.log" 2>&1 &
    pids+=("$!")
    for _ in $(seq 50); do [[ -f "$portfile" ]] && break; sleep 0.1; done
    [[ -f "$portfile" ]] || { cat "$work/$2.log" >&2; fail "server for $dir did not start"; }
}
url() { echo "http://127.0.0.1:$(cat "$work/$1.port")"; }
live="$work/live"
serve "$live" site
serve "$releases" assets
site_url="$(url site)"
APT_ASSET_BASE="$(url assets)"
export APT_ASSET_BASE
unset REPO

# A Pages deployment replaces the whole site.
deploy() {
    find "${live:?}" -mindepth 1 -delete
    cp -a "$1/." "$live/"
}

# --- apt, confined to the scratch tree ----------------------------------------

apt_root="$work/apt"
mkdir -p "$apt_root"/{etc/apt.conf.d,etc/preferences.d,etc/trusted.gpg.d,state/lists/partial,cache/archives/partial,log}
: > "$apt_root/state/status"
cat > "$apt_root/apt.conf" <<EOF
Dir::Etc::main "$apt_root/apt.conf";
Dir::Etc::parts "$apt_root/etc/apt.conf.d";
Dir::Etc::sourcelist "$apt_root/sources.list";
Dir::Etc::sourceparts "-";
Dir::Etc::preferences "$apt_root/preferences";
Dir::Etc::preferencesparts "$apt_root/etc/preferences.d";
Dir::Etc::trusted "$apt_root/trusted.gpg";
Dir::Etc::trustedparts "$apt_root/etc/trusted.gpg.d";
Dir::State "$apt_root/state";
Dir::State::status "$apt_root/state/status";
Dir::Cache "$apt_root/cache";
Dir::Log "$apt_root/log";
Debug::NoLocking "true";
APT::Architecture "$a1";
APT::Architectures { "$a1"; "$a2"; };
EOF
# .asc, as docs/INSTALL.md has it: apt reads an armoured key only under that
# extension, and ignores one named .key as an "unsupported filetype".
cp "$APT_PUBLIC_KEY" "$apt_root/lunchbox-os.asc"
echo "deb [signed-by=$apt_root/lunchbox-os.asc] $site_url stable main" > "$apt_root/sources.list"
apt() { APT_CONFIG="$apt_root/apt.conf" apt-get -q "$@"; }

packages() { cat "$1/dists/stable/main/binary-$2/Packages"; }
count() { packages "$1" "$2" | grep -c '^Package:' || true; }
sha() { sha256sum "$1" | cut -d' ' -f1; }
expect_fail() {
    local what="$1"; shift
    if "$@" >"$work/out.log" 2>&1; then
        cat "$work/out.log" >&2
        fail "$what: publish-apt.sh succeeded but should have refused"
    fi
    grep -q "::error::" "$work/out.log" || { cat "$work/out.log" >&2; fail "$what: failed without an ::error::"; }
    ok "$what refused: $(grep -m1 '::error::' "$work/out.log" | sed 's/::error:://' | cut -c1-90)"
}

# --- The first release, from nothing ------------------------------------------

echo "== rebuild: the first release"
mkdir -p "$work/first"
cp -a "$releases/v0.1.0" "$work/first/"
"$publish" --out "$work/site1" --rebuild "$work/first" >/dev/null
[[ "$(count "$work/site1" "$a1")" == 1 && "$(count "$work/site1" "$a2")" == 1 ]] \
    || fail "rebuild of v0.1.0 should index one package per arch"
grep -qx "Filename: pool/0.1.0/lunchbox_0.1.0_${a1}.deb" "$work/site1/dists/stable/main/binary-$a1/Packages" \
    || fail "Filename: should be pool/<version>/<file>"
ok "one package per architecture, at pool/<version>/<file>"
deploy "$work/site1"

# --- The second release, appended ---------------------------------------------

echo "== previous: append the second release"
"$publish" --out "$work/site2" --previous "$site_url" "$releases"/v0.2.0/*.deb >/dev/null
[[ "$(count "$work/site2" "$a1")" == 2 && "$(count "$work/site2" "$a2")" == 2 ]] \
    || fail "append should leave two packages per arch"
ok "both releases indexed for both architectures"
[[ "$(wc -l < "$work/site2/_redirects")" == 4 ]] || fail "expected four redirects"
grep -qx "/pool/0.1.0/lunchbox_0.1.0_${a2}.deb $APT_ASSET_BASE/v0.1.0/lunchbox_0.1.0_${a2}.deb 302" \
    "$work/site2/_redirects" || fail "redirect should point at the v<version> release asset"
ok "_redirects maps every pool path to its release asset"
cmp -s "$work/site2/repository.key" "$APT_PUBLIC_KEY" || fail "repository.key not deployed"
deploy "$work/site2"

echo "== apt, through the redirect"
apt update >"$work/apt.log" 2>&1 || { cat "$work/apt.log" >&2; fail "apt-get update"; }
grep -q "stable InRelease" "$work/apt.log" || { cat "$work/apt.log" >&2; fail "apt did not fetch InRelease"; }
ok "apt-get update accepted the signed index"
mkdir -p "$work/dl"
for spec in lunchbox=0.1.0 lunchbox=0.2.0 lunchbox:$a2=0.2.0; do
    (cd "$work/dl" && apt download "$spec" >"$work/apt.log" 2>&1) \
        || { cat "$work/apt.log" >&2; fail "apt-get download $spec"; }
done
for f in lunchbox_0.1.0_${a1}.deb lunchbox_0.2.0_${a1}.deb lunchbox_0.2.0_${a2}.deb; do
    v="${f#lunchbox_}"; v="${v%%_*}"
    [[ "$(sha "$work/dl/$f")" == "$(sha "$releases/v$v/$f")" ]] || fail "$f downloaded differs from the release asset"
done
ok "apt-get download fetched old and new versions, both arches, byte-identical"

# --- Re-running and rebuilding agree ------------------------------------------

echo "== idempotence and agreement"
"$publish" --out "$work/rerun" --previous "$site_url" "$releases"/v0.2.0/*.deb >"$work/out.log"
grep -q "0 package(s) added" "$work/out.log" || { cat "$work/out.log" >&2; fail "re-run should add nothing"; }
for a in "${arches[@]}"; do
    cmp -s "$work/rerun/dists/stable/main/binary-$a/Packages" "$work/site2/dists/stable/main/binary-$a/Packages" \
        || fail "re-run changed the $a index"
done
ok "a re-run of the same release adds nothing"

"$publish" --out "$work/full" --rebuild "$releases" >/dev/null
for a in "${arches[@]}"; do
    diff <(packages "$work/full" "$a" | sort) <(packages "$work/site2" "$a" | sort) >/dev/null \
        || fail "a full rebuild's $a index differs from the appended one"
done
cmp -s "$work/full/_redirects" "$work/site2/_redirects" || fail "a full rebuild's _redirects differs"
ok "a full --rebuild produces the same index as appending"

# --- Refusals -----------------------------------------------------------------

echo "== refusals"
mkdir -p "$work/changed"
cp "$releases/v0.2.0/lunchbox_0.2.0_${a1}.deb" "$work/keep.deb"
mkdeb 0.2.0 "$a1" "rebuilt differently"
mv "$releases/v0.2.0/lunchbox_0.2.0_${a1}.deb" "$work/changed/"
mv "$work/keep.deb" "$releases/v0.2.0/lunchbox_0.2.0_${a1}.deb"
expect_fail "a changed release asset" \
    "$publish" --out "$work/x1" --previous "$site_url" "$work/changed/lunchbox_0.2.0_${a1}.deb"

echo "Package: evil" >> "$live/dists/stable/main/binary-$a1/Packages"
expect_fail "a tampered published Packages" \
    "$publish" --out "$work/x2" --previous "$site_url"
deploy "$work/site2"

sed -i 's/^Suite: stable/Suite: evil/' "$live/dists/stable/InRelease"
expect_fail "a tampered published InRelease" \
    "$publish" --out "$work/x3" --previous "$site_url"
deploy "$work/site2"

newkey "$work/other-gnupg"
expect_fail "a signing key that is not repository.key" \
    env GNUPGHOME="$work/other-gnupg" "$publish" --out "$work/x4" --previous "$site_url"

mkdir -p "$work/unsigned/v0.3.0"
cp "$releases/v0.2.0/lunchbox_0.2.0_${a2}.deb" "$work/unsigned/v0.3.0/"
expect_fail "a rebuild over a .deb with no .asc" \
    "$publish" --out "$work/x5" --rebuild "$work/unsigned"

echo "[OK] publish-apt.sh: $pass checks passed"
