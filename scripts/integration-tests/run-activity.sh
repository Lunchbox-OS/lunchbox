#!/usr/bin/env bash
# Inside-the-activity runner for the "Integration Tests" activity.
#
# Launched (via ptyxis) by shepherdd when the integration-tests entry is
# started. Shepherd wraps this process tree in a systemd scope with the
# entry's firewall rules applied (deny-by-default + loopback allow). This
# script asserts that:
#   1. The config still parses (cheap sanity check)
#   2. An ALLOWED destination (127.0.0.1, the management API) is reachable.
#   3. A DENIED destination (an external IP) is NOT reachable.
#
# Output is mirrored to a log file via `tee` so the orchestrator at
# scripts/integration-tests/test-firewall.sh can pick it up. The final line
# is `EXIT_CODE: <n>`, which is the sentinel the orchestrator polls for.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

LOG_FILE="${SHEPHERD_INTEGRATION_LOG:-$REPO_ROOT/dev-runtime/integration-tests.log}"
CONFIG_FILE="${SHEPHERD_INTEGRATION_CONFIG:-$REPO_ROOT/config.example.toml}"
VALIDATE_BIN="${SHEPHERD_VALIDATE_BIN:-$REPO_ROOT/target/debug/validate-config}"
HOLD_SECONDS="${SHEPHERD_INTEGRATION_HOLD_SECONDS:-60}"

# host:port pairs the activity will probe. Override via env if needed (e.g.
# for offline test runs you can point the deny target at any unroutable IP).
ALLOW_TARGET="${SHEPHERD_INTEGRATION_ALLOW_TARGET:-127.0.0.1:8080}"
DENY_TARGET="${SHEPHERD_INTEGRATION_DENY_TARGET:-8.8.8.8:53}"

mkdir -p "$(dirname "$LOG_FILE")"
: > "$LOG_FILE"

# Mirror everything to the log file as well as the terminal.
exec > >(tee -a "$LOG_FILE") 2>&1

echo "Integration Tests - firewall rules validation"
echo "============================================="
echo "Config file:     $CONFIG_FILE"
echo "Validator:       $VALIDATE_BIN"
echo "Log file:        $LOG_FILE"
echo "Allow target:    $ALLOW_TARGET (must be reachable)"
echo "Deny target:     $DENY_TARGET (must be blocked)"
echo "Started at:      $(date -Iseconds)"
echo

rc=0

# --- 1. Static config validation ------------------------------------------------
echo "[1/2] Validating firewall rules in config..."
if [[ ! -x "$VALIDATE_BIN" ]]; then
    echo "FAIL: validator binary not found or not executable: $VALIDATE_BIN"
    rc=1
elif [[ ! -f "$CONFIG_FILE" ]]; then
    echo "FAIL: config file not found: $CONFIG_FILE"
    rc=1
else
    echo "----- firewall sections in $CONFIG_FILE -----"
    grep -nE '^\[entries\.firewall\]|^[[:space:]]*default[[:space:]]*=|^[[:space:]]*allow[[:space:]]*=|^[[:space:]]*deny[[:space:]]*=' \
        "$CONFIG_FILE" || echo "(no firewall sections found)"
    echo "---------------------------------------------"
    if "$VALIDATE_BIN" "$CONFIG_FILE" >/dev/null; then
        echo "PASS: $CONFIG_FILE parses and firewall rules are well-formed."
    else
        validate_rc=$?
        echo "FAIL: validator exited with code $validate_rc."
        rc=$validate_rc
    fi
fi
echo

# --- 2. Live firewall enforcement check ----------------------------------------
echo "[2/2] Checking live firewall enforcement (running inside the activity scope)..."
SHEPHERD_INTEGRATION_ALLOW_TARGET="$ALLOW_TARGET" \
SHEPHERD_INTEGRATION_DENY_TARGET="$DENY_TARGET" \
python3 - <<'PY'
import os
import socket
import sys

def parse_target(s):
    host, port = s.rsplit(":", 1)
    return host, int(port)

def can_connect(host, port, timeout):
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True, None
    except OSError as e:
        return False, e

allow_host, allow_port = parse_target(os.environ["SHEPHERD_INTEGRATION_ALLOW_TARGET"])
deny_host, deny_port = parse_target(os.environ["SHEPHERD_INTEGRATION_DENY_TARGET"])

probes = [
    ("allow", allow_host, allow_port, True,  3.0),
    ("deny",  deny_host,  deny_port,  False, 4.0),
]

passed = True
for label, host, port, expect_ok, timeout in probes:
    actual_ok, err = can_connect(host, port, timeout)
    if actual_ok == expect_ok:
        verdict = "OK"
    else:
        verdict = "WRONG"
        passed = False
    expect_str = "should connect" if expect_ok else "should be blocked"
    if actual_ok:
        actual_str = "connected"
    else:
        actual_str = f"blocked ({type(err).__name__}: {err})"
    print(f"  [{verdict}] {label:<5} {host}:{port}  ({expect_str})  ->  {actual_str}")

if passed:
    print("PASS: firewall enforcement matches expectations.")
    sys.exit(0)
else:
    print("FAIL: firewall enforcement does not match expectations.")
    print("       (deny target reachable -> firewall is not blocking outbound traffic;")
    print("        allow target unreachable -> loopback is unexpectedly blocked.)")
    sys.exit(1)
PY
firewall_rc=$?
if [[ $rc -eq 0 ]]; then
    rc=$firewall_rc
fi
echo

# --- Summary -------------------------------------------------------------------
echo "Finished at:     $(date -Iseconds)"
if [[ $rc -eq 0 ]]; then
    echo "Overall:         PASS"
else
    echo "Overall:         FAIL"
fi
echo "EXIT_CODE: $rc"

# Keep the window open briefly so a human can read it. The orchestrator will
# normally stop the session as soon as it sees the EXIT_CODE: line, so this
# sleep is only relevant when the activity is launched manually.
sleep "$HOLD_SECONDS" || true

exit "$rc"
