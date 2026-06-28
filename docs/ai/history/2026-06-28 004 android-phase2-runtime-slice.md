# 2026-06-28 — Android activity kind: Phase 2 (runtime vertical slice)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Scope: [2026-06-28 001 android-activity-kind-scoping.md](2026-06-28%20001%20android-activity-kind-scoping.md)
Phase 0: [2026-06-28 002 android-phase0-host-spike.md](2026-06-28%20002%20android-phase0-host-spike.md)
Phase 1: [2026-06-28 003 android-phase1-config-plumbing.md](2026-06-28%20003%20android-phase1-config-plumbing.md)

## Prompt

> write your findings into the history, commit, and go as far as you can.

Phase 2: make the `android` kind actually launch, track, and stop a Waydroid
app on Linux. The defining constraint (from Phase 0) is that **Android has no
host process** — `waydroid app launch` returns immediately and the app runs
inside the container. So the whole runtime is built on the Wayland toplevel,
not a pid.

## A bench finding that shaped the design

Before writing code I tested, on the real Waydroid install, whether the
**user-level** `swaymsg [app_id="waydroid.<pkg>"] kill` ends a session — because
shepherdd runs **unprivileged** (it elevates via pkexec, like the firewall
helper), and `waydroid shell am force-stop` needs root.

Result: closing the toplevel via Sway removes the app from screen in ~2 s with
no root — **but the Android process survives** (Android caches it). So:

- **stop = close the Wayland toplevel** (user-level, always works; this is the
  authoritative, visible session end).
- **`am force-stop`** (root) is only needed to *reclaim the cached process* and
  is therefore **best-effort** until a privileged seam exists.

This unblocked a Phase 2 that is essentially all user-level.

## What landed

### `crates/shepherd-host-linux/src/waydroid.rs` (new)
CLI wrapper + pure helpers:
- `app_id_for_package` → `waydroid.<pkg>`, `launch_argv`, `force_stop_argv`,
  `parse_session_running` (pure, unit-tested).
- `session_running()` (`waydroid status`), `launch_app()` (session user),
  `force_stop()` (best-effort; logs and returns on the expected unprivileged
  "needs root" failure rather than erroring).
- Module doc spells out the user-vs-root privilege split.

### `crates/shepherd-host-api/src/handle.rs`
- New `HostHandlePayload::Android { package_name }` (no pid). `pid()` returns
  `None` for it.

### `crates/shepherd-host-api/src/capabilities.rs`
- `linux_full()` now advertises `EntryKindTag::Android` (the kind is spawnable).

### `crates/shepherd-host-linux/src/adapter.rs`
- `spawn()` dispatches Android **early** to `spawn_android`, bypassing the
  pid/`ManagedProcess` machinery entirely:
  1. require `waydroid::session_running()` (else a clear spawn error — preboot
     is a follow-up, see below);
  2. `launch_app(pkg)`;
  3. poll `sway::list_windows()` for the `waydroid.<pkg>` toplevel up to 20 s;
     on success emit `WindowReady`, on timeout best-effort `force_stop` + error;
  4. arm a **window-watch** task and return an `Android` handle.
- `spawn_android_window_watch`: polls every 1 s; when the toplevel disappears
  (user closed it, or `stop` closed it), best-effort `force_stop`, remove the
  session-info token, and emit `Exited` **exactly once** (the removal is the
  dedup guard). This is the single exit path for Android.
- `stop()` dispatches `Android` payloads to `stop_android`: close the toplevel
  via `sway::act_on_window(Close)` + best-effort `force_stop`. It does **not**
  emit `Exited` — the watch task does, keeping one consistent exit path. Both
  `Graceful` and `Force` map to the same action.
- Helpers `android_window_id` / `wait_for_android_window` over `sway`.

## Verification

- `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all-targets` — all green (0 failures).
- New unit tests: app_id convention, launch/force-stop argv, status parsing.
- The wrapped CLI behaviours were validated **live** on the box in Phase 0 and
  the stop experiment: `waydroid status` format (`parse_session_running` agrees
  with live output), `app launch`, the `waydroid.<pkg>` app_id, window close via
  Sway, and `am force-stop` needing root.
- **Not yet verified end-to-end**: a full launch→play→exit cycle driven by
  `shepherdd` in the real nested-Sway kiosk runtime. The dev box runs GNOME, and
  standing up the whole kiosk is out of scope here. The building blocks are each
  validated; the integrated run is the main open verification item.

## Deferred (clearly-scoped follow-ups)

1. **Preboot + multi-window enforcement.** `[service.waydroid]` is parsed
   (Phase 1) but not yet consumed. A `preboot` step (start the session at
   daemon startup, set `persist.waydroid.multi_windows`, wait for the ready log
   line) should run from `shepherdd` startup when `preboot` is set or any
   Android entry exists. The session-start helpers were intentionally **not**
   committed here to avoid shipping unverified orchestration; today `spawn`
   errors clearly if the session isn't already running. The
   `[service.waydroid]` config must also be threaded from `ServiceConfig` into
   the host adapter.
2. **Privileged force-stop.** A pkexec + polkit seam (mirroring
   `org.shepherd.firewall.policy` / `shepherd-firewall-helper`) so the cached
   Android process is reliably reclaimed on stop. Without it, stop still ends
   the visible session; it just leaves a cached process for the container's
   idle-suspend to handle.
3. **Per-app network policy** remains container-global (one Waydroid instance);
   the per-entry firewall does not reach Android traffic. Documented in scoping.

## Status

The Android kind now launches, tracks, and stops end-to-end *in code* and is
advertised as supported on Linux. Phases 0–2 give a working vertical slice
(modulo preboot and the privileged force-stop add-on) plus the host validation
behind it.
