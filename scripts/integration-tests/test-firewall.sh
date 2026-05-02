#!/usr/bin/env bash
# End-to-end firewall rules integration test.
#
# Boots a nested shepherd dev environment via ./run-dev, manually enables
# the "Integration Tests" activity through the management API, launches it,
# and verifies that the activity's firewall validation reports success.
#
# Requires a Wayland session to nest into (run-dev launches sway with the
# wayland backend). curl and jq are the only extra runtime dependencies.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

API_BASE="${SHEPHERD_API_BASE:-http://127.0.0.1:8080/api/v1}"
LOG_FILE="${SHEPHERD_INTEGRATION_LOG:-$REPO_ROOT/dev-runtime/integration-tests.log}"
RUN_DEV_LOG="$REPO_ROOT/dev-runtime/run-dev.log"
ENTRY_ID="integration-tests"
TIMEOUT_BOOT_SECONDS="${SHEPHERD_INTEGRATION_BOOT_TIMEOUT:-180}"
TIMEOUT_RESULT_SECONDS="${SHEPHERD_INTEGRATION_RESULT_TIMEOUT:-60}"
DENY_TARGET="${SHEPHERD_INTEGRATION_DENY_TARGET:-8.8.8.8:53}"

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "ERROR: required command '$1' not found on PATH" >&2
        exit 1
    fi
}
require_command curl
require_command jq
require_command python3

mkdir -p "$REPO_ROOT/dev-runtime"
rm -f "$LOG_FILE"

# Positive control: confirm the deny target is reachable from OUTSIDE the
# firewalled scope. Otherwise the activity's "deny target was blocked" result
# is meaningless (could be no-internet rather than enforcement).
echo "[orchestrator] Pre-flight: verifying deny target $DENY_TARGET is reachable from outside the firewall..."
if ! SHEPHERD_INTEGRATION_DENY_TARGET="$DENY_TARGET" python3 -c '
import os, socket, sys
host, port = os.environ["SHEPHERD_INTEGRATION_DENY_TARGET"].rsplit(":", 1)
try:
    socket.create_connection((host, int(port)), timeout=5).close()
except OSError as e:
    sys.exit(f"unreachable: {type(e).__name__}: {e}")
'; then
    echo "[orchestrator] FAIL: deny target $DENY_TARGET is not reachable from outside the firewall." >&2
    echo "  The activity's deny check would pass for the wrong reason. Either fix" >&2
    echo "  network connectivity or set SHEPHERD_INTEGRATION_DENY_TARGET to a" >&2
    echo "  host:port that IS reachable here but is OUTSIDE the entry's allow list." >&2
    exit 1
fi
echo "[orchestrator] Pre-flight OK: $DENY_TARGET is reachable; the deny check will be meaningful."

echo "[orchestrator] Starting nested dev environment via ./run-dev (log: $RUN_DEV_LOG)..."
./run-dev >"$RUN_DEV_LOG" 2>&1 &
RUN_DEV_PID=$!

cleanup() {
    local rc=$?
    echo "[orchestrator] Cleaning up nested sway (pid $RUN_DEV_PID)..."
    if [[ -n "${RUN_DEV_PID:-}" ]] && kill -0 "$RUN_DEV_PID" 2>/dev/null; then
        kill -TERM "$RUN_DEV_PID" 2>/dev/null || true
        for _ in $(seq 1 10); do
            kill -0 "$RUN_DEV_PID" 2>/dev/null || break
            sleep 1
        done
        kill -KILL "$RUN_DEV_PID" 2>/dev/null || true
    fi
    pkill -x shepherdd 2>/dev/null || true
    pkill -x ptyxis 2>/dev/null || true
    exit "$rc"
}
trap cleanup EXIT INT TERM

echo "[orchestrator] Waiting up to ${TIMEOUT_BOOT_SECONDS}s for shepherdd API at $API_BASE..."
api_ready=0
for i in $(seq 1 "$TIMEOUT_BOOT_SECONDS"); do
    if curl -sf "$API_BASE/health" >/dev/null 2>&1; then
        echo "[orchestrator] API is up after ${i}s."
        api_ready=1
        break
    fi
    sleep 1
done

if [[ "$api_ready" != "1" ]]; then
    echo "[orchestrator] FAIL: management API never became available." >&2
    echo "----- last 200 lines of run-dev log -----" >&2
    tail -200 "$RUN_DEV_LOG" >&2 || true
    echo "-----------------------------------------" >&2
    exit 1
fi

echo "[orchestrator] Enabling '$ENTRY_ID' via override (PUT /overrides/$ENTRY_ID)..."
if ! curl -sfS -X PUT \
        -H "Content-Type: application/json" \
        -d '{"availability": true}' \
        "$API_BASE/overrides/$ENTRY_ID" >/dev/null; then
    echo "[orchestrator] FAIL: could not enable '$ENTRY_ID' via API." >&2
    exit 1
fi

echo "[orchestrator] Launching '$ENTRY_ID' (POST /sessions)..."
launch_response=$(curl -sfS -X POST \
    -H "Content-Type: application/json" \
    -d "{\"entry_id\": \"$ENTRY_ID\"}" \
    "$API_BASE/sessions")
result=$(printf '%s' "$launch_response" | jq -r '.result // empty')
if [[ "$result" != "approved" ]]; then
    echo "[orchestrator] FAIL: launch was not approved: $launch_response" >&2
    exit 1
fi
echo "[orchestrator] Launch approved: $launch_response"

echo "[orchestrator] Waiting up to ${TIMEOUT_RESULT_SECONDS}s for activity result in $LOG_FILE..."
exit_code=""
for _ in $(seq 1 "$TIMEOUT_RESULT_SECONDS"); do
    if [[ -f "$LOG_FILE" ]] && grep -q '^EXIT_CODE: ' "$LOG_FILE"; then
        exit_code=$(grep '^EXIT_CODE: ' "$LOG_FILE" | tail -1 | awk '{print $2}')
        break
    fi
    sleep 1
done

# Stop the session so the activity (and its sleep) doesn't keep the nested
# environment busy.
curl -sfS -X DELETE "$API_BASE/sessions/current" >/dev/null 2>&1 || true

echo
echo "===== activity log ($LOG_FILE) ====="
cat "$LOG_FILE" 2>/dev/null || echo "(no log produced)"
echo "====================================="
echo

if [[ -z "$exit_code" ]]; then
    echo "[orchestrator] FAIL: activity did not finish within ${TIMEOUT_RESULT_SECONDS}s." >&2
    exit 1
fi

if [[ "$exit_code" != "0" ]]; then
    echo "[orchestrator] FAIL: activity reported EXIT_CODE: $exit_code." >&2
    exit "$exit_code"
fi

echo "[orchestrator] PASS: integration test succeeded."
