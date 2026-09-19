#!/usr/bin/env bash
# Inside-the-activity firewall probe.
#
# Launched by lunchboxd as the body of the "firewall-probe" entry, under the
# transient systemd scope created by lunchbox-firewall-helper. Probes one
# allowed TCP target and one denied TCP target via bash's /dev/tcp, then
# writes the results atomically to a log file the orchestrator polls.
#
# Required env (set by the test via [entries.kind.env]):
#   SHEPHERD_FIREWALL_PROBE_LOG     log path (atomically rewritten)
#   SHEPHERD_FIREWALL_PROBE_ALLOW   host:port that should be reachable
#   SHEPHERD_FIREWALL_PROBE_DENY    host:port that should be blocked
# Optional:
#   SHEPHERD_FIREWALL_PROBE_HOLD_SECONDS  seconds to keep the activity
#                                         alive after writing the log
#                                         (default 120). The orchestrator
#                                         normally stops the activity well
#                                         before this.

set -uo pipefail

LOG="${SHEPHERD_FIREWALL_PROBE_LOG:?SHEPHERD_FIREWALL_PROBE_LOG required}"
ALLOW="${SHEPHERD_FIREWALL_PROBE_ALLOW:?SHEPHERD_FIREWALL_PROBE_ALLOW required}"
DENY="${SHEPHERD_FIREWALL_PROBE_DENY:?SHEPHERD_FIREWALL_PROBE_DENY required}"
HOLD="${SHEPHERD_FIREWALL_PROBE_HOLD_SECONDS:-120}"
# Snap/Flatpak entries have an inherent race: lunchboxd polls for the
# runtime's scope cgroup to appear, then invokes the helper to attach BPF.
# That can lag the activity's start by hundreds of ms. Wait so the probes
# happen *after* the BPF program is attached. Process-kind entries don't
# have the race (BPF is attached at scope creation), so default 0.
INITIAL_DELAY="${SHEPHERD_FIREWALL_PROBE_INITIAL_DELAY:-0}"

# Probe a host:port over TCP. Returns "OPEN" on successful connect, "BLOCKED"
# otherwise. Uses bash's /dev/tcp (so no python/curl/nc dependency).
# Different timeouts for allow vs deny: an allow target should connect or
# RST quickly; a denied target gets dropped silently and SYN-retries until
# we time out.
probe() {
    local hp="$1"
    local timeout_secs="${2:-3}"
    local host="${hp%:*}"
    local port="${hp##*:}"
    if timeout "$timeout_secs" bash -c "exec 3<>/dev/tcp/$host/$port" 2>/dev/null; then
        # Best-effort close; don't fail the script on it.
        exec 3<&- || true
        echo "OPEN"
    else
        echo "BLOCKED"
    fi
}

mkdir -p "$(dirname "$LOG")"

if [ "$INITIAL_DELAY" != "0" ]; then
    sleep "$INITIAL_DELAY"
fi

allow_result=$(probe "$ALLOW" 3)
deny_result=$(probe "$DENY" 6)

# Write atomically so a polling orchestrator never reads a partial file.
TMP="${LOG}.tmp.$$"
{
    printf 'allow=%s\n' "$allow_result"
    printf 'deny=%s\n'  "$deny_result"
} > "$TMP"
mv "$TMP" "$LOG"

# Stay alive so the orchestrator has time to read and stop us. The test
# DELETEs the session well before HOLD elapses; this is a fallback ceiling.
sleep "$HOLD"
