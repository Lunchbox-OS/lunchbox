# 2026-05-02 — Manual firewall enforcement test

## Prompt

> Now add a test and script intended to be called manually from a host that
> *is* properly configured that actually tests the firewall. Then make sure
> it works correctly (this host should be properly configured).

## Context

The previous commit ("Add e2e firewall integration test") covered the
**wiring** but stopped short of actual BPF enforcement: the
`firewall_supported_path_invokes_helper_with_expected_argv` test stubs
`pkcheck`, `pkexec`, and `shepherd-firewall-helper`, so it asserts the
argv `shepherdd` *would* hand to the real chain — without ever attaching
a BPF filter. CI cannot do better; `ubuntu:25.10` containers don't have
`CAP_NET_ADMIN`, the system systemd manager, or a working polkit.

A proper, fully-set-up dev box (this one is) *can* run the real chain. The
manual test fills that gap.

## Approach

Three pieces, all under `scripts/integration-tests/` plus one new
`shepherd-e2e` test binary:

### 1. `crates/shepherd-e2e/tests/firewall_real.rs` — the test

`firewall_enforcement_with_real_helper`, `#[ignore]` and `multi_thread`,
follows the same pattern as the rest of `shepherd-e2e`:

- **Self-skip on misconfigured hosts.** First, check
  `/usr/libexec/shepherd-firewall-helper` exists and that
  `pkcheck --action-id org.shepherd.firewall.apply-process` succeeds for
  the calling pid. If either fails, print `[SKIP]` with the reason and
  return `Ok`. The same `cargo test --include-ignored` command therefore
  works in CI and on a dev box — CI just produces a no-op pass.
- **Pre-flight the deny target.** If `8.8.8.8:53` is unreachable from
  outside the firewall scope, the deny check would pass for the wrong
  reason (no internet vs. firewall blocked). Skip with a clear message.
- **Stand up an in-process loopback listener** on an ephemeral
  `127.0.0.1` port to be the allow target. The activity probes it
  *through* the firewall to confirm `127.0.0.0/8` is reachable. The
  listener accepts and immediately drops connections.
- **Run the activity** under the standard `TestHarness` (real `sway`,
  real `shepherdd`, isolated XDG dirs). The config has one entry,
  `firewall-probe`, with `[entries.firewall] default = "deny"` and
  loopback in `allow`. `[entries.kind.env]` carries the allow target,
  deny target, and probe log path into the activity's environment. The
  full chain (`pkexec → shepherd-firewall-helper → systemd-run --scope`)
  attaches the BPF cgroup program and execs the probe.
- **Read the probe log** and assert `allow=OPEN` and `deny=BLOCKED`.
  Stop the session via `DELETE /sessions/current` so the harness
  shutdown is clean.

### 2. `scripts/integration-tests/run-firewall-probe.sh` — the activity

Re-introduced under a new name (the old `run-activity.sh` was deleted in
the rebase). Runs *inside* the activity, i.e. inside the systemd scope
the helper just configured. Reads `SHEPHERD_FIREWALL_PROBE_*` env vars,
probes both targets via `bash`'s `/dev/tcp` (no python/curl/nc
dependency), and writes `allow=OPEN|BLOCKED\ndeny=OPEN|BLOCKED\n` to the
log path. Atomic publish via `mv` from a sibling `.tmp.$$`. Different
timeouts for allow vs. deny: an allowed target connects in ms; a
filtered target gets silently dropped and SYN-retries until we time out.

### 3. `scripts/integration-tests/test-firewall.sh` — the orchestrator

Re-introduced under the old name but re-purposed for the new flow.
Pre-checks preconditions (helper present, polkit grants, deny target
reachable) so the user sees actionable failures *before* `shepherdd` /
`sway` boot. Then `./scripts/shepherd build` and exec the cargo test
with `--include-ignored --test-threads=1 --nocapture`.

## Why a separate test binary

`crates/shepherd-e2e/tests/firewall_real.rs` is a **separate** integration
test binary (cargo creates one per file under `tests/`). The CI job runs
`cargo test -p shepherd-e2e -- --include-ignored --test-threads=1` which
*does* sweep up `firewall_real`, but the `skip_reason()` precondition makes
that a no-op pass. Keeping it in its own binary makes it cheap to invoke in
isolation locally:

```sh
cargo test -p shepherd-e2e --test firewall_real -- \
    --include-ignored --test-threads=1 --nocapture
```

…and keeps the wiring tests in `firewall.rs` separately runnable too.

## Validation on this host

Run end-to-end from the orchestrator:

```
[orchestrator] Verifying preconditions...
[orchestrator] Pre-flight: verifying deny target 8.8.8.8:53 is reachable from outside the firewall...
[orchestrator] Building shepherd binaries...
[orchestrator] Running cargo test -p shepherd-e2e --test firewall_real...

running 1 test
test firewall_enforcement_with_real_helper ... ---- probe log ----
allow=OPEN
deny=BLOCKED
-------------------
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.23s
```

`allow=OPEN` and `deny=BLOCKED` together prove the BPF address filter is
both attached and selective. If shepherdd ever regresses to the silent
no-op `systemd-run --user --scope` path, this test fails with `deny=OPEN`.

## Files

- `scripts/integration-tests/run-firewall-probe.sh` — inside-activity probe.
- `scripts/integration-tests/test-firewall.sh` — orchestrator (preconditions
  + build + cargo test).
- `crates/shepherd-e2e/tests/firewall_real.rs` — the test binary.

## Things this test deliberately does not cover

- **snap / flatpak / Steam.** The fix in `ce82b42` only enables enforcement
  for `process`-kind entries through the helper. Snap and Flatpak would
  still hit the "user manager can't attach BPF" wall via
  `apply_firewall_to_existing_scope`; a real check there is a follow-up.
- **IPv6.** The probe is IPv4-only; bash's `/dev/tcp/host/port` doesn't
  bracket-quote IPv6 cleanly and the test's allow target is
  `127.0.0.1:<port>`. Adding a separate `::1` probe and an IPv6 deny
  target is straightforward but unnecessary for proving the wiring.
- **Wider rule sets.** Just `default=deny` + loopback allow. That's the
  classic "kid kiosk" config; sweeping every CIDR/token shape goes in
  `crates/shepherd-config` validation tests, not e2e.
