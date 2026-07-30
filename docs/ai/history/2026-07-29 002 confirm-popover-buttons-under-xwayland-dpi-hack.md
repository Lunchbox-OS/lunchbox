# Issue 114 follow-up: close-confirmation buttons too small under the XWayland DPI hack

Follow-up to PR #117 (`31d4184`), which fixed the warning banner and slider
knobs for issue #114.

## Prompt

> following up on #117, the close activity warning/confirmation is also too small
> with the XWayland hack active

## Background

Same mechanism as #114: while an `xwayland_native_resolution` activity runs,
shepherdd drops the sway output to `scale 1.0` and the HUD counter-scales by
multiplying every `Npx` literal in its own stylesheet (`scale_px_literals`). Any
dimension left to the GTK theme keeps its logical-pixel value and so renders
1/factor too small on screen. See `2026-07-29 001` and `2026-05-17 001`.

## Root cause

The close-confirmation popover (the "really end?" prompt from the HUD "X"
button, issue #78) has three text nodes:

- `.confirm-close-message` — states `font-size: 15px`, so it scales. Correct.
- The **Cancel / End activity buttons** — stated no `font-size`.

PR #117 set `font-size: 14px` on `.confirm-close-popover > contents` (the
popover surface) on the assumption that the button labels would *inherit* that
scaled size. They don't: the GTK theme sets an explicit `font-size` on the
`button` node, and an explicit rule on the element beats an inherited value in
the cascade regardless of the `> contents` rule's specificity. So the labels
kept the theme's logical-pixel size while the button box around them
(`min-height`/`padding`, stated in px) grew with the factor — the label shrank
relative to its button exactly on a HiDPI panel.

This is the same class of bug as the slider knob in #117 (theme size wins the
cascade), not the warning banner (no rule at all): inheritance was never going
to reach the buttons.

## Fix

`crates/shepherd-hud/src/app.rs` (`CSS_TEMPLATE`): add `font-size: 14px` to the
`.confirm-close-popover button` rule itself. `.confirm-close-popover button` is
more specific than the theme's bare `button`, so it wins the cascade, and being
a px literal it now follows the counter-scale. 14px matches the base #117 chose
for `.hud-bar` and `> contents`, so the un-hacked HUD is effectively unchanged
(the theme's own button font is ≈14.7px).

New regression test `confirm_popover_button_declares_its_own_font_size` asserts
the button rule states a `font-size`, so a future edit can't regress it back to
relying on inheritance.

## Verification

Headless dev session, a two-entry fixture identical except one entry sets
`xwayland_native_resolution` — so the same popover can be shot with the hack off
(the reference: output `scale 1.5`, factor 1.0, GTK renders natively) and on
(`scale 1.0`, factor 1.5, HUD counter-scales). The popover was opened with a
temporary debug hook (`SHEPHERD_HUD_DEBUG_POP_CONFIRM`) that calls
`action_button.emit_clicked()` once a session is up, because the synthetic
pointer does not fire GTK `clicked` handlers (see the `headless-dev` skill).

Three shots of the button row, cropped and stacked:

- reference (scale 1.5, factor 1.0): large labels — the target.
- hack, before fix: labels visibly ~30% smaller than the target while the button
  boxes are full size.
- hack, after fix: labels back to the target size.

Same recipe and gotchas as the #114 note (async scale restore on stop; assert
the target state in the pixels; there is no `dev swaymsg` passthrough — use
`SWAYSOCK` from `dev-runtime/headless/session.env`).
