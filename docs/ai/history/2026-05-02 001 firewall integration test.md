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

Third follow-up: after presenting three architectural options (BPF-direct with caps; polkit-helper; per-session netns), the user picked **option 2 -- privileged helper via polkit/pkexec**.

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

## Option 2 implementation (current)

* New crate [`crates/shepherd-firewall-helper`](../../crates/shepherd-firewall-helper/): tiny, std-only privileged helper. Two subcommands: `apply-process` (the launch path) and `stop-scope` (kill path). `apply-process` validates every argument with strict allowlists (no shell metachars in CIDRs; uid must equal `$PKEXEC_UID`; `--uid 0` rejected; scope name must end in `.scope` and only contain unit-name-safe chars; env keys must match `[A-Za-z_][A-Za-z0-9_]*`), then `exec`s `systemd-run --scope --uid=N --gid=N --property=IPAddress*=...` against the *system* manager. The system manager has the kernel privileges needed to attach `cgroup_skb` BPF programs.
* New polkit policy at [`dist/polkit/org.shepherd.firewall.policy`](../../dist/polkit/org.shepherd.firewall.policy) declaring action `org.shepherd.firewall.apply-process` and annotating the helper path. Companion rule at [`dist/polkit/50-shepherd-firewall.rules`](../../dist/polkit/50-shepherd-firewall.rules) returns `polkit.Result.YES` for members of the `shepherd-firewall` unix group -- no password prompt at runtime.
* shepherdd's [`firewall_helper_argv_prefix()`](../../crates/shepherd-host-linux/src/process.rs) replaces the old `firewall_systemd_run_prefix`. The prefix is `pkexec --keep-cwd /usr/libexec/shepherd-firewall-helper apply-process --scope-name shepherd-<sid>.scope --uid=N --gid=N --default deny|allow [--allow R...] [--deny R...] [--cwd P] [--env K=V...] --` followed by the activity's argv. Env vars are passed as args because pkexec strips its parent environment. `--keep-cwd` preserves shepherdd's cwd so the activity's effective cwd matches the no-firewall path.
* `FirewallEnforcementStatus` now probes for (a) helper installed at expected path AND (b) `pkcheck --action-id org.shepherd.firewall.apply-process --process $$` exits 0 (i.e. polkit grants without prompt). Either missing -> Unsupported with a reason that points at the dev setup script.
* Env-var override `SHEPHERD_FIREWALL_HELPER` lets dev runs use `target/debug/shepherd-firewall-helper` instead of the installed path.
* New [`scripts/integration-tests/setup-firewall-dev.sh`](../../scripts/integration-tests/setup-firewall-dev.sh): one-time `sudo` install of helper + polkit files + creation of `shepherd-firewall` group with the calling user added to it. Production install (`shepherd install firewall`) is a follow-up -- the dev script is enough to validate the full flow.

## What works for which entry kind

* **Process kind**: fully supported via the helper. Filter is enforced by the system manager's BPF attach.
* **Snap / Flatpak**: the existing `apply_firewall_to_existing_scope` path goes to `systemctl --user --runtime set-property`, which still hits the user-manager-cannot-attach-BPF wall. The runtime check now logs a per-spawn warning and skips the no-op. Extending the helper with `libbpf-rs`/`aya` to attach to user-slice cgroups directly is a follow-up -- it requires a real BPF program in the helper, not just a systemd-run wrapper.
* **Steam**: still unsupported, same as before.

## Manual validation

After running `sudo ./scripts/integration-tests/setup-firewall-dev.sh` and re-logging so the `shepherd-firewall` group takes effect, the integration test ran end-to-end and the activity log reported:

```
[OK] allow 127.0.0.1:8080  (should connect)  ->  connected
[OK] deny  8.8.8.8:53      (should be blocked) -> blocked (TimeoutError: timed out)
PASS: firewall enforcement matches expectations.
EXIT_CODE: 0
```

## Bugs the test caught after wiring up option 2

Three more bugs surfaced once the helper was actually being invoked:

1. **`shepherd-http` was dropping `entry.firewall` on the floor.** The HTTP `POST /api/v1/sessions` handler in `crates/shepherd-http/src/handlers/sessions.rs` built `SpawnOptions` from scratch without copying `entry.firewall`. Only the IPC `Launch` path in `crates/shepherdd/src/main.rs` was populating it. Since the integration test launches via the HTTP API, the firewall config never reached the host adapter -- the helper wrapper wasn't even being invoked. Fixed by mirroring the IPC path's snapshot+populate pattern.

2. **Argv form mismatch between the prefix builder and the helper parser.** `firewall_helper_argv_prefix` was emitting `--uid=1000` (concatenated form) while the helper's argv parser expected `--uid` + a separate value (consistent with all the other args). The helper rejected with `unknown option '--uid=1000'` and exited 2 before ever calling `systemd-run`. Fixed to use the separated form and updated the unit test to assert it.

3. **ptyxis re-parents into a fresh `systemd-run --user --scope` per tab.** This was the hardest one. The journal showed two scopes for one launch: `shepherd-cd433d9d-...scope` (system, with our `IPAddressDeny=any`) ran `ptyxis`; then `ptyxis-spawn-094a5ef4-...scope` (user, no firewall) was created by ptyxis itself to host the activity script. The activity escaped our system-slice filter into a user-slice scope. Same problem will hit anything that reparents -- snap, flatpak, gnome-terminal. Switched the integration-tests entry to `command = "bash"` (no terminal wrapper) and added a comment in `config.example.toml` explaining the constraint. The agent-driven test reads from `dev-runtime/integration-tests.log` anyway, so dropping the visible terminal didn't cost anything.

## Cleanup bug also caught (separate from the firewall work)

The user reported that subsequent `./run-dev` instances were failing with `Could not connect to remote display: Connection refused`. Root cause: `scripts/lib/sway.sh::sway_start_nested` only set `trap sway_cleanup EXIT`. Bash's default action on `SIGTERM` is to terminate **without** running the EXIT trap, so the orchestrator's `kill -TERM "$RUN_DEV_PID"` was orphaning the nested sway. Sway's `wayland-1` socket file leaked, and any later shell whose `WAYLAND_DISPLAY` happened to be set to that name connected to the dead socket. Two fixes:

* Added an explicit `trap sway_handle_signal TERM INT HUP` in `sway_start_nested` that resets the trap to avoid recursion, calls `sway_cleanup`, then `exit`s.
* Added `sway_purge_stale_sockets` (called from both `sway_cleanup` and `sway_kill_existing`) that walks `/run/user/$UID/` and unlinks any `wayland-N` whose listener is dead and any `sway-ipc.<uid>.<pid>.sock` whose pid is gone. Self-heals from any prior unclean exit, not just my test's.

## Final state for option 2

Process-kind activities **with firewall config and a non-reparenting command** now actually have IP filtering enforced end-to-end. Verified by the integration test. snap/flatpak/steam still no-op (the existing `apply_firewall_to_existing_scope` path goes to the user manager which can't attach BPF) -- extending the helper with `aya`/`libbpf-rs` to attach `cgroup_skb` directly to those user-slice scopes is the natural follow-up.

## Production install integration

The firewall helper, polkit policy, and polkit rule are now installed by the main `shepherd install` flow:

* New `install_firewall` function in [`scripts/lib/install.sh`](../../scripts/lib/install.sh) installs the helper to `/usr/libexec/shepherd-firewall-helper`, the policy to `/usr/share/polkit-1/actions/`, and the rule to `/etc/polkit-1/rules.d/`. Creates the `shepherd-firewall` system group, adds the requested user to it, and reloads polkit. All of these are skipped when `DESTDIR` is set so packagers don't get host-mutating side effects in their build.
* Wired into `install_all`, so `shepherd install all --user kiosk` does the firewall step automatically (right after `install_bins`).
* Available as a standalone subcommand: `shepherd install firewall --user kiosk` for incremental installs / re-installs after rebuilds.
* New `--release` (default) and `--debug` flags select the binary source, so the same code path covers production *and* development.
* [`scripts/integration-tests/setup-firewall-dev.sh`](../../scripts/integration-tests/setup-firewall-dev.sh) is now a thin wrapper that builds the debug helper as the invoking user and `exec`s `shepherd install firewall --user $SUDO_USER --debug`. Production and dev share one install code path.

The helper's install path is hardcoded to `/usr/libexec/shepherd-firewall-helper` regardless of `--prefix`, because the polkit `.policy` file references the helper by absolute path and polkit's own directories aren't relocatable. Documented in `shepherd install help`.
