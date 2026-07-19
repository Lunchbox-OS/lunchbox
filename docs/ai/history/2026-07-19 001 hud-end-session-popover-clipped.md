# "End session" confirmation popover is clipped off the right edge (issue #97)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/97>
("'End session' button is occasionally cut off")

## Symptom

The HUD's "End session" confirmation prompt sometimes appears positioned too far
to the right and is partially cut off at the right screen edge. Reported as
intermittent ("not sure what's triggering it"); the maintainer's guess was "some
combination of the XWayland DPI hack and screen mirroring."

## Root cause

The prompt is a `gtk4::Popover` parented to the action button
(`crates/shepherd-hud/src/app.rs`), which is the **rightmost** widget of the
right-aligned HUD bar (`right_box` is `halign(End)`). The popover was given no
explicit placement, so GTK positioned it **horizontally centered on the button**.
Because the button sits at the extreme right edge, half the popover lands past
the right edge of the output.

Normally a toolkit slides such a popup back on-screen, but under
gtk4-layer-shell + sway the oversized layer-shell popup is **not** slid to fit —
verified live (see below), it clips in whichever direction it overflows (right
when centered; the top when positioned `Left`). So the centered popover is
simply clipped.

The XWayland HiDPI workaround (`shepherdd::hidpi`, issue #45) makes it worse and
more visible: while a `xwayland_native_resolution` activity runs, every output
drops to scale 1.0 and the HUD counter-scales its CSS by the captured factor
(e.g. 1.5). The popover's padding/font scale with that factor, so it gets
physically larger and the overflow grows. On a wide output the centered popover
already overflows at factor 1.0, which is why it looked intermittent — the DPI
hack (and a wide docked/mirrored panel) just makes it obvious.

## Fix

`crates/shepherd-hud/src/app.rs`, popover setup + the action button's click
handler:

- Give the popover an explicit `PositionType::Bottom`, so it drops straight down
  from the button (a top bar has unlimited room below — no vertical clipping).
- At `popup()` time, right-align the popover to the button instead of letting it
  center: shift its center left by `(popover_width - button_width) / 2`, which
  lands the popover's right edge on the button's right edge. Since the button is
  at the right edge and the popover is far narrower than the output, the whole
  popover is guaranteed on-screen — **without relying on the compositor doing
  any slide-to-fit**.
- Width is measured from the content `Box` (a plain widget), not the popover: a
  `GtkPopover` is a native surface and reports a near-zero preferred size before
  it is mapped. The popover chrome (`> contents` padding, which scales with the
  HUD factor) is added on top so the whole surface clears the edge.

No config or API change.

## Verified (headless)

Reproduced and fixed end-to-end with the `headless-dev` harness, since the real
trigger needs the scale hack:

- Fixture config with one always-available entry, `command = "sleep"`,
  `xwayland_native_resolution = true`.
- Boot headless at 1920×1080, `swaymsg output HEADLESS-1 scale 1.5`, then launch
  the entry over the shepherdd IPC socket (`{"method":"launch",...}` — synthetic
  pointer clicks do **not** reach GTK in the headless harness). Launching fires
  the HiDPI hack: output drops to scale 1.0 and the HUD gets `factor = 1.5`.
- Before the fix, the popover was clearly clipped ("End Repro Native?" and the
  red "End activity" button ran off the right edge). After the fix, the whole
  prompt sits just left of the "X", dropping below the bar, at both factor 1.5
  and the normal factor 1.0.

`cargo test -p shepherd-hud`, `cargo clippy -p shepherd-hud --all-targets -- -D
warnings`, and `cargo fmt --all` all clean.

### Note on the repro

To pop the confirmation headlessly (no working synthetic clicks) a temporary
one-shot debug hook (`SHEPHERD_HUD_DEBUG_AUTOPOPUP=1` → `action_button
.emit_clicked()` once a session is active) was used and then removed; it is not
part of the committed change.
