#!/usr/bin/env bash
# Exercise publish-fdroid.sh and upload-fdroid.sh end to end, offline.
#
# Both only run for real on a release tag, where a mistake is an F-Droid
# repository no phone will refresh, or an APK the pin should have refused. So
# this stands the whole arrangement up locally instead:
#
#   * throwaway keys: one signing the test APKs (and pinned in a copy of
#     dist/fdroid/metadata), one that is not pinned, the index key, and an
#     index key devices do not trust;
#   * small real APKs for both application IDs and two versions, built with
#     aapt2 and apksigner, laid out <tag>/<file> with .sha256 sidecars the way
#     fetch-release-apks.sh lays out GitHub releases;
#   * one server playing R2: its S3 API, which checks each request's SigV4
#     signature the way S3 canonicalises it, and the bucket's public custom
#     domain, serving what was uploaded with the headers it was uploaded with.
#
# It checks both publish modes and that they agree: --rebuild for the first
# release, --previous to add the second, and a --rebuild of both producing the
# same index. It checks the upload order and caching headers the live
# repository depends on, and that the result verifies the way a client checks
# it. Then it checks the refusals, each of which stands between a mistake and
# every phone: a changed APK, an APK the pin does not allow, a wrong index key,
# a tampered published index, a missing or wrong .sha256, an upload over a
# published APK, and bad credentials.
#
# Needs fdroidserver and a JDK (keytool), the Android SDK's build-tools and a
# platform (ANDROID_SDK_ROOT, ANDROID_HOME, or /opt/android-sdk), python3 and
# curl. No network, no root.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
publish="$here/publish-fdroid.sh"
upload="$here/upload-fdroid.sh"
fetch_index="$here/fetch-fdroid-index.sh"

work="$(mktemp -d)"
pids=()
cleanup() {
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    rm -rf "$work"
}
trap cleanup EXIT

pass=0
ok() { echo "  ok: $*"; pass=$((pass + 1)); }
fail() { echo "FAIL: $*" >&2; exit 1; }

for cmd in fdroid keytool python3 curl; do
    command -v "$cmd" >/dev/null || fail "$cmd is required"
done
sdk="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-/opt/android-sdk}}"
build_tools="$(find "$sdk/build-tools" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort -V | tail -n1)"
platform="$(find "$sdk/platforms" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort -V | tail -n1)"
[[ -x "$build_tools/aapt2" && -x "$build_tools/apksigner" ]] || fail "no build-tools with aapt2 and apksigner under $sdk"
[[ -f "$platform/android.jar" ]] || fail "no platform android.jar under $sdk"

# --- Keys ---------------------------------------------------------------------

PASS=test-password
newkey() {
    keytool -genkeypair -noprompt -storetype PKCS12 -keystore "$1" -alias "$2" \
        -keyalg RSA -keysize 2048 -validity 30 -storepass "$PASS" \
        -dname "CN=$2, O=test, C=US" >/dev/null 2>&1 || fail "keytool could not make $1"
}
fingerprint() {
    keytool -list -v -keystore "$1" -storepass "$PASS" \
        | awk '/SHA256:/ { gsub(":", "", $2); print tolower($2); exit }'
}
newkey "$work/app.p12" app
newkey "$work/other-app.p12" app
newkey "$work/index.p12" fdroid-index
newkey "$work/other-index.p12" fdroid-index

export FDROID_KEYSTORE="$work/index.p12"
export FDROID_KEYSTORE_PASSWORD="$PASS"
FDROID_INDEX_FINGERPRINT="$(fingerprint "$work/index.p12")"
export FDROID_INDEX_FINGERPRINT

# The real listings, pinned to the test APK key instead of the release key.
# Anything else about them -- the icons, the anti-features -- is what ships.
export FDROID_METADATA="$work/metadata"
cp -RL "$repo_root/dist/fdroid/metadata" "$FDROID_METADATA"
app_fpr="$(fingerprint "$work/app.p12")"
sed -i -E "s/^(  - )[0-9a-f]{64}$/\1$app_fpr/" "$FDROID_METADATA"/*.yml
grep -q "$app_fpr" "$FDROID_METADATA/com.lunchboxos.companion.yml" || fail "could not re-pin the test metadata"

# --- APKs ---------------------------------------------------------------------

releases="$work/releases"
# mkapk APP VERSION [KEYSTORE] [LABEL] -> releases/v<VERSION>/lunchbox-<APP>_<VERSION>.apk
mkapk() {
    local app="$1" version="$2" keystore="${3:-$work/app.p12}" label="${4:-Lunchbox $1}"
    local build="$work/build/$app-$version" code
    IFS=. read -r major minor patch <<<"$version"
    code=$((major * 10000 + minor * 100 + patch))
    mkdir -p "$build" "$releases/v$version"
    cat > "$build/AndroidManifest.xml" <<EOF
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.lunchboxos.$app">
  <application android:label="$label" />
</manifest>
EOF
    "$build_tools/aapt2" link -o "$build/unsigned.apk" -I "$platform/android.jar" \
        --manifest "$build/AndroidManifest.xml" --min-sdk-version 24 --target-sdk-version 35 \
        --version-code "$code" --version-name "$version" >/dev/null
    local apk="$releases/v$version/lunchbox-${app}_$version.apk"
    # Quiet unless it fails: newer JDKs warn about apksigner's native library
    # on every run.
    "$build_tools/apksigner" sign --ks "$keystore" --ks-pass "pass:$PASS" \
        --out "$apk" "$build/unsigned.apk" >"$work/apksigner.log" 2>&1 \
        || { cat "$work/apksigner.log" >&2; fail "apksigner could not sign $apk"; }
    rm -f "$apk.idsig"
    (cd "$releases/v$version" && sha256sum "$(basename "$apk")" > "$(basename "$apk").sha256")
    rm -rf "$build"
}
for v in 0.6.0 0.7.0; do
    for app in companion media; do mkapk "$app" "$v"; done
done

# --- R2: its S3 API and its public custom domain ------------------------------

export FDROID_S3_ACCESS_KEY_ID=test-key-id
export FDROID_S3_SECRET_ACCESS_KEY=test-secret
export FDROID_BUCKET=test-bucket

cat > "$work/r2.py" <<'EOF'
import hashlib, hmac, http.server, json, os, sys, threading, urllib.parse

store, portdir, bucket, key_id, secret = sys.argv[1:6]
log = open(os.path.join(portdir, "puts.log"), "a", buffering=1)


def canonical_uri(raw):
    # S3 canonicalises the decoded path, URI-encoding each byte once. A client
    # that signed the path it sent without encoding it the same way fails here,
    # as it would against R2.
    return urllib.parse.quote(urllib.parse.unquote(raw), safe="/-_.~")


def verify(h):
    auth = h.headers.get("Authorization", "")
    if not auth.startswith("AWS4-HMAC-SHA256 "):
        return "no SigV4 Authorization"
    parts = dict(p.strip().split("=", 1) for p in auth[len("AWS4-HMAC-SHA256 "):].split(","))
    akid, date, region, service, term = parts["Credential"].split("/")
    if akid != key_id:
        return "unknown access key"
    signed = parts["SignedHeaders"].split(";")
    for name in h.headers.keys():
        if name.lower().startswith("x-amz-") and name.lower() not in signed:
            return f"{name} is not signed"
    for name in ("host", "x-amz-date", "x-amz-content-sha256"):
        if name not in signed:
            return f"{name} is not signed"
    payload = h.headers["x-amz-content-sha256"]
    raw, query = (h.path.split("?", 1) + [""])[:2]
    canonical = "\n".join([
        h.command,
        canonical_uri(raw),
        query,
        "".join(f"{n}:{' '.join(h.headers.get(n, '').split())}\n" for n in signed),
        ";".join(signed),
        payload,
    ])
    scope = f"{date}/{region}/{service}/{term}"
    to_sign = "\n".join([
        "AWS4-HMAC-SHA256", h.headers["x-amz-date"], scope,
        hashlib.sha256(canonical.encode()).hexdigest(),
    ])
    k = ("AWS4" + secret).encode()
    for part in (date, region, service, term):
        k = hmac.new(k, part.encode(), hashlib.sha256).digest()
    if not hmac.compare_digest(hmac.new(k, to_sign.encode(), hashlib.sha256).hexdigest(), parts["Signature"]):
        return "signature does not match"
    return None


def path_of(key):
    return os.path.join(store, key)


class Base(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def reply(self, code, body=b"", headers=()):
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def serve(self, key):
        f = path_of(key)
        if not key or not os.path.isfile(f) or not os.path.isfile(f + ".meta"):
            return self.reply(404, b"not found")
        meta = json.load(open(f + ".meta"))
        headers = [("Content-Type", meta["type"]), ("Cache-Control", meta["cache"]),
                   ("x-amz-meta-sha256", meta["sha256"])]
        self.reply(200, open(f, "rb").read(), headers)


class Api(Base):
    def key(self):
        raw = self.path.split("?", 1)[0]
        prefix = f"/{bucket}/"
        return urllib.parse.unquote(raw[len(prefix):]) if raw.startswith(prefix) else None

    def check(self):
        err = verify(self)
        if err:
            self.reply(403, err.encode())
        return err is None

    def do_HEAD(self):
        if self.check():
            self.serve(self.key() or "")

    do_GET = do_HEAD

    def do_PUT(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if not self.check():
            return
        if self.headers["x-amz-content-sha256"] != hashlib.sha256(body).hexdigest():
            return self.reply(400, b"XAmzContentSHA256Mismatch")
        key = self.key()
        if not key:
            return self.reply(404, b"no such bucket")
        f = path_of(key)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        open(f, "wb").write(body)
        json.dump({"type": self.headers.get("Content-Type", ""),
                   "cache": self.headers.get("Cache-Control", ""),
                   "sha256": self.headers.get("x-amz-meta-sha256", "")}, open(f + ".meta", "w"))
        log.write(key + "\n")
        self.reply(200)


class Public(Base):
    def do_GET(self):
        self.serve(urllib.parse.unquote(self.path.split("?", 1)[0]).lstrip("/"))

    do_HEAD = do_GET


for name, handler in (("api", Api), ("public", Public)):
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    with open(os.path.join(portdir, name + ".port.tmp"), "w") as f:
        f.write(str(srv.server_address[1]))
    os.rename(os.path.join(portdir, name + ".port.tmp"), os.path.join(portdir, name + ".port"))
    threading.Thread(target=srv.serve_forever, daemon=True).start()
threading.Event().wait()
EOF

store="$work/bucket"
mkdir -p "$store"
python3 "$work/r2.py" "$store" "$work" "$FDROID_BUCKET" \
    "$FDROID_S3_ACCESS_KEY_ID" "$FDROID_S3_SECRET_ACCESS_KEY" >"$work/r2.log" 2>&1 &
pids+=("$!")
for _ in $(seq 50); do [[ -f "$work/public.port" ]] && break; sleep 0.1; done
[[ -f "$work/public.port" ]] || { cat "$work/r2.log" >&2; fail "the R2 stand-in did not start"; }
FDROID_S3_ENDPOINT="http://127.0.0.1:$(cat "$work/api.port")"
export FDROID_S3_ENDPOINT
public="http://127.0.0.1:$(cat "$work/public.port")"
export FDROID_URL="$public/fdroid/repo"

# --- Helpers ------------------------------------------------------------------

quiet() { "$@" >"$work/out.log" 2>&1 || { cat "$work/out.log" >&2; fail "$* failed"; }; }
# "<file> <sha256>" for every APK an index lists, sorted.
versions() {
    python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
for p in d["packages"].values():
    for v in p["versions"].values():
        print(v["file"]["name"].lstrip("/"), v["file"]["sha256"])
' "$1" | sort
}
header() { curl -sSI "$public/$1" | awk -v h="$2:" 'tolower($1) == tolower(h) { $1 = ""; sub(/^ /, ""); print }' | tr -d '\r'; }
expect_fail() {
    local what="$1"; shift
    if "$@" >"$work/out.log" 2>&1; then
        cat "$work/out.log" >&2
        fail "$what: succeeded but should have refused"
    fi
    grep -q "::error::" "$work/out.log" || { cat "$work/out.log" >&2; fail "$what: failed without an ::error::"; }
    ok "$what refused: $(grep -m1 '::error::' "$work/out.log" | sed 's/::error:://' | cut -c1-90)"
}
puts() { cat "$work/puts.log" 2>/dev/null || true; }

# --- The first release, from nothing ------------------------------------------

echo "== rebuild: the first release"
mkdir -p "$work/first"
cp -a "$releases/v0.6.0" "$work/first/"
quiet "$publish" --out "$work/site1" --rebuild "$work/first"
[[ "$(versions "$work/site1/fdroid/repo/index-v2.json" | wc -l)" == 2 ]] \
    || fail "rebuild of v0.6.0 should index one APK per app"
[[ -f "$work/site1/fdroid/repo/lunchbox-companion_0.6.0.apk" ]] || fail "the APKs belong in the upload tree"
[[ ! -e "$work/site1/fdroid/repo/status" ]] || fail "status/ should not be uploaded"
ok "both apps indexed, laid out under fdroid/repo/"

quiet "$upload" "$work/site1"
quiet "$fetch_index" "$FDROID_URL" "$work/live1.json"
diff <(versions "$work/live1.json") <(versions "$work/site1/fdroid/repo/index-v2.json") >/dev/null \
    || fail "the live index differs from the one built"
ok "the uploaded repository verifies the way a client checks it"

[[ "$(header fdroid/repo/lunchbox-media_0.6.0.apk Cache-Control)" == "public, max-age=31536000, immutable" ]] \
    || fail "an APK should be cached as immutable"
[[ "$(header fdroid/repo/lunchbox-media_0.6.0.apk Content-Type)" == "application/vnd.android.package-archive" ]] \
    || fail "an APK should be served as an APK"
for f in entry.jar index-v2.json index.html; do
    [[ "$(header "fdroid/repo/$f" Cache-Control)" == "no-cache" ]] || fail "$f should be no-cache"
done
[[ "$(header fdroid/repo/index.html Content-Type)" == "text/html; charset=utf-8" ]] || fail "index.html should be HTML"
ok "APKs immutable, index files no-cache, content types set"

# fdroidserver names each listing icon with an = in it, which the S3 stand-in
# only accepts if curl signed it the way S3 canonicalises it.
icon="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(d["packages"]["com.lunchboxos.companion"]["metadata"]["icon"]["en-US"]["name"].lstrip("/"))
' "$work/site1/fdroid/repo/index-v2.json")"
[[ "$icon" == *=* ]] || fail "expected a listing icon name with an = in it, got $icon"
cmp -s <(curl -fsS "$public/fdroid/repo/$icon") "$work/site1/fdroid/repo/$icon" \
    || fail "the listing icon was not uploaded intact"
ok "listing icons uploaded under their = names"

# --- The second release, added ------------------------------------------------

echo "== previous: add the second release"
: > "$work/puts.log"
quiet "$publish" --out "$work/site2" --previous "$FDROID_URL" "$releases"/v0.7.0/*.apk
[[ "$(versions "$work/site2/fdroid/repo/index-v2.json" | wc -l)" == 4 ]] \
    || fail "adding v0.7.0 should leave two versions of each app"
ok "both releases indexed for both apps"

quiet "$upload" "$work/site2"
grep -q "2 new APK(s)" "$work/out.log" || { cat "$work/out.log" >&2; fail "only the two new APKs should be uploaded"; }
mapfile -t order < <(puts)
last_apk=-1
first_other=-1
for i in "${!order[@]}"; do
    case "${order[$i]}" in
        *.apk) last_apk=$i ;;
        *) [[ $first_other -ge 0 ]] || first_other=$i ;;
    esac
done
[[ $last_apk -ge 0 && $last_apk -lt $first_other ]] || fail "every APK must be uploaded before any index file: ${order[*]}"
n=${#order[@]}
[[ "${order[$((n - 2))]}" == fdroid/repo/entry.json && "${order[$((n - 1))]}" == fdroid/repo/entry.jar ]] \
    || fail "entry.json then entry.jar must be uploaded last: ${order[*]}"
ok "uploaded in order: new APKs, then the index, then entry.json and entry.jar"

quiet "$fetch_index" "$FDROID_URL" "$work/live2.json"
[[ "$(versions "$work/live2.json" | wc -l)" == 4 ]] || fail "the live index should list four APKs"
ok "the live index lists both releases"

# --- Re-running and rebuilding agree ------------------------------------------

echo "== idempotence and agreement"
: > "$work/puts.log"
quiet "$publish" --out "$work/rerun" --previous "$FDROID_URL" "$releases"/v0.7.0/*.apk
diff <(versions "$work/rerun/fdroid/repo/index-v2.json") <(versions "$work/live2.json") >/dev/null \
    || fail "a re-run changed the indexed APKs"
quiet "$upload" "$work/rerun"
! puts | grep -q '\.apk$' || fail "a re-run should upload no APK"
ok "a re-run of the same release changes nothing and uploads no APK"

quiet "$publish" --out "$work/full" --rebuild "$releases"
diff <(versions "$work/full/fdroid/repo/index-v2.json") <(versions "$work/live2.json") >/dev/null \
    || fail "a full rebuild indexes different APKs than adding did"
ok "a full --rebuild indexes the same APKs as adding"

# --- Refusals -----------------------------------------------------------------

echo "== refusals"
mkdir -p "$work/changed"
mv "$releases/v0.7.0/lunchbox-companion_0.7.0.apk" "$work/keep.apk"
mkapk companion 0.7.0 "$work/app.p12" "Rebuilt differently"
mv "$releases/v0.7.0/lunchbox-companion_0.7.0.apk" "$work/changed/"
mv "$work/keep.apk" "$releases/v0.7.0/lunchbox-companion_0.7.0.apk"
(cd "$releases/v0.7.0" && sha256sum lunchbox-companion_0.7.0.apk > lunchbox-companion_0.7.0.apk.sha256)
expect_fail "a changed APK" \
    "$publish" --out "$work/x1" --previous "$FDROID_URL" "$work/changed/lunchbox-companion_0.7.0.apk"

mkdir -p "$work/foreign"
mv "$releases/v0.7.0" "$work/keep-0.7.0"
mkapk media 0.7.0 "$work/other-app.p12"
mv "$releases/v0.7.0" "$work/foreign/"
mv "$work/keep-0.7.0" "$releases/v0.7.0"
expect_fail "an APK signed by a key the pin does not allow" \
    "$publish" --out "$work/x2" --rebuild "$work/foreign"

expect_fail "an index key devices do not trust" \
    env FDROID_KEYSTORE="$work/other-index.p12" "$publish" --out "$work/x3" --previous "$FDROID_URL"

cp "$store/fdroid/repo/index-v2.json" "$work/index-v2.keep"
sed -i 's/"Lunchbox Apps"/"Evil Apps"/' "$store/fdroid/repo/index-v2.json"
expect_fail "a tampered published index" \
    "$publish" --out "$work/x4" --previous "$FDROID_URL"
cp "$work/index-v2.keep" "$store/fdroid/repo/index-v2.json"

mkdir -p "$work/nosum/v0.6.0"
cp "$releases/v0.6.0/lunchbox-media_0.6.0.apk" "$work/nosum/v0.6.0/"
expect_fail "a rebuild over an APK with no .sha256" \
    "$publish" --out "$work/x5" --rebuild "$work/nosum"
echo "0000000000000000000000000000000000000000000000000000000000000000  lunchbox-media_0.6.0.apk" \
    > "$work/nosum/v0.6.0/lunchbox-media_0.6.0.apk.sha256"
expect_fail "a rebuild over an APK whose .sha256 does not match" \
    "$publish" --out "$work/x6" --rebuild "$work/nosum"

cp -a "$work/site2" "$work/overwrite"
cp "$work/changed/lunchbox-companion_0.7.0.apk" "$work/overwrite/fdroid/repo/"
: > "$work/puts.log"
expect_fail "an upload over a published APK" "$upload" "$work/overwrite"
[[ -z "$(puts)" ]] || fail "a refused upload must not have written anything: $(puts | tr '\n' ' ')"

expect_fail "an upload with the wrong secret" \
    env FDROID_S3_SECRET_ACCESS_KEY=wrong "$upload" "$work/site2"

echo "[OK] publish-fdroid.sh and upload-fdroid.sh: $pass checks passed"
