# Show the loading screen while Android preboots (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

Implements the scope in
[2026-07-30 001 startup-loading-screen-scope.md](./2026-07-30%20001%20startup-loading-screen-scope.md).

## Prompt

> keep going on the Android build-out starting with the latest history doc,
> starting with the loading screen for Waydroid only (it's the only one that
> visibly alters the display as its boot process)

That settles the scope doc's "generic or Waydroid-specific?" question: the
plumbing is generic ("a startup step is disrupting the screen"), but Waydroid's
pre-boot is the only thing that emits it. The Steam preload is slow but
invisible — it has no reason to cover the grid, and a cover would be a
regression there.

## What landed

`ServiceStateSnapshot.startup_busy: bool` — true while a screen-disrupting
startup step is in flight.

- **`shepherd-api`** — the new snapshot field.
- **`shepherd-core`** — `CoreEngine::set_startup_busy` (returns whether it
  changed, like `set_kind_readiness`), surfaced in `get_state`.
- **`shepherd-host-api`** — `HostEvent::StartupBusy { busy }`.
- **`shepherd-host-linux`** — `PrebootGate`, which owns *both*
  `waydroid_preboot_done` and the `StartupBusy` event. `preboot_waydroid`
  closes it before spawning the task and opens it where the old
  `preboot_done.store(true)` was.
- **`shepherdd`** — seeds `set_startup_busy(true)` next to the existing
  `set_kind_readiness` seeds, and re-broadcasts `StateChanged` on the host's
  `StartupBusy`.
- **`shepherd-launcher-ui`** — `LauncherState::StartingUp` renders the existing
  loading page.
- Regenerated wire types (`WireTypes.generated.kt`); hand-updated
  `shepherd-webui/src/api/types.ts`, which is not generated.

### Snapshot field, not an event

The scope proposed `EventPayload::StartupBusy`. A one-shot event loses to a
startup race: pre-boot starts with shepherdd, the launcher connects a beat
later, and a client that connects after the event never learns about it. That
would have failed on the *normal* path, not an edge case.

Carrying it in `ServiceStateSnapshot` and re-broadcasting `StateChanged` on
each transition (exactly how kind readiness already works) is correct for late
subscribers, reconnects, and the HUD, and is one wire change instead of two.
`HostEvent::StartupBusy` still exists — it's the host→daemon leg.

### `PrebootGate`

The scope's top gotcha was "emit on **every** exit path; a missed one leaves the
kiosk on a loading screen forever". Rather than trusting that, the flag and the
event are one RAII guard:

- `close()` sets `preboot_done = false` and emits `busy = true`, synchronously,
  before the task spawns.
- `open()` is idempotent (an `AtomicBool::swap` decides whether to emit).
- `Drop` calls `open()`, so an early return or a panicking pre-boot task still
  clears the cover.

Two unit tests cover exactly the two ways this could strand a kiosk: an open
that never emits, and a double emit.

### The launcher had three copies of "apply a snapshot"

The first attempt showed the grid, not the loading page, even though the daemon
had `startup_busy: true` on the wire. `ServiceClient::connect_and_run` — the
initial `service_state` fetch — had its **own** copy of `apply_snapshot` that
ignored the new field, and `app.rs`'s post-launch-failure refresh had a third,
partial one. Both now delegate to `SharedState::apply_snapshot`, so a future
snapshot field can't be honoured on one route and dropped on another.

### Open decisions, resolved

- **HUD**: left alone. It stays live above the loading page, which is what
  `Connecting` and `Launching` already do; the child can't act on it (no
  session, no grid), and a "starting up" HUD state would be new UI for a window
  that is ~15 s long.
- **Generic vs Waydroid-specific**: generic mechanism, single emitter (above).

## Verification

Headless rig (`headless-dev` skill), the scope's four cases:

1. **Cold-ish pre-boot, `config.example.toml`** — loading page up while the
   scale-1 hold runs (`startup_busy: true` in the snapshot on the wire, launcher
   logs "Startup step in progress"); grid returns after "Waydroid pre-boot
   complete", with the Calculator tile now un-gated.
2. **No Android entries** — same code path as (3): `should_preboot` false means
   nothing ever seeds busy.
3. **`preboot = false`** (fixture from `config.example.toml`) — no pre-boot log
   line, grid appears normally.
4. **Pre-boot failure** — forced with a stub `waydroid` on `PATH` that fails
   `session start`. Pre-boot logs "did not reach a ready session", `busy=false`
   is broadcast anyway, and the grid is up. Android stays gated, correctly.

`cargo test --workspace --all-targets`, `cargo clippy --workspace --all-targets
-- -D warnings`, and `cargo fmt --all` all clean, including the rpc-codegen
drift guard.

### Rig note

`target/debug/incremental` had grown to 15 GB and filled the disk mid-build.
Deleting it is safe (cargo regenerates it) and was enough to continue.

## Related / not done

- The ~15 s scale-1 hold could likely shrink toward ~6 s; still an independent
  tuning question, and now purely a performance one — the artifact it caused is
  covered either way. See the scope doc's "Related".
- Nothing else emits `StartupBusy`. If the Steam preload ever wants a cover, the
  wire and UI plumbing is already there.
