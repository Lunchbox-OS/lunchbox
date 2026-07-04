# Productivity preset face-button remap (issue #84)

## Prompt

> implement the one in #84

(Following up on a `/remote-control` query about the current productivity gamepad mapping.)

## Issue #84 — "gamepad bridge mapping"

> After observing usage, the productivity mapping should have the following:
> * A: Left mouse button (not Enter)
> * B: Right mouse button
> * X: Enter

## Change

In `crates/shepherd-gamepad-bridge/src/preset.rs`, `tick_productivity`:

- **A (`Button::South`)** now contributes to the **left mouse button** (previously
  emitted Enter).
- **B (`Button::East`)** now contributes to the **right mouse button** (previously
  unused in this preset).
- **X (`Button::West`)** now emits **Enter** (previously unused in this preset).

The existing trigger/bumper → mouse-button mappings are kept as alternates; the
face buttons are OR'd in as additional sources, so A/B are the primary click
surface with triggers/bumpers still working. Start → Esc is unchanged.

Updated the mapping table in `crates/shepherd-gamepad-bridge/README.md` and the
preset tests (renamed `productivity_face_south_emits_enter` →
`productivity_face_south_emits_left_click`, added
`productivity_face_east_emits_right_click` and
`productivity_face_west_emits_enter`).

Only the `productivity` preset changed; `gpd` is untouched.
