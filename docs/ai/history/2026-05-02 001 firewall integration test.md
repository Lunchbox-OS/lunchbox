# 2026-05-02 -- Firewall integration test scaffolding

## Prompt

> Using the API, create an integration test for the firewall rules that:
> 1. Starts the environment in a nested environment with `./run-dev`
> 2. Manually enables a newly-created "Integration Tests" activity
> 3. Launches it
>
> For now, the "Integration Tests" activity should just
> * be a terminal window via the process type (you can just use pytxis for this like the Terminal activity)
> * validate the firewall rules configuration
> * keep the output on screen but also log it somewhere an agent can pick it up to run end-to-end

Follow-up in same session:

> Make it so that the activity has firewall rules, and the activity should validate that it can't send establish connections outside of those rules

Second follow-up in same session:

> Use this test to validate and fix the firewall

## Context

Branch: `u/albert/10/managed-chrome` (commit `0464f89` "WIP: Initial implementation of firewall rules"). Per-entry `firewall` config (default/allow/deny) was added to `config.example.toml` and to the policy schema, enforced via systemd `IPAddressAllow=`/`IPAddressDeny=` on session scopes. The management HTTP API on `:8080` already exposes overrides, sessions, and config reload (see `crates/shepherd-http/src/handlers/`).

## Approach

Two cooperating bash scripts plus one new activity entry:

* `scripts/integration-tests/run-activity.sh` -- runs *inside* the activity (and therefore inside the systemd scope with `IPAddressDeny=any` + `IPAddressAllow=127.0.0.0/8`/`::1/128`). `tee`s output to `dev-runtime/integration-tests.log`, then does two checks:
  1. **Static**: `target/debug/validate-config config.example.toml` (exercises `validate_firewall` in `crates/shepherd-config/src/validation.rs` for CIDR/token syntax).
  2. **Live**: a `python3 socket.create_connection` probe against an allow target (`127.0.0.1:8080`, the management API listening inside the nested env) and a deny target (`8.8.8.8:53`, Google DNS). Allow target must connect; deny target must fail. Only PASS if both behave as expected.
  Final line is `EXIT_CODE: <n>`, the sentinel the orchestrator polls for. Then sleeps so a human watching the terminal has time to read the result.
* `scripts/integration-tests/test-firewall.sh` -- the orchestrator. **Pre-flight**: `python3 socket.create_connection` to the deny target from outside the firewall scope -- if it isn't reachable here, the activity's "deny target was blocked" result would pass for the wrong reason (no internet vs. enforcement). Then backgrounds `./run-dev`, polls `GET /api/v1/health` until ready, `PUT /api/v1/overrides/integration-tests {"availability": true}` to manually enable, `POST /api/v1/sessions {"entry_id":"integration-tests"}` to launch, polls the log for the sentinel, then `DELETE /api/v1/sessions/current` and tears down sway in a trap.
* `config.example.toml` -- new `[[entries]]` block `id = "integration-tests"`, `disabled = true`, `kind.command = "ptyxis"`, `kind.args = ["-s", "--", "./scripts/integration-tests/run-activity.sh"]`, plus an `[entries.firewall]` section with `default = "deny"` and only loopback in `allow`. `-s` keeps ptyxis standalone so the launching process represents the session for its full lifetime.

For Process-kind entries, `crates/shepherd-host-linux/src/adapter.rs:353` wraps the spawn in `systemd-run --user --scope --property=IPAddressDeny=any --property=IPAddressAllow=...`, so the entire `ptyxis` -> `bash` -> `python3` tree inherits the BPF address filter -- which is what makes the live probe inside the activity meaningful.

The relative path `./scripts/integration-tests/run-activity.sh` works because `./run-dev` starts shepherdd with the repo root as its CWD and `ManagedProcess::spawn` inherits that CWD when `entries.kind.cwd` is unset. Even so, the activity script resolves `$REPO_ROOT` from `BASH_SOURCE` so absolute paths are used inside the script.

## Why a disabled entry + override

The user asked to "manually enable" the activity. `disabled = true` keeps it out of the normal launcher; `PUT /api/v1/overrides/<id> {"availability": true}` flips it on for today (`engine.rs:135` skips the `disabled` check when an enable-today override is set, see `test_enable_override_bypasses_config_disabled`). This proves the override + launch path end-to-end without polluting the production launcher UI.

## Limitations

* The harness needs a Wayland session to nest into; not suitable for the current GitHub Actions CI (`.github/workflows/ci.yml` runs `ubuntu:25.10` without a display).
* The deny target is hardcoded to `8.8.8.8:53`. Override is plumbed for the orchestrator (`SHEPHERD_INTEGRATION_DENY_TARGET`) and the activity script reads the same env var, but shepherdd does not currently propagate orchestrator-side env to the spawned activity, so an override has to be set in the activity's `entries.kind.env` (or in the script's defaults) to take effect there.
* The firewall enforcement test only covers Process-kind entries -- snap and flatpak go through `apply_firewall_to_existing_scope` (a small race window), and Steam isn't supported yet (see `crates/shepherd-host-linux/src/adapter.rs:386`).

## Bug found by the test

First end-to-end run was a clean FAIL:
```
[OK]    allow 127.0.0.1:8080  (should connect)  ->  connected
[WRONG] deny  8.8.8.8:53      (should be blocked) -> connected
```

Root cause -- proven, not guessed:

* `systemd-run --user --scope --property=IPAddressDeny=any --property=IPAddressAllow=127.0.0.0/8 -- bash` *creates the scope but attaches no BPF program*. The resulting cgroup's `cgroup.controllers` lists only `memory pids`, and `bpftool cgroup show` returns `Operation not permitted` -- there is no `cgroup_skb` hook.
* `systemctl show user@1000.service -p DelegateControllers` prints `cpu memory pids` -- no bpf delegation.
* `man systemd.resource-control` (systemd 257) lists `IPAddressDeny=` under "**not** supported for services running in per-user instances of the service manager." The user systemd manager lacks `CAP_NET_ADMIN`/`CAP_BPF`, so it cannot attach `cgroup_skb` programs.

Conclusion: the WIP `firewall_systemd_run_prefix` (which hardcodes `--user`) is a silent no-op for **all** shepherd installations -- both `./run-dev` and the production sway-launched daemon run as a regular user.

## Fix applied this round

Targeted, validated change: detect the unenforceable case at runtime and stop pretending. Big architectural decisions (how to actually get enforcement) are left to the user.

* New `FirewallEnforcementStatus` enum + cached `firewall_enforcement_status()` probe in [crates/shepherd-host-linux/src/process.rs](crates/shepherd-host-linux/src/process.rs). Probe = `geteuid() == 0` OR `CAP_NET_ADMIN` set in `/proc/self/status`'s `CapEff`.
* `init()` logs the result once at startup -- `INFO` if Supported, loud `WARN` (with full diagnosis text and the three architectural options) if Unsupported.
* The two firewall apply paths in [crates/shepherd-host-linux/src/adapter.rs](crates/shepherd-host-linux/src/adapter.rs) (Process-kind `systemd-run` wrapper, and snap/flatpak `apply_firewall_to_existing_scope`) check the status before applying, log a per-spawn warning when Unsupported, and skip the no-op call instead of issuing it.
* Two unit tests in `process::tests`: one that the probe doesn't panic, and -- critically -- one that asserts the probe reports `Unsupported` whenever the test runner is unprivileged. That second test is what stops the silent-no-op bug from regressing.
* The orchestrator at [scripts/integration-tests/test-firewall.sh](scripts/integration-tests/test-firewall.sh) now `grep`s the `run-dev.log` for firewall/CAP_NET_ADMIN/IPAddress lines on activity failure and prints them inline, so a developer running the test sees the diagnosis without having to dig.

After the fix, the integration test still fails (deny target still reachable -- enforcement requires admin choices we can't make in code alone) but the failure mode is now loud and actionable, with the daemon WARN line printed directly above the orchestrator's FAIL line.

## Architectural choices for actually enabling enforcement

Three options, in increasing order of code change but not necessarily of correctness:

1. **Run shepherdd from a system unit with `AmbientCapabilities=CAP_NET_ADMIN+CAP_BPF`** and switch the implementation to attach `cgroup_skb` BPF programs directly (libbpf or raw BPF syscalls), removing the dependency on systemd-run for filtering. Cleanest at runtime, biggest code change, requires installing a system unit during `shepherd install`.
2. **Privileged helper invoked via polkit / pkexec.** Helper does `systemctl set-property` against the system manager. Install ships a polkit rule that grants the kiosk user this action without password. Smaller code change. New runtime dependency on polkit.
3. **Per-session network namespace via `unshare -rUn`** -- works fully unprivileged, but changes the *semantics* of the rules: the activity gets its own loopback, so `127.0.0.0/8` in `allow` no longer means "the host's API." The integration test's allow-target probe would have to change. Rules outside loopback would need user-mode bridging (pasta / slirp4netns).
