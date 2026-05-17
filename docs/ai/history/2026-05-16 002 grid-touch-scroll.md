# Grid touch-scroll: custom drag-to-scroll

## The bug

In `shepherd-media`'s browse mode on a touchscreen:

> First drag scrolls the grid correctly. On every subsequent drag, the scroll
> jumps to the top before the new drag takes effect — *but only* when the
> previous touch was released while the finger was stationary. Releasing
> mid-motion produces a clean kinetic flick with no jump on the next touch.

## Root cause

egui's `ScrollArea` drag-to-scroll is delta-based:

```rust
// egui-0.34.2 scroll_area.rs:822
if response.dragged() {
    state.offset[d] -= input.pointer.delta()[d];
}
```

`pointer.delta()` is computed as `new_latest_pos - old_latest_pos` between
frames. When the user releases with a non-zero velocity, kinetic scrolling
keeps frames flowing and consumes the time between touches — by the time the
next touch starts, the pointer history is clean.

When the user releases while stationary, no kinetic runs and frames stall
until the next touch event. Whatever combination of stale-state buffering
and pointer-event ordering happens between the lift and the next press
gets surfaced as a non-zero `delta()` on the first frame of the new drag
— and that delta is large enough (a full screen height of "jump") to push
the offset to 0 (clamped). The same bug bit `egui::Slider` on touch and was
already worked around in `playback.rs::touch_slider` by reading absolute
pointer positions instead of frame deltas.

## Fix

`crates/shepherd-media/src/ui/grid.rs` now disables egui's drag-to-scroll
on the grid (`ScrollSource { drag: false, ..ALL }`) and implements a custom
drag handler that mirrors `touch_slider`'s approach:

1. Allocate a `Sense::drag()` rect over the scroll area (drag-only so it
   coexists with `Sense::click()` tiles via egui's independent click/drag
   hit-test).
2. On press, snapshot `start_pointer_y` and `start_offset_y`.
3. Each drag frame: `target_offset = start_offset_y + (start_pointer_y - current_pointer_y)`.
4. Track a smoothed velocity from `(time, pointer_y)` samples for the kinetic
   flick on release.
5. Override `ScrollArea::vertical_scroll_offset()` only when actively
   dragging or kinetic-decaying, so wheel/keyboard/`scroll_to_me` keep
   working normally.

Persistent state lives on `App` as `grid::ScrollState`.

## Things tried and rejected

- **`ScrollBarVisibility::AlwaysHidden`** — first guess was that the bug
  was the scrollbar's click-to-jump-position behavior. It wasn't.
- **Bumping egui from 0.29 → 0.34** — egui 0.32 has #5778 (kinetic touch
  fix). The upgrade improved kinetic behavior generally but did not fix
  this specific delta-on-press bug. Kept the upgrade though: 0.34 has
  several other relevant ScrollArea fixes.

## Upstream

No matching issue filed against `emilk/egui`. Worth doing — the test case
is "release stationary, then drag again." `touch_slider`'s comment in
`playback.rs:413` already calls out the same delta-vs-touchscreen problem
on `egui::Slider`.
