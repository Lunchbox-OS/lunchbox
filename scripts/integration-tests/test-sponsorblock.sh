#!/usr/bin/env bash
# SponsorBlock on-the-wire test (issue #159).
#
# Answers the one question the unit tests cannot: what does the player actually
# send? Two arms, both driving the real `shepherd-media` inside the headless dev
# session, both traced with `strace -e trace=connect`:
#
#   off  -- no `--sponsorblock-categories`, the default. No connection may be
#           made to any address `sponsor.ajay.app` resolves to, and no bucket
#           may appear in the cache. This is the assertion that "off" means the
#           device is silent rather than merely that it does not skip.
#   on   -- categories plus `--sponsorblock-api` pointing at a stand-in that
#           records what it is asked for. Proves the request is the privacy
#           endpoint (a four-character hash prefix, never the video id), that it
#           asks for every skippable category, and that the mirror override
#           really does redirect it away from the public instance.
#
# Deliberately does not assert that a segment was skipped: that needs a video
# whose submissions still exist, which is not a property this repo controls. See
# `docs/ai/history/2026-09-03 001 sponsorblock-scope.md` for that measurement.
#
# Do not tear the dev session down from another shell while this runs: shepherdd
# kills every `sleep` the user owns on its way out, including this script's, and
# the script then dies silently mid-arm. See the headless-dev skill's gotchas.
#
# Prerequisites:
#   - strace, python3
#   - the headless dev session's dependencies (`shepherd deps install agent`)
#   - a network (the "off" arm still plays a YouTube video, which is the point:
#     it proves the silence is about SponsorBlock and not about being offline)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

command -v strace >/dev/null 2>&1 || fail "strace not found on PATH"
command -v python3 >/dev/null 2>&1 || fail "python3 not found on PATH"

WORK="$(mktemp -d)"
PORT="${SPONSORBLOCK_TEST_PORT:-8391}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/shepherd/media/sponsorblock"
STARTED_SESSION=0
LISTENER=""

cleanup() {
    [[ -n "$LISTENER" ]] && kill "$LISTENER" 2>/dev/null || true
    pkill -x shepherd-media 2>/dev/null || true
    [[ "$STARTED_SESSION" == 1 ]] && ./scripts/shepherd dev stop >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

# A YouTube item is required: the database is YouTube-only, so nothing else
# would start a lookup even with the feature on, and the "off" arm would pass
# for the wrong reason.
cat > "$WORK/library.toml" <<'TOML'
schema_version = 1
library_id = "sponsorblock-wire-test"
title = "SponsorBlock wire test"

[[items]]
id = "item"
title = "A YouTube video"
kind = "video"
[[items.sources]]
platforms = ["*"]
uri = "https://www.youtube.com/watch?v=5Uui0VMqCOY"
TOML

# The stand-in instance. Records every path it is asked for and answers with one
# well-formed submission, so the "on" arm exercises a working lookup.
cat > "$WORK/stand_in.py" <<'PY'
import http.server, json, socketserver, sys

PORT, LOG = int(sys.argv[1]), sys.argv[2]
BODY = json.dumps([{"videoID": "5Uui0VMqCOY", "segments": [{
    "category": "sponsor", "actionType": "skip", "segment": [31.599, 100.331],
    "UUID": "u", "videoDuration": 793.741, "locked": 1, "votes": 18,
    "description": ""}]}]).encode()

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(LOG, "a") as f:
            f.write(self.path + "\n")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)
    def log_message(self, *a):
        pass

socketserver.TCPServer(("127.0.0.1", PORT), Handler).serve_forever()
PY

mapfile -t SB_IPS < <(getent ahosts sponsor.ajay.app | awk '{print $1}' | sort -u)
[[ ${#SB_IPS[@]} -gt 0 ]] || fail "could not resolve sponsor.ajay.app (no network?)"
echo "[test] sponsor.ajay.app resolves to: ${SB_IPS[*]}"

echo "[test] Building shepherd-media..."
cargo build -p shepherd-media

if [[ ! -f dev-runtime/headless/session.env ]]; then
    echo "[test] Booting the headless session..."
    ./scripts/shepherd dev headless --no-build >/dev/null || fail "could not boot the headless session"
    STARTED_SESSION=1
fi
set -a
# shellcheck disable=SC1091
. dev-runtime/headless/session.env
set +a

: > "$WORK/requests.log"
python3 "$WORK/stand_in.py" "$PORT" "$WORK/requests.log" &
LISTENER=$!
sleep 1

# Play for long enough that the lookup, which runs off the UI thread as playback
# starts, has landed either way.
arm() {
    local label="$1"; shift
    pkill -x shepherd-media 2>/dev/null || true
    sleep 3
    rm -rf "$CACHE"   # no cached bucket may stand in for a fetch
    strace -f -qq -e trace=connect -o "$WORK/strace-$label.txt" \
        ./target/debug/shepherd-media --log-level debug "$@" \
        play --library "$WORK/library.toml" --item item \
        > "$WORK/player-$label.log" 2>&1 &
    # `grep -q ... && break` would trip `set -e` on every iteration that has
    # not seen the line yet, and the script would exit here with no message.
    local _
    for _ in $(seq 1 90); do
        if grep -q STARTED_PLAYBACK "$WORK/player-$label.log"; then
            break
        fi
        sleep 1
    done
    grep -q STARTED_PLAYBACK "$WORK/player-$label.log" \
        || fail "$label: playback never started (see $WORK/player-$label.log)"
    sleep 15
    pkill -x shepherd-media 2>/dev/null || true
    sleep 2
}

count_sponsor_connects() {
    local file="$1" total=0 ip hits
    for ip in "${SB_IPS[@]}"; do
        hits=$(grep -c "\"$ip\"" "$file" || true)
        total=$(( total + hits ))
    done
    printf '%s' "$total"
}

echo "[test] Arm 1/2: the feature off (the default)..."
arm off

off_sponsor=$(count_sponsor_connects "$WORK/strace-off.txt")
off_standin=$(grep -c "htons($PORT)" "$WORK/strace-off.txt" || true)
off_requests=$(wc -l < "$WORK/requests.log")
# `find` on a missing directory exits non-zero, and `pipefail` would make that
# the whole script's exit status — an absent cache is the expected result here.
if [[ -d "$CACHE" ]]; then
    off_buckets=$(find "$CACHE" -type f | wc -l)
else
    off_buckets=0
fi
off_total=$(grep -c 'connect(' "$WORK/strace-off.txt" || true)

[[ "$off_sponsor" == 0 ]] || fail "off: connected to sponsor.ajay.app $off_sponsor time(s)"
[[ "$off_standin" == 0 ]] || fail "off: connected to the stand-in $off_standin time(s)"
[[ "$off_requests" == 0 ]] || fail "off: the stand-in was asked for $off_requests path(s)"
[[ "$off_buckets" == 0 ]] || fail "off: $off_buckets bucket(s) were cached"
# The arm has to have been doing *something*, or it proves nothing.
[[ "$off_total" -gt 0 ]] || fail "off: no connections at all — did the player run?"
echo "[test]   silent: 0 of $off_total connections went to SponsorBlock, 0 buckets cached"

echo "[test] Arm 2/2: the feature on, pointed at a stand-in..."
arm on --sponsorblock-categories sponsor --sponsorblock-api "http://127.0.0.1:$PORT"

on_sponsor=$(count_sponsor_connects "$WORK/strace-on.txt")
on_requests=$(wc -l < "$WORK/requests.log")
[[ "$on_requests" -ge 1 ]] || fail "on: the stand-in was never asked for anything"
[[ "$on_sponsor" == 0 ]] \
    || fail "on: --sponsorblock-api did not redirect; $on_sponsor connection(s) to sponsor.ajay.app"

path="$(head -1 "$WORK/requests.log")"
# The privacy endpoint is `/api/skipSegments/<4 hex>`; the exact-video endpoint
# would carry `videoID=`, which is a description of what somebody is watching.
[[ "$path" =~ ^/api/skipSegments/[0-9a-f]{4}\? ]] \
    || fail "on: expected a hash-prefix request, got: $path"
[[ "$path" != *"videoID"* ]] || fail "on: the request named the video: $path"
[[ "$path" == *"music_offtopic"* ]] \
    || fail "on: the request did not ask for every skippable category: $path"
[[ "$path" != *"poi_highlight"* ]] || fail "on: the request asked for a marker category: $path"

echo "[test]   requested: $path"
echo "PASS: with SponsorBlock off nothing reaches the service; with it on the request is the hash-prefix endpoint."
