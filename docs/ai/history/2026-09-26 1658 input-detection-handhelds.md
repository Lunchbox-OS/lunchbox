# Input-device detection on gaming handhelds (#236)

## The issue

> **Input method detection is broken**
>
> Post #207, I tried setting up an activity that requires a keyboard to be
> present. It was still visible and enabled even when the device (a gaming
> handheld) was undocked, meaning no keyboard is present.
>
> ```toml
> [[entries]]
> group = "creative"
> id = "tinkercad"
> label = "Tinkercad"
> internet = { required = true }
> browser = { mode = "kiosk", profile_id = "autodesk" , start_url = "https://www.tinkercad.com" , wipe_on_exit = false }
> requires_input = ["keyboard", "mouse"]
>
> [entries.kind]
> app_id = "com.google.Chrome"
> args = []
> type = "flatpak"
> ```

Prompt: "investigate #236", then "go with (b), make all four commits".

## What was found

#207 (launcher branding) was not involved: the engine still turns missing
devices into `RequiredInputUnavailable` and the launcher still renders it. The
engine really believed a keyboard and mouse were present, for three independent
reasons, plus a fourth that fails open.

The handheld is a Lenovo Legion Go S. Its `/proc/bus/input/devices` undocked,
run through the same capability checks `input_devices.rs` uses:

| Device | Bus | Counted as | Really |
|---|---|---|---|
| `AT Translated Set 2 keyboard` | i8042 | keyboard | EC's AT keyboard; atkbd declares a full keymap |
| `wch.cn Legion Go S` (USB if 0) | USB 1a86:e310 | keyboard | controller's keyboard emulation |
| `Legion Go S` (if 1, js0) | USB 1a86:e310 | gamepad | the controller |
| `wch.cn Legion Go S Mouse` (if 2) | USB 1a86:e310 | mouse | controller's trackpad/mouse emulation |
| `wch.cn Legion Go S Keyboard` (if 2) | USB 1a86:e310 | keyboard | controller's keyboard emulation |
| `wch.cn Legion Go S Touchpad` (if 2) | USB 1a86:e310 | nothing | indirect absolute pad |
| `NVTK0603:00 0603:F200` | I2C | touch | the touchscreen |

So all four types were always present and no `requires_input` could fail.

1. **Controller emulation.** Handheld controllers (Legion Go S, Steam Deck, ROG
   Ally) expose HID keyboard and mouse interfaces beside the gamepad on one USB
   device.
2. **The AT keyboard.** Present on x86 handhelds with nothing wired; on a laptop
   it is the real keyboard.
3. **Lunchbox's own uinput devices.** The HUD's page-turn keyboard (#160)
   declares keys 1–255 and lives as long as the HUD; the gamepad bridge's
   pointer+keyboard device lives as long as its activity.
4. **Reload.** `InputMonitor` was built only when the boot policy had a gated
   entry. An entry added by config reload got no detection report, and the gate
   fails open before the first report.

`journalctl -u lunchboxd` came back empty, which proves nothing: lunchboxd is
exec'd from `sway.conf`, not a systemd unit.

## Decisions

* Controller emulation (1) is recognised by sysfs: a keyboard/mouse node whose
  nearest ancestor with `idVendor` (the USB device, not the interface) is shared
  with a node that has `BTN_SOUTH`/`BTN_TRIGGER`. Only nodes on `BUS_USB` are
  grouped, because a Bluetooth HID device's ancestors lead to the adapter every
  Bluetooth device shares.
* For the AT keyboard (2) three options were offered: count only hot-pluggable
  buses (breaks laptops until a USB keyboard is attached), discount built-in
  keyboards on a machine that is evidently a handheld, or a config list of
  devices to ignore. The user chose the handheld heuristic. "Handheld" means an
  emulating controller (as in 1) on a USB device whose sysfs `removable` is not
  `removable`. An ordinary external pad emulates nothing, so a laptop with one
  keeps its keyboard. Known miss: a Steam Controller receiver in a laptop port
  whose firmware does not describe it (`unknown`) hides the laptop's keyboard
  while it is in. A config override was left for if that ever matters.
  i8042 pointers are discounted with the keyboard on a handheld, since the same
  reasoning applies.
* Own devices (3) are skipped by name, with the prefix now
  `lunchbox_bridge::VIRTUAL_DEVICE_NAME_PREFIX` so the names and the filter
  cannot drift.
* The monitor (4) now always runs (as the media prefetcher already did for the
  same reason), scans only while some entry sets `requires_input`, and re-scans
  on `PolicyReloaded`.

A mechanical commit ahead of these makes the scan record every device before
classifying, since (1) and (2) depend on the other devices present.

Not verified on the handheld itself: the dev machine is a QEMU VM. The
classification is unit-tested against the Legion Go S device set above.

## Follow-up in the same PR: other reload bugs of the same shape (#188, #243)

Prompt: "hm #188 seems related to keeping the input monitor alive -- does this
change address that too?", then "check to see if there are any other latent
bugs that have the same shape", then "file issues for 1-4 and the docs gap",
then "fix #188 and #243 -- do both here in this PR, maybe there's some logic
that can be shared between the implementations".

The audit compared everything lunchboxd derives from the boot policy against
what `handle_config_reload` refreshes. It filed #243 (Steam preload, readiness
gate and `[service.steam]`), #244 (global HUD orientation), #245
(`[service.display]`), #246 (auto-brightness poll interval) and #247 (settings
that need a restart, undocumented).

What the fixes share:

* `Policy::internet_check_targets()` gives one list of targets. The monitor
  probes it and the engine reports it, so the two can't drift apart. #188 was
  exactly that drift.
* `PolicyReloads` (`crates/lunchboxd/src/policy_reloads.rs`) wakes a
  background task on `PolicyReloaded`, treating a lagged subscription as a
  reload. Tasks re-read the policy from the engine when woken, so the engine
  stays the only place the policy lives. Both monitors use it.
* Steam has a different shape and doesn't use it: its gate has to be in place
  before the post-reload snapshot goes out. So boot and reload both call one
  `apply_steam_policy`, and the reload calls it under the same engine lock as
  the policy swap. A task woken afterwards would let the launcher briefly show
  the ungated entry.

Verified #188 end to end in the headless session. Boot with `[service.internet]
check = "tcp://127.0.0.1:9"` and `interval_seconds = 3600`, then edit the file to
a local listener: `service_state` reports it available within a second. Editing
it back reports `:9` unavailable and drops the old target. #243 is not verified
end to end: Steam isn't installed on the dev VM, and `LinuxHost` spawns a real
client, so there is no unit test either.

Left out: #188's suggestion of a "not checked yet" state for a target that has
not been probed. It changes the wire format and every display of the status.

## Follow-up: #244, #245, #246

Prompt: "fix #244 #245 #246 too".

`handle_config_reload` would have gone past clippy's argument limit, so a
refactor commit first gathers what a reload pushes settings into, as
`ReloadTargets` (Steam's host, the file manager, and now the HUD layout and the
display manager). Its `apply()` hands each one its part of the new policy.

* #244: `HudLayout` keeps the global edge and the per-activity override behind
  one lock, and both change through the same "announce only if the effective
  edge moved" step. A reload moves the HUD at once unless an activity has its
  own edge. In that case the new global takes effect when the session ends.
* #245: the `DisplayManager` is built whether or not docking is enabled. While
  it's disabled it ignores hotplug, mode requests and the HiDPI re-assert
  (checked under its apply lock). `configure()` enables it (reconciling what's
  connected), disables it (back to the internal display, mirror stopped, audio
  restored), or moves audio when `mirror_audio` changes while docked. Side
  effect: with docking disabled, the display state now names the primary, so
  the HUD anchors to the internal display.
* #246: the auto-brightness loop waits on `PolicyReloads` and rebuilds its
  ticker when the interval changes.

Verified #244 and #245 together in the headless session. I booted with
`docking_enabled = false` and `[service.hud] orientation = "top"` and added a
second output with `swaymsg create_output`: it was left alone. Editing the file
to enable docking and set `orientation = "left"` moved the HUD (the screenshot
shows it on the left with the display toggle present) and mirrored onto
HEADLESS-2 with wl-mirror running. Disabling docking again stopped wl-mirror and
went back to `single_internal` without rebuilding the HUD. #246 is untested: the
loop only runs with a light sensor, which the VM doesn't have.
