# Confirm before the HUD "X" button ends an activity (issue #78)

## Prompt

Issue #78 (<https://git.armeafamily.com/albert/shepherd-launcher/issues/78>),
titled "Add confirmation dialog for closing activities using the \"X\" button":

> Many activities will lose state when closed forcibly. Since the "X" is easy
> to accidentally hit, we should show a confirmation dialog when it is pressed,
> and only end the activity upon confirmation.
>
> This should be configurable per-activity but enabled by default.
>
> (This behavior only affects the "X" button -- if the activity closes from the
> API, time expiration, or similar, the activity should close right away.)

## Design

The "X" is the HUD's action button (`shepherd-hud/src/app.rs`), which shows
`window-close-symbolic` / "End session" while a session is active and sends
`Command::StopCurrent { mode: Graceful }`.

The key constraint — confirm **only** for the "X", never for API / expiration /
process-exit — drove where the confirmation lives. At the daemon, a HUD
graceful stop is **indistinguishable** from an HTTP `DELETE /sessions/current`
graceful stop: both call `CoreEngine::stop_current` and both arrive as
`SessionEndReason::UserStop`. Time expiration takes a different path entirely
(engine marks the session `Expiring`, the process is asked to stop, and
`SessionEnded { Expired }` only fires when the process actually exits). So the
daemon *cannot* gate on "was this the X button", and it must not — the API is
supposed to close immediately. The confirmation is therefore purely a **HUD**
concern: intercept the click, prompt, and only then send the same
`StopCurrent` the button always sent. No daemon behaviour changes.

What the daemon *does* do is tell the HUD the activity's preference. A new
per-entry `confirm_on_close` bool (default `true`) is plumbed to the HUD via the
same two channels the HUD already learns about a session from:
`EventPayload::SessionStarted` and `ServiceStateSnapshot.current_session`
(`SessionInfo`).

### Confirmation UI

A `gtk4::Popover` parented to the action button. On a layer-shell overlay a
popover is a child popup that renders above the fullscreen activity, so no
separate top-level window (which Sway would place awkwardly under the activity)
is needed. It is built once and re-shown on demand, its message relabeled with
the current activity name each time ("End {name}? Unsaved progress may be
lost."), with **Cancel** and a destructive **End activity** button.

The popover is `set_autohide(true)` so it self-dismisses on focus loss / outside
click / Escape. Autohide relies on an input grab that needs the layer surface to
accept keyboard focus, and a bar that permanently requested keyboard focus would
steal it from the activity, so the HUD is switched to `KeyboardMode::OnDemand`
only while the prompt is up (on `popup()`) and back to `KeyboardMode::None` in
the popover's `closed` handler. Autohide only reacts to user input, so it does
**not** cover the activity ending underneath the prompt — the HUD's existing
500 ms state tick also pops the popover down whenever the session is no longer
`Active`/`Warning` (expiry, API stop, process exit).

Contrast: the popover uses an opaque dark surface (`#1e1e1e`) with explicit
white text and a solid-red destructive button, hard-coded rather than via the
theme's CSS variables, so it stays high-contrast regardless of the system GTK
theme and the bright activity behind it never bleeds through.

Behaviour on click: session active + `confirm_on_close` ⇒ popup; session active
+ opted out ⇒ send `StopCurrent` immediately (old behaviour); no session ⇒
`Logout` (unchanged, never confirmed).

## What was implemented

- `crates/shepherd-config/src/schema.rs` — `RawEntry.confirm_on_close`
  (`#[serde(default = "default_true")]`) + parse test
  (`confirm_on_close_defaults_true_and_parses_false`).
- `crates/shepherd-config/src/policy.rs` — `Entry.confirm_on_close`, copied in
  `Entry::from_raw`.
- `crates/shepherd-core/src/session.rs` — `SessionPlan.confirm_on_close`;
  carried into `SessionInfo` in `to_session_info`.
- `crates/shepherd-core/src/engine.rs` — set the plan field from
  `entry.confirm_on_close` in `request_launch`; add it to
  `CoreEvent::SessionStarted` in `start_session`.
- `crates/shepherd-core/src/events.rs` — `CoreEvent::SessionStarted.confirm_on_close`.
- `crates/shepherd-api/src/types.rs` — `SessionInfo.confirm_on_close`
  (`#[serde(default = "default_confirm_on_close")]`, defaults `true` for older
  payloads).
- `crates/shepherd-api/src/events.rs` — `EventPayload::SessionStarted.confirm_on_close`
  (same serde default).
- `crates/shepherdd/src/main.rs` — thread the flag through both
  `SessionStarted` broadcast sites.
- `crates/shepherd-http/src/handlers/sessions.rs` — capture
  `plan.confirm_on_close` before the plan is moved and pass it in its
  `SessionStarted` broadcast.
- `crates/shepherd-hud/src/state.rs` — `confirm_on_close` on
  `SessionState::{Active, Warning}`; `confirm_on_close()` / `entry_name()`
  accessors (both `false`/`None` when there is no session to end); populate from
  `SessionStarted`, the warning transitions, and `StateChanged`; accessor tests.
- `crates/shepherd-hud/src/app.rs` — the confirmation popover, extracted
  `spawn_command` / `request_stop_current` IPC helpers, and popover CSS.
- `crates/shepherd-launcher-ui/src/state.rs` — ignore the new event field.
- Docs/config: `config.example.toml` (documented `confirm_on_close`, off-by-
  default example), `crates/shepherd-hud/README.md`,
  `shepherd-webui/src/api/types.ts` (`SessionInfo.confirm_on_close`).

## Notes / scope

- Deliberately scoped to the HUD "X", per the issue. The launcher-ui gamepad
  "Mode" button and the `--stop-current` compositor keybinding
  (`shepherd-launcher-ui`) also send `StopCurrent` but are not gated — they are
  more deliberate gestures, and the issue names only the "X". Extending the
  prompt to them would be a follow-up.
- Not exercised end-to-end against a live compositor here (needs the kiosk
  session). Confirm on-device that the popover renders above a fullscreen
  activity and that Cancel/End behave; `confirm_on_close = false` should still
  close instantly, and an HTTP `DELETE /sessions/current` should never prompt.

Verified: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --all-targets`, and `validate-config config.example.toml` all pass.
