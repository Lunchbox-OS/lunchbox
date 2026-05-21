# 2026-05-02 — Firewall e2e test (CI-runnable)

## Prompt

> Rebase this branch atop `origin/main`. When you get to the commits that
> implement firewall tests, rewrite them to use the new e2e integration
> test for this so that they can run in CI

## Context

`origin/main` had just gained the `shepherd-e2e` crate (commit `41013d9`,
"Add end to end tests in CI, including Sway and launcher run"). It boots a
real headless Sway + a real `shepherdd` inside a per-test temp environment
and drives the daemon through its HTTP management API and IPC socket — all
under an unprivileged user, in an `ubuntu:25.10` container, suitable for
GitHub Actions.

The `u/albert/10/managed-chrome` branch already had:

* `0464f89` WIP firewall implementation (config schema, host-side wiring,
  example config).
* `6885f49` end-to-end firewall test as **two bash scripts** under
  `scripts/integration-tests/` (`test-firewall.sh` orchestrator +
  `run-activity.sh` inside-the-activity probe), driven by `./run-dev`. Per
  its own history doc, this test was "not suitable for the current GitHub
  Actions CI" — it required a real Wayland session to nest into.
* `9b4aeb3` make enforcement failures explicit (probe + `WARN`).
* `ae39bec` fix process-type firewall via a privileged helper +
  pkexec/polkit; also added `setup-firewall-dev.sh`.

## Approach

Drop the shell-script test and replace it with two `#[ignore]` tests in a
new `crates/shepherd-e2e/tests/firewall.rs`. Both run inside the standard
`shepherd-e2e` harness (real `shepherdd`, real `sway`, isolated temp
env), and both are CI-runnable as part of the existing `e2e` job.

CI cannot exercise actual BPF address filtering — that needs
`CAP_NET_ADMIN`, the *system* systemd manager, and a working polkit, none
of which are available in the `ubuntu:25.10` container. So the tests
target the **wiring** that `shepherdd` does on top of those primitives:

1. **`firewall_unsupported_path_runs_activity`** — sets
   `SHEPHERD_FIREWALL_HELPER` to a nonexistent path so
   `firewall_enforcement_status()` reports `Unsupported` (the probe checks
   file existence first). Configures an entry with `[entries.firewall]`
   and asserts the activity launches anyway. This is the regression guard
   against the silent-no-op bug that `9b4aeb3` was fixing — the daemon
   must log a `WARN` and fall through, not refuse the launch and not
   silently spawn a no-op `systemd-run --user --scope`.

2. **`firewall_supported_path_invokes_helper_with_expected_argv`** — drops
   three stub executables in a per-test temp dir and prepends it to
   `PATH` for `shepherdd`:
   * fake `pkcheck` that always exits 0 (probe says polkit grants),
   * fake `pkexec` that strips `--keep-cwd` and `exec`s the rest (so the
     chain runs unchanged in CI),
   * fake `shepherd-firewall-helper` that records its full argv (one
     element per line) to a log file then `exec`s the trailing
     `--`-delimited command.

   It then launches the entry and asserts the recorded argv contains
   exactly what `firewall_helper_argv_prefix` is supposed to build:
   `apply-process`, `--scope-name`, `--uid`, `--gid`, `--default deny`,
   `--allow 127.0.0.0/8`, `--allow ::1/128`, the `--` terminator, and
   the activity command (`/usr/bin/sleep 600`).

## Harness change

Added a single method, `HarnessBuilder::shepherdd_env(key, value)`, that
appends to a `Vec<(String, String)>` applied after the harness's standard
`env_clear()` + base env when spawning `shepherdd`. That's enough to
override `PATH` (for the stub binaries) and inject
`SHEPHERD_FIREWALL_HELPER`. No change to the existing `e2e.rs` tests.

## What's *not* in the e2e suite

* The actual BPF cgroup filter check from the old shell test (allow target
  reachable, deny target blocked). That requires the real helper +
  `systemctl set-property` against the system manager. The fake helper
  records argv and execs the activity straight through — there is no
  filter applied. Validating real enforcement still belongs to a manual
  run on a developer machine after `setup-firewall-dev.sh`.
* The `integration-tests` `[[entries]]` block previously added to
  `config.example.toml`. It only made sense alongside the bash scripts
  it pointed at and would have been a dead reference.

## Files changed in this commit

* `crates/shepherd-e2e/src/lib.rs` — `shepherdd_env` builder method +
  apply loop in `TestHarness::start`.
* `crates/shepherd-e2e/tests/firewall.rs` — the two ignored tests.
* `scripts/integration-tests/setup-firewall-dev.sh` — comment update
  (removed reference to the deleted `test-firewall.sh`; pointed at the
  e2e suite).

## Running

```sh
./scripts/shepherd deps install run
./scripts/shepherd deps install test
./scripts/shepherd build
cargo test -p shepherd-e2e -- --include-ignored --test-threads=1
```

Both tests are gated `#[ignore]` so a plain `cargo test --all-targets`
skips them, matching the convention from the rest of `shepherd-e2e`.
