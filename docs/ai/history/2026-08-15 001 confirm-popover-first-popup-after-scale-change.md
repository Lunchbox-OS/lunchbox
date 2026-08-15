# Issue 118 follow-up: the close-confirmation prompt is wrong on the first popup after a HUD scale change

Follow-up to PR #118 (`c1f1822`'s ancestor `fix(hud): scale the close-confirmation
buttons under the XWayland DPI hack`), itself a follow-up to #117 / issue #114.

## Prompt

> consider and stress test #118. I'm still seeing the dialog as a whole be too
> small sometimes (but sometimes it's also correct)

## What the stress test found

Driven with the `headless-dev` harness at 1920x1080, a two-entry fixture
identical but for `xwayland_native_resolution`, and output scales 1.5 and 2.0, so
the same prompt could be shot with the hack off (output `scale N`, factor 1.0 —
the reference) and on (output `scale 1.0`, factor N).

Two defects, both only visible while the XWayland DPI hack is active:

1. **The first popup after every `HudScaleChanged` is mispositioned.** Entering
   the hack it is clipped off the right screen edge (the "End activity" button is
   cut in half — #97's failure mode again); leaving it, it floats ~85px (at 1.5)
   / ~196px (at 2.0) left of the "X". The *second* popup — and every one after —
   is correct, which is exactly the "sometimes wrong, sometimes right" the
   maintainer saw: press X during an XWayland activity and it is wrong, cancel
   and press again and it is right.

   Measured, scale 2.0, before the fix (popover bbox, physical px):

   | popup | bbox | verdict |
   | --- | --- | --- |
   | hack, 1st | x=1520..1919 | clipped at the screen edge |
   | hack, 2nd | x=1414..1896 | correct |
   | back to factor 1.0, 1st | x=1216..1707 | 196px left of the "X" |
   | back to factor 1.0, 2nd | x=1412..1895 | correct |

2. **The prompt is consistently a few percent short under the hack**, because
   `gtk4::Box` spacing is a widget property, not CSS, so `scale_px_literals`
   never reaches it — the compromise `2026-07-29 001` recorded. At factor 2.0 the
   popover measured 227px tall against the reference's 252.

Not a defect: the popover's *scale* was right in every state tested, including
when the prompt is opened inside the 500ms window before the HUD applies a new
factor (GTK restyles the open popover) — so no state was found where the dialog
renders at the un-counter-scaled size.

## Root cause of (1)

`popup()` right-aligns the popover by measuring its content box and shifting the
offset by `(popover_width - button_width) / 2` (#97). But **GTK styles a hidden
popover's contents lazily**: they keep the style they had when the popover was
last shown and are only restyled once it is mapped again. So immediately after a
`HudScaleChanged`, `confirm_box.measure()` returns the *previous* factor's
layout, while the `chrome` term already uses the new factor. At 2.0 that is 218
against a true 427 — the offset is 104px short and the surface runs off the
screen.

Chasing the correct measurement after `map` does not work: the restyle lands
several frames later, and re-measuring from an idle (even re-arming until the
value stops changing) reads a half-restyled width — 398 where the truth is 214.

## Fix

`crates/shepherd-hud/src/app.rs`:

- The prompt is **rebuilt** on every `HudScaleChanged` (`build_confirm_prompt`)
  rather than restyled in place, so its widgets are always styled for the factor
  in force and the width measured to place it is always current. Why that is the
  only thing that works is in "The reported symptom" below.
- The HUD's `gtk4::Box` spacings (bar and prompt) and the `TimeDisplay` clock
  icon now rescale with the factor alongside the other icons and the sliders, so
  the counter-scaled HUD matches the native-scale one instead of being tighter.
  `TimeDisplay::set_icon_pixel_size` is new; that icon was the last one still
  pinned at its logical-pixel size.
- Kept as a permanent debug-build affordance:
  `SHEPHERD_HUD_DEBUG_CONFIRM_TRIGGER=<path>` pops the prompt when `<path>` is
  created and dismisses it on `<path>.down`. Three agents in a row have hand-added
  and removed a one-shot version of this hook because the headless harness cannot
  click a GTK button; now it just exists (never in a release build).
- `apply_scale` logs the factor it applies at `info`, and the popover alignment
  logs its inputs at `debug` — enough to tell "the HUD never got the factor" from
  "the HUD got it and mis-laid-out" in a field report.

## Verification

Headless, 1920x1080, both scales:

- Reference vs hacked at 2.0, second popup: 484x252 @ x=1412..1895 vs 483x239 @
  x=1414..1896 — same position, height gap down from 25px to 13px (the rest is
  font leading at 30px vs 15px, not a scalable dimension).
- First popup after every transition, from a freshly booted session: 3
  alternating cycles at scale 1.5 (`xw-confirm` → `plain-confirm`, stop +
  relaunch each time) plus the 2.0 comparison above — 10/10 correct, within 5px
  of the settled position and never clipped. The HUD's `Aligning confirm popover`
  debug line shows the measurement is now always the current factor's:
  `content_w=214` at 1.0, `328` at 1.5, `427` at 2.0 — on the *first* popup after
  each change, where it used to read the previous factor's number.
- A burst of screenshots taken as fast as `grim` allows, starting the instant the
  prompt is triggered: the first captured frame is already at the right size and
  position, and stays there.

`cargo test -p shepherd-hud`, `cargo clippy -p shepherd-hud --all-targets -- -D
warnings`, `cargo fmt --all` clean.

## Harness gotchas found (also folded into the `headless-dev` skill)

- **A stale `dev-runtime/headless/session.env` breaks `dev headless`.** The start
  path sources it, so a leftover `SWAYSOCK` from a dead session is inherited by
  the new sway, which then uses *that* path while the script waits on the one it
  derived from the new pid — "Headless Sway did not answer IPC within 10s" with a
  perfectly healthy stack running. Delete the file (or `dev stop` first).
- **Never use `sleep` as a fixture entry's `command`.** Session teardown calls
  `kill_by_command(<command name>)`, which happily kills the *harness's* own
  `sleep` processes; scripts die mid-scenario with exit 144. Point the fixture at
  a small wrapper script instead.
- `--no-build` with a cleaned `target/debug` boots a stack whose `shepherdd` is
  missing; its `|| swaymsg exit` then takes sway down a second later.

## Take 1 on the reported symptom: could the factor never reach the HUD?

Follow-up from the maintainer, after the above:

> I have been seeing it at the un-counter-scaled size. The repro is on 0.3.4 on a
> 1080p panel with 1.5 DPI, and I've typically seen it when closing Minecraft
> launched with Prism Launcher

0.3.4 contains #117 and #118, so that is post-fix behaviour. "Un-counter-scaled"
first read as "the HUD's factor was 1.0 for the whole session", which turned out
to be **wrong** (see the next section — the bar is never small), but chasing it
surfaced a real robustness gap worth keeping.

`HudScaleChanged` was the *only* way a shell ever learned the factor, and it is
broadcast exactly once, at launch. Any shell that was not subscribed at that
instant renders 1/factor too small until the activity ends, with nothing to
correct it:

- it connected after the launch (started late, or was restarted), or
- its IPC connection dropped and reconnected mid-activity — the reconnect path
  re-seeds volume, brightness, display state and the service snapshot, but never
  the scale factor.

`DisplayController::state` exists for exactly this reason ("for shells that
connect after the last broadcast"); the HiDPI controller had no equivalent.

### Fix

- `HidpiController::factor()` — the counter-scale currently in force, derived
  from the same `saved` scales `restore` reinstates, so it cannot drift from what
  `apply` broadcast. `NoOpHidpiController` returns 1.0.
- `ManagementService::get_hud_scale() -> f64`, hence a `get_hud_scale` IPC/HTTP/BLE
  method for free via `#[management_rpc]` (wire artifacts regenerated with
  `cargo run -p shepherd-wire-codegen --bin rpc-codegen`).
- The HUD calls it on **every** connect, before subscribing, next to the existing
  volume/brightness/display seeding. A missed event now self-heals on the next
  reconnect, and a HUD started mid-activity comes up correct.
- `XwaylandHidpi::apply` warns when every output is already at scale 1.0. On a 1x
  panel that is normal; on a HiDPI one it means a previous session's restore was
  lost (shepherdd restarted mid-session, or `set_output_scale` failed), which
  leaves sway at 1.0 with no record of the real scale — and then *every*
  subsequent XWayland activity renders the HUD un-counter-scaled, sticky until
  the next sway reload. From the HUD's side that is indistinguishable from a
  genuine 1x panel, so it has to be said out loud in the log.

### Verified (headless)

Launch the `xwayland_native_resolution` fixture at output scale 1.5 (sway drops
to 1.0, factor 1.5), then kill and respawn the HUD mid-session — the shape of
"connected after the one-shot event":

```
--- hacked: HEADLESS-1 scale=1.0 1920x1080
--- get_hud_scale RPC: {"result":{"ok":1.5}}
--- restarting the HUD mid-session
DEBUG shepherd_hud::app: Seeded HUD scale factor factor=1.5
 INFO shepherd_hud::app: Applying HUD scale factor=1.5 height=72
popover: x=1532..1901 370x184   bar_h=80
```

The respawned HUD receives no `HudScaleChanged` at all during its lifetime — the
seed is the only source of the 1.5 — and it renders the bar and the prompt at
full size. Without it the same HUD sits at the `factor=1.0` it starts with
(bar 54px, prompt ~2/3 size), which is the reported symptom.

## Take 2, the actual mechanism: a hidden popover keeps the previous factor's style

The maintainer ruled out the whole factor-is-1.0 family:

> I've never seen the HUD itself render small, only the dialog -- what
> circumstances could cause it to restart mid-activity

Nothing does: `sway.conf` starts the HUD with `exec sleep 1 && $hud` — not
`exec_always`, no supervisor — so a crash leaves it gone, not small. And if the
factor were 1.0 the bar would be 48px logical instead of 72px, which is not what
they see.

"Bar right, dialog wrong" leaves exactly one mechanism: **the bar is mapped
continuously, so it restyles the moment `apply_scale` reloads the stylesheet,
while the prompt is hidden — and GTK does not restyle hidden widgets.** The
prompt therefore keeps whatever factor it was last shown under, and can both
measure *and paint* at that size. Measured in the harness at factor 2.0 with the
prompt hidden across the change:

| widget | measured while hidden | truth at factor 2.0 |
| --- | --- | --- |
| the label that survived the change | 218px | 427px |
| a label rebuilt after the change | 366px (183px at factor 1.0) | current |

Re-rooting the contents (`set_child(None)` then `set_child(Some(..))`) does *not*
clear it — measured 218px again. Only a freshly built widget takes the current
stylesheet.

Why it is intermittent for them and never visible here: in this harness the
restyle lands between `map` and the first paint, so only the *measurement* (hence
the position) was ever wrong locally. On a real GPU with a fullscreen XWayland
client above, whether that restyle wins the race before the prompt is drawn is
not something the HUD should be relying on either way.

### Fix

`build_confirm_prompt(action_button, window, factor)` builds the whole prompt —
popover, content box, label, buttons, handlers — and the scale-change branch of
the HUD timer calls it again after `apply_scale`, popping down and unparenting
the old one. The prompt's widgets therefore never survive a scale change, so
they cannot carry a stale style into either a measurement or a frame. The prompt
lives in an `Rc<RefCell<ConfirmPrompt>>` so the click handler, the timer's
"dismiss a lingering prompt" path, and the debug hook all reach the current one;
the click handler copies the three widget refs out and drops the borrow before
`popup()`, since that runs signal handlers.

This also subsumes the ratio-correction heuristic from the first round: with the
widgets always current, `align_popover_to_button` can just measure. That code and
its unit tests are gone.

## Still open

Whether this is what the device is hitting is not confirmed — the symptom does
not reproduce here, because locally the restyle always beats the first paint. If
it recurs on 0.3.6+, the discriminator is the *bar's text* in a photo of the
small dialog: correct-size bar text means the stylesheet is current and something
else is wrong; small bar text in a correctly-sized bar means the stylesheet
itself is stale.
