#!/usr/bin/env bash
# Fetch a published F-Droid repository's index-v2.json and verify it the way
# the F-Droid client does: entry.jar must carry a valid JAR signature by the
# key whose fingerprint we publish, and index-v2.json must have the SHA-256
# that entry.jar signed.
#
# The verification is fdroidserver's own (index.download_repo_index_v2), not a
# reimplementation, so it accepts exactly what fdroidserver's idea of a client
# would.
#
# Used by publish-fdroid.sh --previous, to build on the published index only
# after checking it is ours, and by release.yml's smoke test, to check what a
# phone will see.
#
# Reads from the environment:
#   FDROID_INDEX_FINGERPRINT  SHA-256 of the index signing certificate, hex
#                             (default: dist/fdroid/index-key.fingerprint)
#
# Usage: fetch-fdroid-index.sh REPO_URL OUT.json

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() {
    echo "::error::$*" >&2
    exit 1
}

[[ $# -eq 2 ]] || { echo "usage: fetch-fdroid-index.sh REPO_URL OUT.json" >&2; exit 1; }
url="${1%/}"
out="$2"

fingerprint="${FDROID_INDEX_FINGERPRINT:-}"
if [[ -z "$fingerprint" ]]; then
    file="$repo_root/dist/fdroid/index-key.fingerprint"
    [[ -f "$file" ]] || die "$file is missing: the F-Droid index key has not been made yet (see docs/release-signing.md)"
    fingerprint="$(tr -d '[:space:]' < "$file")"
fi
[[ "$fingerprint" =~ ^[0-9a-fA-F]{64}$ ]] || die "not a SHA-256 fingerprint: $fingerprint"

# fdroidserver downloads into the working directory's tmp/.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
out="$(realpath -m "$out")"

(
    cd "$work"
    python3 - "$url?fingerprint=$fingerprint" "$out" <<'EOF'
import json, logging, sys
from fdroidserver import common, index

logging.basicConfig(level=logging.WARNING)
common.config = common.read_config()
try:
    data, _ = index.download_repo_index_v2(sys.argv[1])
except Exception as e:
    sys.exit(f"::error::{sys.argv[1]}: {type(e).__name__}: {e}")
with open(sys.argv[2], "w") as f:
    json.dump(data, f)
EOF
)

echo "Verified the index at $url: $(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(sum(len(p["versions"]) for p in d["packages"].values()), "APK(s) in", len(d["packages"]), "app(s)")
' "$out")"
