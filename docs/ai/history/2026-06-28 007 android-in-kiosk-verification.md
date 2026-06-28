# 2026-06-28 — Android: in-kiosk end-to-end verification

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Prior: [2026-06-28 006 android-force-stop-helper-and-preboot.md](2026-06-28%20006%20android-force-stop-helper-and-preboot.md)

## Prompt

> do the in kiosk verification.

Closes the two gaps the earlier phases left explicitly unverified: the runtime
**pkexec → polkit → helper** grant, and a full **launch → window → stop → exit**
cycle driven through the real `LinuxHost` adapter against a live Sway + Waydroid.

## How

The dev box runs GNOME, not the kiosk's Sway, so the test stands up its own
**nested headless Sway** (`WLR_BACKENDS=headless`, with the production
`for_window [app_id="^waydroid\..*"] fullscreen enable` rule), starts a Waydroid
session attached to it, and runs an adapter-level integration test.

- **`crates/shepherd-host-linux/tests/waydroid_real.rs`** — an `#[ignore]`
  integration test (same convention as `firewall_real`): builds a real
  `LinuxHost`, `spawn`s an `EntryKind::Android` (`com.android.calculator2`),
  asserts `HostEvent::WindowReady` + the `waydroid.<pkg>` toplevel is on screen,
  then `stop`s and asserts `HostEvent::Exited` + the window is gone. Skips
  cleanly (`[SKIP]`) when Waydroid / Sway / a running session is absent, so
  `cargo test --include-ignored` is safe in CI.
- **`scripts/integration-tests/test-waydroid.sh`** — orchestrator (mirrors
  `test-firewall.sh`): builds the test, brings up the container (via the helper),
  starts nested Sway + a Waydroid session, then runs the test binary via
  `sudo -u` so the `shepherd-waydroid` group is effective — exercising the
  `force_stop → pkexec → helper` reclaim path as well.

## Result — passed

```
[INFO] Nested sway: SWAYSOCK=… WAYLAND_DISPLAY=wayland-1
[INFO] Waydroid session ready.
[OK] Android app launched, window present: waydroid.com.android.calculator2
[OK] Android session stopped, window gone, Exited emitted
test result: ok. 1 passed; 0 failed
```

This exercises the production code paths end-to-end: `spawn_android` (launch +
wait for the toplevel + `WindowReady`), the for_window fullscreen match, the
window-watch exit task (`Exited`), `stop_android` (Sway window close), and
`force_stop` through pkexec → polkit → the root helper.

Separately confirmed live: `pkexec /usr/libexec/shepherd-waydroid-helper preboot`
under the `shepherd-waydroid` group runs password-less and starts the container
— so the polkit grant itself works, not just the helper.

## Preboot coverage (added)

A second `#[ignore]` test, **`waydroid_preboot_enables_multi_window`**, drives
the real `LinuxHost::preboot_waydroid` and asserts the end state (session
RUNNING + `persist.waydroid.multi_windows == true`). It passed (~94 s),
exercising `preboot_container` (→ pkexec → helper), `start_session_and_wait`
(real boot + ready-log detection), the prop check, and idle-suspend. The
**set-and-restart branch** also passed in a run with
`WAYDROID_TEST_FORCE_RESTART=1` (pre-sets the prop to `false`), ~93 s.

### Finding: cold-launch window timeout

Chaining preboot → launch surfaced that a *cold* first app launch right after a
fresh boot (under software rendering) exceeds the original 20 s
`ANDROID_WINDOW_TIMEOUT` — a warm launch is ~2 s. Bumped to **45 s** so a slow
cold start isn't mistaken for a failed launch. (The orchestrator runs the
launch test against an orchestrator-managed long-lived session, because the
session preboot starts is held by the short-lived test process and dies with it
— a test artifact; in production `shepherdd` is long-lived so its session
persists.)

## What this does and doesn't cover

- **Covered:** the full adapter runtime against real Waydroid + Sway, the
  privileged seam (pkexec/polkit/helper) with the group effective, and
  `preboot_waydroid` (incl. the multi-window restart branch).
- **Still not exercised here:** the launcher UI tile → IPC → engine → adapter
  path (this drives the adapter directly, which is the Android-specific surface;
  engine/IPC routing has its own tests; a full TestHarness HTTP e2e would need
  the harness to pass `SWAYSOCK` to shepherdd, which it doesn't today). Phase 4
  items (Lock Task / Device Owner, GAPPS apps) remain.

## Verification commands

```sh
# one-time: install helper + join the group
sudo ./scripts/integration-tests/setup-waydroid-dev.sh
# run the end-to-end test (sets up nested sway + session itself)
./scripts/integration-tests/test-waydroid.sh
```
