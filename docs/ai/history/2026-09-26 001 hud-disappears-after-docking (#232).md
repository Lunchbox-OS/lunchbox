# The HUD disappeared after docking (#232)

The prompt was "investigate potential root causes for #232". The issue had only
a title — "HUD disappears after docking/undocking a few times" — and the
maintainer added two things during the investigation:

> the bug only appears when using the external display as the primary via the
> button that switches this (i.e. not mirrored)

and a journal from the affected machine (a Lenovo with an amdgpu eDP-1 panel,
docked to a Dell U2717D on `DP-1`), with "look around the time that Tinkercad
was opened". After the findings below: "go ahead with step 2", which was
restarting the HUD when it exits.

## What the journal showed

The HUD was not hidden. It was dead:

```
10:57:51 sway: Destroying output eDP-1
10:57:51 lunchbox-hud[10201]: Error 71 (Protocol error) dispatching to Wayland display.
...
11:49:34 sway: Destroying output eDP-1
11:49:35 lunchbox-hud[26792]: Error 71 (Protocol error) dispatching to Wayland display.
```

GTK ends the process on any Wayland protocol error, and `sway.conf` started the
HUD once with `exec`, so it stayed gone for the rest of the session. The
Tinkercad launch at 11:49:37 was just when it was noticed.

"Destroying output eDP-1" is lunchboxd's `apply_external_only` disabling the
internal panel — the external-only toggle. Mirror mode never disables a panel,
which is why only the toggle showed the bug. Across two sway sessions in the
journal it was the **second** external-only switch of each that killed the HUD:

| sway session | 1st switch | 2nd switch (after undock → redock) |
|---|---|---|
| A | 10:47:23, HUD survives | 10:57:51, HUD dies |
| B | 11:13:56, HUD survives | 11:49:35, HUD dies |

The surviving and fatal switches look identical from lunchboxd and sway (same
operations, same order, the same amdgpu `REG_WAIT` warning each time), and the
error lands before lunchboxd broadcasts `ExternalOnly`, so the HUD's own
re-anchor timer (`app.rs`, the `set_monitor` dance) had not yet run. The error
is somewhere in how GTK and gtk4-layer-shell react to eDP-1's `wl_output`
disappearing, conditioned on state left from the previous cycle.

**What request was illegal is still unknown.** GTK logs only the generic line;
the `wl_display` error naming the object goes to GTK's Wayland log handler at
debug level. To capture it, run the HUD with `G_MESSAGES_DEBUG=Gdk` (or
`WAYLAND_DEBUG=client`) and repeat dock → external-only → undock → dock →
external-only.

## What did not reproduce it

`lunchbox dev headless` can add and remove outputs (`swaymsg create_output`,
`output HEADLESS-N unplug`), and `set_display_mode` can be called on
`dev-runtime/lunchbox.sock` directly. None of these killed the HUD:

- ~50 dock → external-only → undock cycles at 3s, 0.3s and 0.05s gaps;
- lunchboxd stopped (`SIGSTOP`) across the undock, holding the session at zero
  enabled outputs for 2s;
- HEADLESS-1 at scale 2 (the panel is HiDPI; the Dell is not);
- a minimal Python GTK4 layer-shell bar pinned to an output with a popover
  open, then that output disabled.

What the headless session cannot model: connector names are never reused
(`HEADLESS-2`, `-3`, … where the device always sees `DP-1`); its seat has no
pointer, so the HUD's own toggle — and the tooltip over it — cannot be used
(`seat - cursor` and `wlrctl` do not reach it); and real modeset timing.

Two things worth knowing if this is picked up again:

- gtk4-layer-shell 1.3.0 (Ubuntu 26.04's) ignores `zwlr_layer_surface_v1.closed`
  unless `respect_close` is set, and its own remap on monitor invalidation is
  skipped while there are no monitors. Upstream has since changed the
  closed-before-configure handling and added
  `test-monitor-destroyed-before-configure` (commits `980a578`, `6fa6610`,
  `1a230f1`), and fixed a use-after-free in window teardown.
- A headless session left idle for 120s has its output powered off by swayidle,
  after which screencopy (`grim`) fails. That is not a bug in anything here.

## What was changed

The root cause stays open, but the consequence was the part that hurt: a HUD
that dies for any reason takes away the only way a child can end an activity,
for the rest of the session. `sway.conf` now starts it in a loop that starts it
again whenever it exits.

- A restarted HUD already asks lunchboxd for the display arrangement, HUD
  orientation, scale and service state when it connects, so it comes back on
  the right output and at the right size, mid-activity included.
- The loop remembers the inode of the Wayland socket it started against and
  stops when that file is gone or replaced. Checking only for a socket by name
  let a loop orphaned by `dev stop` start a second HUD in the next `dev
  headless`, which reuses `wayland-1`; verified both ways.
- The first restart after any exit is a second later. A HUD that keeps dying
  within 10s of starting backs off, doubling to at most 10s; one that ran
  longer starts over at a second. Each exit is logged with its status. (The
  first version doubled to 30s and let one step reach 32s; see below for why
  it changed.)
- An activity at the kiosk uid can no longer get rid of the HUD by killing it.

Verified in `lunchbox dev headless`: `kill -9` on the HUD brings it back (on
the external output in external-only mode), repeated kills back off, and
`dev stop` followed by `dev headless` leaves exactly one HUD and no old loop.
`lunchbox install sway-config` rewrites the binary path in the new line like
the others.

### Restarts under the XWayland HiDPI workaround

Asked whether this had been tested with the XWayland workaround and other
DPIs: at the time it had not. Tested afterwards, headlessly, with a config
adding an `xwayland_native_resolution = true` entry that runs `sleep`, at
output scales 1, 1.25, 1.5 and 2. The HUD was measured in physical pixels at
rest, during the activity (sway at 1.0, the HUD counter-scaling), after a
`kill -9` mid-activity, after the activity ended, and after a `kill -9` at
rest:

| scale | rest | activity | killed mid-activity | after | killed at rest |
|---|---|---|---|---|---|
| 1    | 50  | 50 | 50 | 50  | 50  |
| 1.25 | 62  | 62 | 62 | 62  | 62  |
| 1.5  | 75  | 74 | 74 | 75  | 75  |
| 2    | 100 | 98 | 98 | 100 | 100 |

A HUD restarted mid-activity logs `Seeded HUD scale factor` with the
activity's factor and comes back at exactly the size it had before the kill.
The 1–2px difference between rest and activity at 1.5 and 2 is there before
any restart. It is the counter-scale's rounding, not something the restart
introduced.

The prompt then was "test all of them" — the four gaps listed at the time: the
confirm prompt and flyouts on a restarted HUD (issue #118's concern), a side
(`Left`) orientation, a real XWayland client, and a restart while docked with
outputs at different scales.

### The rest of the matrix

The HUD's debug hook (`LUNCHBOX_HUD_DEBUG_CONFIRM_TRIGGER`, which the harness
forwards) opens the confirm prompt (`<path>`) and the volume flyout
(`<path>.volume`). The brightness flyout could not be covered: it is hidden on
a host without a backlight, which the headless session is. Each popup was
measured as the bounding box of what it added to the screen, with anything
that also changed while it was closed (the launcher's loading spinner) masked
out. The pairing card, a separate client that re-lays itself out, was killed
first. Where a number looked off, the screenshots were compared by eye.

- **Confirm prompt and volume flyout** (top bar, `sleep` under the workaround,
  scales 1, 1.25, 1.5, 2): the same size before and after a restart mid-activity,
  within 1–2px of spinner noise, and identical by eye. At rest the flyout
  measured 285×79, 357×99, 428×119 and 570×150 at the four scales, the same
  before and after a restart.
- **A real XWayland client** (a GTK window forced onto `GDK_BACKEND=x11`,
  which reports `X11Display`, under the workaround at 1.5 and 2): it gets the
  panel's native 1280px width. After the HUD restarts, the window keeps its
  size and focus, so the exclusive zone came back the same. Bar, flyout and
  prompt matched before and after.
- **Left orientation**, device-wide (`[service.hud] orientation = "left"`, at
  1.5 and 2, at rest and under the workaround), and per activity (a top-bar
  device running an entry with `hud_orientation = "left"`, the HUD killed while
  on the side): the restarted HUD logs `Seeded HUD orientation
  orientation=Left` with the activity's scale, and comes back on the side at
  the same size. It returns to the top when the activity ends.
- **Docked, internal at scale 2 and external at 1**: restarts in Mirror (on the
  internal, 100px), in external-only (on the external, 50px), and mid-activity
  in external-only all came back as they were. So did three races where the
  HUD was dead across the change: a switch to Mirror, a switch to
  external-only, and an undock (back on the internal at 100px).

### What the matrix changed

The docked races first failed, because the HUD was not back yet. Every kill in
the test came less than 30s after the previous restart, so each counted as a
crash loop, and the delay had climbed 2, 4, 8, 16, **32**s. That also broke
the documented 30s cap: the loop doubled before checking it. For the one
control that ends an activity, half a minute of nothing is too long whatever
the cause. The loop now waits a second before the first restart, doubles only
across repeated quick deaths, and caps at 10s with a 10s reset threshold.
Measured with back-to-back kills: 1.1, 2.1, 4.1, 8.1, 10.0 and 10.1s, then
1.1s after a HUD that had run 11s.

### Found on the way, not caused by this

After external-only → Mirror (no dock change in between), workspace 1 with the
launcher stays on the external display. The internal gets a new, empty
workspace 2, and wl-mirror mirrors that onto the external, so the internal
shows only the HUD over black. It reproduces with the HUD never killed. It is
in lunchboxd's display handling (the Mirror branch of `apply` in
`display.rs` re-enables the primary and pins wl-mirror to the external, with no
step that moves the workspace back), not in the HUD. Filed as #249.
