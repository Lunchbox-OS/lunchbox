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
