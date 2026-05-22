# Volume adjustment via physical volume buttons

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/52>

## Prompt

> Implement #52

Where #52 is:

> Volume adjustment via physical volume buttons
>
> This is specifically for volume buttons on the keyboard and device-specific
> volume controls (i.e. the volume buttons on a handheld).
>
> This will need to go through shepherdd to ensure that the volume limits are
> enforced

## Context

The HUD already had a slider that drove volume through shepherdd's existing
`SetVolume`, `ToggleMute`, and `SetMute` IPC commands. The pipeline that
enforces policy (`max_volume`, `min_volume`, `allow_mute`, `allow_change`)
lived in `Service::handle_command` for `SetVolume`/`ToggleMute`/`SetMute` and
in the parallel HTTP handler.

What did **not** exist was a way for physical XF86Audio* keypresses to drive
that same path. Sway saw the keys but had no binding for them, so the volume
buttons did nothing (or fell through to whichever app happened to be
focused).

## Approach

Match the existing sway-binds-CLI-binary pattern used for `--stop-current`:

1. **Protocol** — add `Command::VolumeUp { step }` and `Command::VolumeDown
   { step }` in `shepherd-api`. The relative form means shepherdd reads
   current volume + policy in one place, avoiding a get/set race over IPC
   and centralizing clamping.
2. **Service** — implement `Service::handle_relative_volume` in `shepherdd`.
   Same restrictions/`broadcast(VolumeChanged)` shape as `SetVolume`.
3. **CLI** — add `--volume-up [STEP]`, `--volume-down [STEP]`, and
   `--toggle-mute` one-shots to `shepherd-launcher`. Default step is 5%
   (matches what most desktops use for media keys).
4. **Compositor** — bind `XF86AudioRaiseVolume` / `XF86AudioLowerVolume` /
   `XF86AudioMute` in `sway.conf` to those one-shots, with `--locked` so
   they keep working when the screen is locked.

Notes:

- `--volume-up`/`--volume-down` is wired as `Option<u8>` with
  `num_args = 0..=1` and `default_missing_value = "5"`, so both
  `--volume-up` (default 5%) and `--volume-up 10` (explicit step) work
  from sway bindings or scripts.
- The CLI logs and exits cleanly on `VolumeDenied { reason }` rather than
  treating policy denials as errors — pressing the volume key while at
  the configured max should be a no-op, not a noisy failure.
- The HUD already subscribes to `VolumeChanged`, so the slider follows
  hardware-key presses without any HUD code changes.

## Files touched

- `crates/shepherd-api/src/commands.rs` — new variants + serialization
  round-trip test.
- `crates/shepherdd/src/main.rs` — dispatch + `handle_relative_volume`
  helper.
- `crates/shepherd-launcher-ui/src/main.rs` — CLI flags + dispatch +
  `send_volume_command` helper.
- `sway.conf` — three XF86Audio* bindings.
- `crates/shepherdd/README.md`, `crates/shepherd-launcher-ui/README.md` —
  doc updates.
