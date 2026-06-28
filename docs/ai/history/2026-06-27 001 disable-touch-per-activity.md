# Disable the touchscreen per-activity (issue #68)

## Prompt

Issue #68 (<https://git.armeafamily.com/albert/shepherd-launcher/issues/68>),
titled "Add a way to disable the touchscreen per-activity", with no body. The
agent was asked to implement it.

## Design

`input_compat` already enumerates orthogonal per-activity input sidecars
(`touch_to_mouse`, `tablet_to_touch`, `gamepad_productivity`, `gamepad_gpd`).
The cleanest place for "disable the touchscreen" is a new `input_compat`
mode, `disable_touch`.

The `shepherd-touch-bridge` sidecar already does exactly the hard part:
auto-detect every touchscreen and `EVIOCGRAB` it so the activity never sees
raw touch. The only difference for "disable" is that nothing should be
synthesized. So rather than add a new crate, the touch bridge gained a
`--grab-only` flag: it grabs the touchscreens and **discards** their events,
emitting nothing. This also means `disable_touch` needs the `input` group but
**not** `/dev/uinput` (no virtual pointer is created).

### Mutual exclusion

`touch_to_mouse`, `tablet_to_touch`, and `disable_touch` all grab or produce
the touchscreen, so at most one can be active at a time (two `EVIOCGRAB`s of
the same device fight; `disable_touch` would also grab the synthetic
touchscreen produced by `tablet_to_touch`). The existing pairwise
touch↔tablet conflict check in `policy::convert_input_compat_list` was
generalized: a new `InputCompatMode::handles_touch()` predicate marks all
three modes, and the conflict guard drops any later touch-handling mode with
a warning. `disable_touch` still stacks fine with a `gamepad_*` preset
(gamepad observes a different device and doesn't grab).

## What was implemented

- `crates/shepherd-touch-bridge/src/main.rs` — `--grab-only` flag and a
  `run_grab_only_loop` that drains and discards reader updates until SIGTERM.
  Skips `UinputSink` creation entirely in this mode.
- `crates/shepherd-api/src/types.rs` — `InputCompatMode::DisableTouch` plus
  `InputCompatMode::handles_touch()`.
- `crates/shepherd-config/src/schema.rs` — `RawInputCompat::DisableTouch`
  (+ parse test).
- `crates/shepherd-config/src/policy.rs` — conversion arm and generalized
  conflict guard via `handles_touch()` (+ exclusion/stacking test).
- `crates/shepherd-host-linux/src/sidecar.rs` — `spawn_disable_touch()`
  (reuses `touch_bridge_binary()` with `--grab-only`) and the
  `GamepadPreset::from_mode` match arm.
- `crates/shepherd-host-linux/src/adapter.rs` — `DisableTouch` spawn arm.
- Docs: `config.example.toml`, `docs/INSTALL.md`,
  `crates/shepherd-touch-bridge/README.md`.

No new workspace member / binary / udev rule was needed — `disable_touch`
runs the existing `shepherd-touch-bridge` binary.

## Verification

`cargo fmt --all`, `cargo build`, `cargo clippy --all-targets -D warnings`,
`cargo test --all-targets`, and `validate-config config.example.toml` all
pass. Not yet exercised end-to-end against a live activity (needs the `input`
group / kiosk user); confirm with `libinput debug-events` showing the
touchscreen produces nothing while a `disable_touch` activity runs.
