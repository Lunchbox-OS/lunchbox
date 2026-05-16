# Embedded mpv player UI for shepherd-media

Branch: `u/albert/9/media-launcher`. Follow-up to the original spec at
`2026-05-02 007 media launcher.md`.

## Problem

The v1 player ran mpv in its own fullscreen window with mpv's built-in
on-screen controller (`osc=yes`) and default keybindings. That worked
for a kiosk with a keyboard, but is poor for the actual deployment
targets: handhelds with touchscreens, and TVs driven by a gamepad. mpv's
OSC was designed for a desktop pointer — tap targets are small, it has
no focus cursor for gamepad navigation, and there is no clean way to
wire arbitrary controller input through it.

The user-facing question was "what does playback look like for touch
and controller?" After comparing three approaches in conversation
(tuning mpv's OSC, a wlr-layer-shell egui overlay, or embedding mpv
into the egui surface) we went with the third: embed mpv inside the
eframe window via `libmpv2::render::RenderContext` and own the whole
input pipeline ourselves.

## What changed

- `PlayerHandle` (in `shepherd-media-core/src/player.rs`) gained
  transport methods (`set_paused`, `seek_relative`, `seek_absolute`,
  `position`, `duration`, `set_volume`, `volume`) and embedded-render
  hooks (`bind_gl`, `render`, `set_redraw_callback`). All have default
  no-op impls so adapters (`CachingPlayer`) only need to delegate.
- `LibmpvPlayer` was rebuilt for embedded rendering: `vo=libmpv`,
  `osc=no`, `input-default-bindings=no`, `hwdec=auto-safe`. The old
  `/tmp/shepherd-media-input.conf` hack that remapped q/ESC/CLOSE_WIN
  to `stop` is gone — the UI drives stop directly through
  `Session::handle_input(SessionInput::StopPlayback)`.
- The `RenderContext` is created lazily inside `bind_gl`, which the UI
  calls from eframe's creation closure where `CreationContext::
  get_proc_address` is available. A SAFETY transmute is needed because
  `libmpv2::render::OpenGLInitParams<C>` has no lifetime parameter even
  though the borrow is only used for the duration of
  `RenderContext::new`.
- `Session` (still platform-agnostic) gained thin delegating wrappers
  for the new trait methods so the playback view doesn't need to hold
  a separate borrow alongside the session.
- New module `crates/shepherd-media/src/ui/playback.rs` owns the
  offscreen FBO + texture, asks mpv to render into it each frame, and
  draws an egui control overlay on top. The texture is registered with
  `Frame::register_native_glow_texture` and used as an
  `egui::Image`.
- `crates/shepherd-media/src/ui/mod.rs` was reshaped so a single
  `App` (formerly `BrowseApp`) hosts both the grid and the playback
  view. `StartMode::Playing(item_id)` lets direct-play mode open
  straight into the playback view and exit when playback ends.
- `crates/shepherd-media/src/main.rs` was simplified: `run_play` and
  `run_browse` both build a `Session` and hand it to `ui::run`. The
  old direct-play loop that managed mpv outside of eframe is gone.

## Input mapping

| Action            | Touch / Mouse         | Keyboard            | Gamepad                  |
|-------------------|------------------------|---------------------|--------------------------|
| Play / Pause      | Tap play button        | `Space`, `K`        | A (south)                |
| Back to grid      | Tap back button        | `Esc`, `Backspace`  | B, Start, Select         |
| ±10 s seek        | Tap skip buttons       | `←` `→`, `J` `L`    | D-pad L/R, LT/RT         |
| Scrub             | Drag scrubber          | —                   | —                        |

Controls auto-hide after 3 s of input silence; any input summons them
back. While paused they stay visible regardless. Volume is owned by the
global `shepherd-hud`, not by this overlay.

## Things we deliberately did *not* do

- Spawn a separate wlr-layer-shell overlay window. Tried in the
  three-approach comparison; rejected because of Wayland input-region
  complexity.
- Add a focus cursor to the playback overlay. The action set is small
  enough that fixed button bindings work without a moving focus ring,
  matching how a real remote control works.
- Touch up the browse grid's UI. Browse-mode focus, navigation, and
  poster styling are unchanged.
- Add animated transitions or other "kid-mode" affordances. The
  original spec explicitly forbade them and that still applies.

## Open follow-ups

- The new `bind_gl` requires an `unsafe { transmute }` to extend a
  borrow's lifetime to `'static` because the upstream `libmpv2` API
  has no lifetime parameter on `OpenGLInitParams<C>`. A PR upstream
  would let us drop the transmute.
- Audio-only items render a blank FBO. Acceptable for v1 (the title
  is shown in the header) but a future tweak could draw the poster
  art for `kind = "audio"` items instead of black.
