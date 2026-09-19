#!/usr/bin/env bash
# Firewall enforcement test for the cgroup attach path (issue #151).
#
# Drives `cargo test -p lunchbox-e2e --test firewall_cgroup`, which creates a
# cgroup, has `lunchbox-firewall-helper apply-cgroup` attach the BPF program to
# it, and runs run-firewall-probe.sh inside it to check that an allowed target
# connects and a denied one does not.
#
# Unlike its siblings this needs no polkit grant, no systemd user session, no
# flatpak/snapd, and no internet -- but it does need root, because it creates
# cgroups and loads BPF. The test binary is built as the invoking user and only
# the run is elevated, so target/ does not end up root-owned.
#
# Prerequisites:
#   - cgroup v2 mounted at /sys/fs/cgroup (the default on Ubuntu 26.04)
#   - sudo
#   - a routable non-loopback IPv4 (the denied target is this host itself)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

command -v cargo >/dev/null 2>&1 || fail "cargo not found on PATH"
[[ -e /sys/fs/cgroup/cgroup.controllers ]] \
    || fail "cgroup v2 is not mounted at /sys/fs/cgroup"

echo "[orchestrator] Building the helper and the test..."
cargo build -p lunchbox-firewall-helper
test_bin="$(cargo test -p lunchbox-e2e --test firewall_cgroup --no-run \
    --message-format=json 2>/dev/null \
    | python3 -c '
import json, sys
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if msg.get("reason") == "compiler-artifact" and msg.get("executable"):
        if msg["target"]["name"] == "firewall_cgroup":
            print(msg["executable"])
')"
[[ -n "$test_bin" ]] || fail "could not find the built firewall_cgroup test binary"

echo "[orchestrator] Running as root: $test_bin"
# REQUIRED=1: report an unmet precondition instead of skipping quietly.
sudo env SHEPHERD_FIREWALL_CGROUP_REQUIRED=1 \
    "$test_bin" --include-ignored --test-threads=1 --nocapture
