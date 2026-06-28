# Loading screen / freeze cover on suspend (issue #73)

Date: 2026-06-28

Status: **implemented** (launcher UI; HUD cover deferred)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/73>

## Prompt

> Scope out #73.

Issue #73 ("Loading screen"):

> Particularly on sleep and resume from sleep, we are not able to draw to the
> screen for seconds at a time. This effectively freezes all UI, most
> prominently the following which are likely to have changed since the system
> was last up:
> * Current time
> * Battery status
> * List of available activities
>
> The cleanest solution is probably to detect that we are about to go to sleep
> and show a static image on the screen before we lose the ability to draw.

Decisions confirmed with the user while scoping:

- **Use a delay inhibitor** so the cover frame is reliably committed before the
  compositor stops drawing.
- A simple "Loading..." text cover is acceptable for now (no image asset).

## Problem

On suspend/resume the compositor cannot draw for several seconds. Whatever frame
was last committed stays frozen on screen, so on resume the user sees a stale
clock, stale battery percentage, and a stale activity list until the UI manages
to redraw with fresh state.

The fix is to commit a neutral "cover" frame *before* sleep. Because nothing
redraws during sleep or during the resume render gap, that committed frame
persists and masks the misleading stale content. Once fresh state arrives after
resume, the cover is removed.

## Why the only hook is logind's `PrepareForSleep`

There are **no in-app power controls**. The HUD README lists "Suspend, shutdown,
restart" but none are wired up: `shepherd-hud/src/app.rs` only issues
`Command::StopCurrent` / `Command::Logout`, and `shepherd-api` has no suspend
command. Suspend is always triggered externally by logind (idle timeout, lid
close). We therefore cannot intercept a button press — the only reliable signal
is logind's `org.freedesktop.login1.Manager.PrepareForSleep`.

That signal is already half-wired in
`crates/shepherdd/src/system_events.rs`: shepherdd subscribes to it but
**deliberately ignores the `start = true` (pre-sleep) edge** (only the
`start = false` resume edge is used today, to nudge the internet re-check — see
`2026-05-30 002`).

## The hard part: timing (the delay inhibitor)

`PrepareForSleep(true)` is best-effort. logind fires it and then proceeds to
sleep; it does **not** wait for subscribers to finish. Without coordination
there is a real race where the cover frame never gets committed before the GPU
stops, making the feature intermittently useless. `grep -rin inhibit` over the
repo returns nothing today — there is no inhibitor anywhere.

The fix is a logind **delay inhibitor lock** taken by shepherdd:

1. On startup, shepherdd calls
   `org.freedesktop.login1.Manager.Inhibit("sleep", "shepherdd",
   "Draw suspend cover before sleep", "delay")`. logind returns a file
   descriptor; holding it open delays sleep up to `InhibitDelayMaxSec`
   (default ~5s).
2. On `PrepareForSleep(true)`, shepherdd broadcasts a new `SystemSuspending`
   event to clients, waits a short grace period for them to draw + commit the
   cover, then **closes the fd** so the system proceeds to sleep.
3. After the system resumes, `PrepareForSleep(false)` fires. shepherdd
   broadcasts `SystemResumed` (and the existing internet re-check still runs)
   and **re-acquires** a fresh delay inhibitor for the next cycle (the fd from
   the previous cycle is dead once we released it).

Notes / constraints:
- The grace period must stay safely under `InhibitDelayMaxSec`; if we overrun,
  logind sleeps anyway. A small fixed delay (e.g. a few hundred ms) is enough to
  let a GTK client switch a view and commit one frame.
- This is a *delay* inhibitor, not a *block* inhibitor — it never prevents
  sleep, it only briefly defers it.
- Missing D-Bus / logind must stay non-fatal, matching the existing watcher
  behavior.

## Proposed changes

### `shepherd-api`
- Add `EventPayload` variants in `crates/shepherd-api/src/events.rs`:
  `SystemSuspending` and `SystemResumed`. Both clients already consume this
  enum, so transport is free.

### `shepherdd`
- `crates/shepherdd/src/system_events.rs`:
  - Act on the `start = true` edge (currently dropped).
  - Add the `Inhibit` method to the `LogindManager` `#[zbus::proxy]` trait and
    manage the inhibitor fd lifecycle (acquire on connect/after resume, release
    on pre-sleep after the grace delay).
  - Give the watcher access to the **event broadcaster** so it can publish
    `SystemSuspending` / `SystemResumed`. Today it only holds an `mpsc` sender
    for internet rechecks; this is the main new plumbing.
- `crates/shepherdd/src/main.rs`: pass the broadcaster handle into
  `spawn_recheck_watchers` (or a renamed/companion spawner).

### `shepherd-launcher-ui`
- Already has a `Stack` with a `"loading"` view
  (`src/app.rs:155`–`164`, `create_loading_view` at `src/app.rs:544`) and a
  central `handle_event` (`src/state.rs:61`).
- On `SystemSuspending`, switch the stack to `"loading"`.
- On resume, keep the cover until the **first fresh `StateChanged`** (or clock
  tick) rather than reacting to `SystemResumed` directly — this guarantees we
  only uncover once there is genuinely fresh content to show.

### `shepherd-hud` (deferred)
- The HUD is a thin always-on layer-shell bar, so covering it would mean a new
  full-screen surface — more work than the launcher cover for less visible
  benefit (during a session the foreground app fills the screen; at the home
  screen the launcher cover already handles it). Out of scope for the first
  pass; revisit if the stale HUD bar on resume proves annoying.

## Scope decisions

- **Cover content**: simple "Loading..." text via the existing
  `create_loading_view` (spinner + label). No PNG asset — none exist in-app
  today, and a styled widget stays theme-consistent.
- **Surfaces covered**: launcher UI only for the first pass; HUD deferred.
- **Inhibitor**: included (required for reliability).
- **Uncover trigger**: first fresh state after resume, not the resume signal.

## Effort

Medium. The bulk is the inhibitor fd lifecycle and wiring the event broadcaster
into `system_events.rs`; the launcher-ui side is a small view switch plus one
event handler.

## What was actually implemented

- `shepherd-api/src/events.rs`: added `EventPayload::SystemSuspending` and
  `EventPayload::SystemResumed`.
- `shepherdd/src/system_events.rs`: renamed `spawn_recheck_watchers` →
  `spawn_system_event_watchers` and generalized it. It now:
  - adds an `Inhibit` method to the `LogindManager` `#[zbus::proxy]` trait and
    holds a `"sleep"`/`"delay"` inhibitor fd (`zbus::zvariant::OwnedFd`),
    re-armed after each resume; acquisition failure is logged and non-fatal;
  - on the `start = true` edge: broadcasts `SystemSuspending`, sleeps
    `SUSPEND_COVER_GRACE` (750 ms), then drops the fd so the system sleeps;
  - on the `start = false` edge: broadcasts `SystemResumed`, signals the
    service via `resume_tx` to push a fresh `StateChanged`, and (if present)
    nudges the internet monitor;
  - takes a `BroadcastFn = Arc<dyn Fn(Event) + Send + Sync>` plus an
    `Option<recheck_tx>` (internet monitor may be absent) and a `resume_tx`.
    It no longer stops when the internet monitor is gone — the cover is useful
    regardless of internet gating.
- `shepherdd/src/main.rs`: the watcher is now spawned unconditionally (the
  internet monitor spawn became conditional and just supplies `recheck_tx`); a
  new `resume_rx` arm in the main `select!` loop broadcasts a fresh
  `StateChanged` snapshot on resume.
- `shepherd-launcher-ui`: new `LauncherState::Suspending` (maps to the existing
  `"loading"` stack view); `handle_event` sets it on `SystemSuspending`, leaves
  the cover up on `SystemResumed`, and the subsequent `StateChanged` from
  shepherdd restores the grid/session with fresh content.
- HUD: unchanged. Its `EventPayload` match already has a `_ => {}` arm, so the
  new variants are ignored there for now.

## Follow-up: HUD suspend placeholders

The launcher cover hides the home screen, but the HUD bar stays visible during
a session and would otherwise freeze showing a stale clock, battery %, and
network status across the suspend/resume gap. So the HUD now shows placeholders
in the suspend state too, mirroring the launcher's model (placeholder on
`SystemSuspending`, clear on the fresh `StateChanged`).

- `shepherd-hud/src/state.rs`: added a `suspended` watch flag.
  `SystemSuspending` sets it; `SystemResumed` is a no-op (placeholders stay up);
  `StateChanged` clears it. `is_suspended()` exposes it to the render timer.
- `shepherd-hud/src/app.rs`: the 500 ms render timer, when suspended, draws
  `--:--` for the clock, `battery-missing-symbolic` + `--%` for battery, and a
  neutral `content-loading-symbolic` "Checking connectivity…" placeholder for
  the network indicator (instead of the live values). Volume/brightness are
  left alone — they don't change while suspended.

### Making the post-resume network status fresh, not just non-stale

Clearing the HUD placeholder on `StateChanged` is only safe if that snapshot's
connectivity is actually fresh. shepherdd's immediate resume `StateChanged`
carried the *pre-suspend* internet status (the re-check runs asynchronously),
and `InternetStatusChanged` only fires on a *change* — so it can't be relied on
to clear the placeholder. Fixed by:

- `shepherdd/src/internet.rs`: after a re-check triggered by a system event
  (resume / network change), broadcast a fresh full `StateChanged` snapshot —
  not just the per-target `InternetStatusChanged` on change.
- `shepherdd/src/system_events.rs`: on resume, when an internet monitor exists,
  drive the post-resume `StateChanged` from that re-check (send
  `RecheckTrigger::ResumedFromSleep`) instead of the immediate stale snapshot;
  with no monitor, fall back to the immediate `resume_tx` broadcast (no checks
  configured → nothing to be stale about).

Trade-off: when internet checks are configured, the launcher cover and HUD
placeholders now persist until the resume re-check completes (bounded by the
check timeout, default 1.5 s) rather than clearing on an immediate-but-stale
snapshot. That is intentional — it guarantees the first non-placeholder frame
shows real connectivity.

## Verification (done)

- `cargo build --workspace`, `cargo clippy -p shepherd-api -p shepherdd
  -p shepherd-launcher-ui --all-targets -- -D warnings`,
  `cargo test -p shepherd-api -p shepherdd -p shepherd-launcher-ui`, and
  `cargo fmt --all` all clean.
- Not yet exercised on real hardware — see the manual plan below.

## Verification plan

- `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
  `cargo fmt --all` clean.
- Manual (in a Sway session): `systemctl suspend`, then resume; with debug
  logging expect a `SystemSuspending` broadcast and the launcher switching to
  the loading view *before* the screen freezes, the loading frame persisting
  across the resume render gap, and the grid returning once fresh state arrives.
- Confirm the inhibitor is visible while held: `systemd-inhibit --list` should
  show a `delay`/`sleep` lock owned by shepherdd, and it should disappear during
  the pre-sleep grace window.
