# Touch-compat coordinate offset on scaled outputs — remove the #58 scale "fix" (issue #47)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/47>
("Touches in touch compatibility mode are offset")

## Symptom

With `input_compat = "touch_to_mouse"` the synthesized cursor did not track
the finger 1:1. Reproduced live on the kiosk running **Scratch** (flatpak
`edu.mit.Scratch`, an XWayland client) on a Legion Go panel at
`output * scale 1.5` (mode 1920x1080, logical 1280x720). Human Resource
Machine, World of Goo, and Monument Valley — the other `touch_to_mouse`
activities — were unaffected.

## Root cause

The output-scale correction added in #58 (`b3f5d27`, "Account for output scale
in touch bridge absolute mapping") divided the bridge's synthetic absolute
coordinates by the compositor output scale. **That correction is wrong in
principle.** libinput maps the bridge's absolute pointer's declared
`0..=ABS_MAX` range straight onto the output's *logical* layout space, so a
full-pad sweep already lands 1:1 on the screen at any scale — no correction is
needed. Dividing by the scale compressed the reachable area (undershoot).

It went unnoticed because #58 was only ever exercised with
`xwayland_native_resolution = true`, whose HiDPI workaround (`shepherdd::hidpi`)
drops every output to **scale 1.0** for the activity's lifetime — making the
divide a no-op. Scratch is the first `touch_to_mouse` activity that runs
*without* that workaround, so the output stayed at 1.5 and the (wrong) divide
actually ran.

### How it was diagnosed (live repro)

The scale *was* being plumbed correctly (bridge ran with `--output-scale 1.5`),
so the bug was in the correction itself. Swapping the running bridge for a
hand-launched one at varying `--output-scale` and reading the mapping:

| `--output-scale` (divisor) | full-pad sweep reaches | verdict |
| -------------------------- | ---------------------- | ------- |
| 1.5 (production)           | overshoot/offset       | wrong   |
| 2.25 (= scale², first guess) | ~middle of screen (undershoot toward upper-left) | wrong |
| **1.0 (no correction)**    | **exact 1:1, corners and center correct** | **right** |

Repeated taps on one physical point landed the cursor in the same place every
time, confirming the mapping is absolute and linear (not pointer-accelerated),
so a single divisor governs it — and that divisor is 1.0.

An earlier hypothesis (fix "A": XWayland picks up a second factor of the scale,
so divide by `scale²`) was tested and **disproved** by the 2.25 row above.

## Fix

Remove the output-scale correction from the input bridges entirely:

- `shepherd-bridge` (`uinput.rs`): `rescale_abs` no longer takes/`applies` a
  scale (plain range remap); `UinputSink` drops the `abs_scale` field;
  `new_absolute()` / `new_touchscreen()` no longer take `output_scale`.
- `shepherd-touch-bridge` / `shepherd-tablet-bridge`: drop the `--output-scale`
  CLI flag.
- `shepherd-host-linux`: `spawn_touch_bridge()` / `spawn_tablet_bridge()` no
  longer pass `--output-scale`; `touch_output_scale()` removed. (`get_outputs()`
  stays — still used by `shepherdd::hidpi`.)

The tablet bridge (`tablet_to_touch`) carried a copy of the same divide and is
fixed the same way; it has **no live repro** (no tablet device handy) but is a
DIRECT touchscreen mapped onto logical space by the same reasoning, so it needs
re-testing on real hardware.

No config change is required, and no new config surface is added (the abandoned
fix "A" would have added an `xwayland` entry flag — reverted).

## Verified

- `cargo test` for the touched crates (bridge/touch/tablet/host-linux) green;
  `cargo clippy` clean; `cargo fmt --all`; `cargo check --workspace --tests`.
- `--output-scale 1.0` confirmed 1:1 end-to-end against the live Scratch repro
  (taps to corners/center, and a slow edge-to-edge drag, all tracked exactly).
