# shepherd-e2e

End-to-end integration tests that exercise the full shepherd stack against a
real, headless Sway compositor. Each test starts its own private Sway,
shepherdd, and (optionally) launcher/HUD processes inside an isolated temp
environment, then drives the daemon through its HTTP management API and IPC
socket.

The tests cover:

- Boot: shepherdd comes up, `POST /api/v1/rpc { "method": "health" }` reports ready, the IPC socket
  accepts a `Ping`, and `SIGTERM` triggers a clean shutdown.
- Activity launch and stop via the HTTP API: `POST /sessions`, the spawned
  child appears under `/proc`, then `DELETE /sessions/current` ends the
  session and reaps the child.
- Activity timeout: an entry with a short `max_run_seconds` expires on its
  own and a `SessionEnded` event with reason `expired` is observed on the
  SSE event stream.
- Session extension via `POST /sessions/current/extend`.
- Daily overrides via `PUT /overrides/{entry_id}` toggling availability.
- Config reload after rewriting the file on disk.

## Firewall tests

Four of the tests here are about the per-entry firewall, and they do not all
need the same host:

| test | covers | needs |
| --- | --- | --- |
| `firewall_cgroup` | `apply-cgroup`: BPF object loads, attaches, and filters | root + cgroup v2. No sway, polkit, flatpak or internet. **Runs in CI.** |
| `firewall_real` | the Process kind, via `systemd-run --scope` | the helper installed, polkit grant, sway. Runs in CI. |
| `firewall_real_flatpak` | the flatpak scope end to end | all of the above plus a flatpak built by `test-firewall-flatpak.sh` |
| `firewall_real_snap` | the snap scope end to end | all of the above plus a snap |

Where `/sys/fs/cgroup` is mounted read-only (containers, including CI's
docker-in-docker sidecar), `firewall_cgroup` mounts cgroup2 a second time to get
a writable view of the same hierarchy and creates its cgroups through that. The
helper is still handed the real `/sys/fs/cgroup` path, which is all it needs —
it only opens the cgroup read-only.

CI runs `firewall_cgroup` twice: once as the sidecar gives it, once under
`unshare -m` with `/sys/fs/cgroup` bind-remounted read-only, so both paths are
covered on every run. Whether the sidecar's cgroupfs is writable turns out to
vary between runs, so without the second invocation the fallback would go
untested until the day it was needed.

`firewall_cgroup` exists because of issue #151: `firewall_real` never loads the
helper's embedded BPF object (the Process path lets systemd attach the filter),
so a helper that could not parse that object passed CI while every firewalled
flatpak ran unfiltered. Run it with
`scripts/integration-tests/test-firewall-cgroup.sh`.

Each of these prints `[SKIP] <reason>` and passes when its host cannot run it —
including the plain E2E job, which runs the whole crate with `--include-ignored`
in an unprivileged container that can neither write cgroupfs nor mount cgroup2.
Set `SHEPHERD_FIREWALL_CGROUP_REQUIRED=1` for `firewall_cgroup` to turn that
skip into a failure — CI sets it, so an unmet precondition is reported rather
than read as a pass.

## Running locally

The harness needs `sway`, `dbus-daemon`, and the shepherd binaries (built
in debug mode). Install runtime deps and a few extras:

```sh
./scripts/shepherd deps install run
./scripts/shepherd deps install test
./scripts/shepherd build
cargo test -p shepherd-e2e -- --include-ignored --test-threads=1
```

The tests are marked `#[ignore]` so a plain `cargo test --all-targets` skips
them — they are slow (each spins up Sway) and require a Linux host with a
working wlroots headless backend. Run them with `--include-ignored` (and
`--test-threads=1` so the per-test sway/shepherdd processes don't fight
over PIDs and ports).

## In CI

The `e2e` job in `.github/workflows/ci.yml` installs the test deps, builds
the binaries, and runs `cargo test -p shepherd-e2e -- --include-ignored
--test-threads=1` inside an `ubuntu:25.10` container.
