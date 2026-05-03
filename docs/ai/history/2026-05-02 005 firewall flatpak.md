# 2026-05-02 — Flatpak firewall enforcement (parity with Snap)

## Prompt

> Now do the Flatpak implementation

## What landed

Flatpak now enforces `[entries.firewall]` rules through the same chain
the Snap path uses (added in `5f3719a`):

```
shepherdd
  └─ ManagedProcess::spawn → flatpak run --env=K=V… <app-id>
  └─ tokio::spawn → wait_for_scope (poll cgroup hierarchy)
       └─ pkexec → /usr/libexec/shepherd-firewall-helper apply-cgroup …
            ─→ aya: load + populate maps from rule list
            ─→ raw bpf(BPF_PROG_ATTACH) on the runtime's scope cgroup
                  (`app-flatpak-<app_id>-<n>.scope` under
                   `…/user@<uid>.service/app.slice/`)
            ─→ exit; program lives until the cgroup dies
```

Verified end-to-end on a configured host: `allow=OPEN`,
`deny=BLOCKED`, BPF program attached at the flatpak's scope. Three
consecutive runs all pass (snap, process, flatpak all green together).

## Code changes

### `crates/shepherd-host-linux/src/adapter.rs`

`flatpak run` strips most environment variables before exec'ing the
sandboxed app. User-supplied `[entries.kind.env]` previously didn't
reach the app at all. Mirror the snap path's env behavior by emitting
`--env=KEY=VAL` flags for each entry between `flatpak run` and
`<app-id>`, sorted for determinism. The same `env` is still set on the
spawned `Command` (so `flatpak run` itself sees it for its own
purposes); the new flags just make sure the inner sandboxed app sees
them too.

### `crates/shepherd-e2e/tests/firewall_real_flatpak.rs` (new)

Manual `#[ignore]` test, same shape as `firewall_real_snap.rs`:

- Skip predicate covers helper presence, polkit grant, `flatpak` CLI,
  and the test app being installed (orchestrator below installs it).
- Pre-flight `8.8.8.8:53` reachable.
- In-process `127.0.0.1:0` listener as the allow target.
- Probe log under `/tmp/shepherd-fw-fp-…/probe.log` — manifest declares
  `--filesystem=/tmp` so the sandboxed app can write there. Default
  `--filesystem=host` does *not* expose `/tmp`; that's a sandbox
  carve-out for tmpfs roots.
- **`XDG_DATA_HOME` override is load-bearing.** The harness sets
  `XDG_DATA_HOME` to a tempdir for shepherdd hermeticity; `flatpak run`
  inherits that and looks for `--user`-installed apps at
  `$TEMPDIR/flatpak/app/<id>/…` instead of the user's real install.
  Test config sets `XDG_DATA_HOME = $HOME/.local/share` via
  `[entries.kind.env]` so the inner `flatpak` resolves the app
  correctly. (Yes, it then leaks into the sandboxed probe via
  `--env=`; harmless for our probe, mildly ugly in general. A cleaner
  long-term fix is to teach `adapter.rs` to set `XDG_DATA_HOME` only
  on the outer process and not pass it via `--env`. Not in this round.)

### `scripts/integration-tests/test-firewall-flatpak.sh` (new)

Mirrors `test-firewall-snap.sh`. Pre-checks:

- helper installed + polkit grants
- `flatpak` and `flatpak-builder` on PATH
- `org.freedesktop.{Platform,Sdk}//24.08` installed for the user
  (clear instructions if not — these are ~1.7 GB combined to download
  on first run)
- deny target reachable

…then materializes a tiny `org.shepherd.firewall.Probe` flatpak from a
literal manifest, builds + installs it via `flatpak-builder --user
--install --force-clean`, runs the cargo test, and uninstalls on exit.

## Why this took surprisingly few daemon changes

`apply_firewall_to_existing_scope` in `process.rs` already handled
flatpak: the scope-prefix pattern `app-flatpak-<app_id>-` is what the
`Flatpak` arm of `adapter.rs` passes to it, and the helper's
`apply-cgroup` subcommand is generic over cgroup paths. So the
"flatpak implementation" was really just (a) the env-passthrough fix
above and (b) test scaffolding.

## Things this does NOT cover

- **`flatpak install --system`.** The test installs the probe `--user`,
  which means the harness has to override `XDG_DATA_HOME` to find it.
  A `--system`-installed app would be in `/var/lib/flatpak/app/…` and
  not need that hack. Out of scope here; the test is manual anyway.
- **Steam.** Same architectural gap as before. Steam uses the Steam
  snap with its own scope discipline; not addressed.
- **Race window.** Same as Snap: shepherdd polls the cgroup hierarchy
  for the runtime-created scope, then attaches BPF after the fact.
  The probe's `INITIAL_DELAY=5` papers over this. Not a real bug as
  long as the activity hasn't already finished its first network
  attempts before BPF lands.
- **CI.** The flatpak test stays manual: the `flatpak install` step
  needs network access to flathub and ~1.7 GB of disk. Like
  `firewall_real_snap.rs` and `firewall_real.rs`, the test
  self-skips if its prerequisites aren't met, so a stock CI run is a
  no-op pass.

## Validation log

```
$ ./scripts/integration-tests/test-firewall-flatpak.sh
…
running 1 test
test flatpak_firewall_enforcement_with_real_helper ... ---- probe log ----
allow=OPEN
deny=BLOCKED
-------------------
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 11.70s
```

shepherdd's log shows:

```
INFO Applied firewall (BPF) to scope
  scope=app-flatpak-org.shepherd.firewall.Probe-4151183371.scope
  cgroup=/sys/fs/cgroup/.../app.slice/app-flatpak-org.shepherd.firewall.Probe-4151183371.scope
```

Snap and Process tests both still pass after the `flatpak run --env=`
adapter change.
