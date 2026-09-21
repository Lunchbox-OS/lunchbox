# lunchbox-hud

Always-visible HUD overlay for Lunchbox.

## Overview

`lunchbox-hud` is a GTK4 layer-shell overlay that remains visible during active sessions. It provides essential information and controls that must always be accessible, regardless of what fullscreen application is running underneath.

## Features

- **Time remaining** - Authoritative countdown from the service
- **Battery level** - Current charge percentage and status
- **Volume control** - Adjust system volume with enforced limits. The bar carries the icon; the slider, the mute toggle and the percentage open out of it as a flyout (see [Pop-out controls](#pop-out-controls-issue-178)).
- **Brightness control** - Adjust screen brightness on hosts with a backlight (hidden otherwise). Same flyout shape, with the automatic-brightness toggle in place of mute.
- **Session controls** - End session button (confirms first unless the activity opts out via `confirm_on_close = false`)
- **Reset** - Restart an activity in place, for kinds that offer it (`type = "retroarch"`)
- **Page turning** - `‹` / `›` for reading activities (`type = "ebook"`), which synthesize `Page Up` / `Page Down` through `/dev/uinput`. They exist because a touchscreen cannot turn a page any other way: readers bind paging to keys, a D-pad or a wheel, and have no swipe gesture. The HUD is where they belong — it is on the overlay layer, above the activity, and it is Lunchbox's own surface rather than something a reader's own restrictions could take away. See `src/page_turn.rs` and `docs/ebooks.md`.

  On the vertical bar the pair stacks at the bottom, but the arrows keep
  pointing `‹` back and `›` forward. They point the way the *pages* go, not the
  way the buttons are stacked: the direction a reader thinks in does not rotate
  when the bar does, and a child who learns `›` on one device should not have
  to learn it again on another.

  A reading session used to be the one case where the bar ran out of room, and the sliders and percentages were shortened and hidden to pay for these two buttons. Since the sliders moved into flyouts (issue #178) the bar has the room and nothing has to yield. The activity name still ellipsizes rather than pushing the end-session button off the end, which GTK clips rather than wraps.
- **Power controls** - Suspend, shutdown, restart
- **Warning display** - Visual and audio alerts for time warnings

## Architecture

```
┌───────────────────────────────────────────────────────┐
│                 Sway / Wayland Compositor             │
│                                                       │
│  ┌──────────────────────────────────────────────────┐ │
│  │            HUD (layer-shell overlay)             │ │
│  │  [Battery] [Volume] [Time Remaining] [Controls]  │ │
│  └──────────────────────────────────────────────────┘ │
│                                                       │
│  ┌──────────────────────────────────────────────────┐ │
│  │         Running Application (fullscreen)         │ │
│  │                                                  │ │
│  │                                                  │ │
│  │                                                  │ │
│  └──────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────┘
```

The HUD uses Wayland's **wlr-layer-shell** protocol to remain above all other surfaces.

## Usage

### Running

```bash
# With default socket path
lunchbox-hud

# With custom socket path
lunchbox-hud --socket /run/lunchboxd/lunchboxd.sock

# Custom position and size
lunchbox-hud --anchor top --height 48
```

### Command-Line Options

| Option | Default | Description |
|--------|---------|-------------|
| `-s, --socket` | `$XDG_RUNTIME_DIR/lunchboxd/lunchboxd.sock` | Service socket path |
| `-l, --log-level` | `info` | Log verbosity |
| `-a, --anchor` | *(unset)* | **Pin** the HUD to `top`, `bottom`, or `left`, ignoring config. Also readable from `LUNCHBOX_HUD_ANCHOR`, which is how the headless dev session drives it. Unset — how `sway.conf` starts the HUD — the edge comes from lunchboxd instead (see below). |
| `--height` | `48` | HUD bar thickness in pixels — its height when horizontal, its width when it runs down the side |

## Display Elements

### Time Remaining

Shows the countdown timer for the current session:

- `MM:SS` format for times under 1 hour
- `H:MM:SS` format for longer sessions
- Visual emphasis when below warning thresholds
- Shows "∞" for unlimited sessions

### Battery

Displays current battery status:

- Percentage (0-100%)
- Charging/discharging indicator
- Data sourced from UPower (not the service)

### Volume

Shows and controls system volume:

- Current level (0-100%)
- Mute indicator
- Click the icon to open the flyout, then drag to adjust (sends commands to service)
- Volume maximum may be restricted by policy

### Controls

- **End Session** - Stops the current session (if allowed)
- **Power** - Opens menu with Suspend/Shutdown/Restart

## Pop-out controls (issue #178)

The volume and brightness sliders are not in the bar. Each bar icon opens a
flyout — a `GtkPopover` parented to that icon — holding a toggle, the slider
and the percentage, the way a tray volume control behaves on any desktop.

They were inline until #178. A pair of inline sliders is ~200 logical pixels of
a 1280px bar and **over a third of the minimum height of the vertical one**,
which is what left a reading session on a short screen with nowhere to put the
page-turn buttons: the bar clipped its bottom, taking the activity title and
both page buttons with it, on anything under about 720 logical pixels tall.
Moving them out costs the bar a 32px icon apiece and buys back everything else.

Four consequences worth knowing:

- **The row is horizontal in both layouts.** A popover is its own surface, so
  it does not inherit the bar's axis. That retired every `.hud-vertical` slider
  rule, both `set_inverted` calls, and the axis branch in the slider sizing.
- **Both percentages are back everywhere.** They were dropped from the vertical
  bar ("100%" does not fit across 48px) and from a reading session (no room);
  the flyout has room in every case, so neither exception survives.
- **The toggles moved with the sliders.** The bar icons *were* the mute and
  automatic-brightness controls; they are now openers, so mute and automatic
  live at the head of their flyout. The bar keeps reporting the manual state
  via `.brightness-manual`, so a glance at the bar still says who is driving
  the backlight.
- **Their handlers are on `clicked`, not `toggled`.** `gtk_toggle_button_set_active`
  emits `toggled`, so a handler there would echo an RPC every time the update
  loop pushed the real state back into the button — which is what the old
  `auto_updating` guard existed to suppress. `clicked` is only ever the user.
  Measured, not assumed: flapping `set_active` from the timer for 15s produced
  20 `toggled` and 0 `clicked`.

Like the confirm prompts, a flyout is **rebuilt on every `HudScaleChanged`**
rather than restyled: it lives hidden across the change and is measured (by
`align_popover_to_button`) just before being shown, which is exactly the case
issue #118 says a fresh widget is required for. The 500ms timer re-pushes
value, range and sensitivity every tick, so a rebuilt flyout has live state
back long before anyone can open it.

## Event Handling

### Warnings

When the service emits a `WarningIssued` event:

1. Visual banner appears on the HUD
2. Time display changes color based on severity
3. Optional audio cue plays
4. Banner auto-dismisses or requires acknowledgment

Severity levels:
- `Info` (e.g., 5 minutes remaining) - Subtle notification
- `Warn` (e.g., 1 minute remaining) - Prominent warning
- `Critical` (e.g., 10 seconds remaining) - Urgent, full-width banner

### Session Expired

When time runs out:

1. "Time's Up" overlay appears
2. Audio notification plays
3. HUD remains visible until launcher reappears

### Disconnection

If the service connection is lost:

1. "Disconnected" indicator shown
2. All controls disabled
3. Automatic reconnection attempted
4. **Time display frozen** (not fabricated)

## Styling

The HUD is designed to be:

- **Unobtrusive** - Small footprint, doesn't cover content
- **High contrast** - Readable over any background
- **Touch-friendly** - Large touch targets
- **Minimal** - Icons over text where possible

## Where the edge comes from (issue #171)

The HUD does not read `config.toml`; lunchboxd does. So the edge arrives over
IPC, by the same two-part mechanism as the scale factor:

- `HudOrientationChanged` is broadcast when the **effective** edge moves — an
  activity with its own `hud_orientation` starts, or one ends and the global
  `[service.hud]` setting takes over.
- `get_hud_orientation` is asked on **every connect**, because that event fires
  only on change. A HUD that started late or reconnected mid-session would
  otherwise sit on the wrong edge, with its exclusive zone reserved on the
  wrong side of the activity, for the rest of the session. This is the same
  hole issue #118 found for the scale factor.

Passing `--anchor` **pins** the bar and makes the HUD ignore both. That is what
makes `LUNCHBOX_HUD_ANCHOR=left` useful in development, and a footgun on a
device — `sway.conf` deliberately passes no flags.

Unpinned, the bar starts at `top` and follows lunchboxd from there, so a device
configured for a side bar shows a top bar for the fraction of a second before
the first connect. That is the deliberate trade: a HUD that waits for the
daemon before showing itself is a HUD a child cannot end a session from when
the daemon is slow or down.

### Changing edge at runtime

An orientation change **rebuilds the bar** rather than restyling it, for the
reason issue #118 documents: GTK validates a widget's style when it is
*mapped* and leaves it alone while hidden, so anything currently hidden — the
confirm prompts, the warning — would keep the previous layout's sizes and paint
at them the next time it is shown. A fresh widget has no cached style.

Two consequences worth knowing before touching `build_hud_content`:

- **Every timer it registers is generation-guarded.** The rebuild bumps a
  shared counter; each timer compares it to the generation it was built as and
  returns `ControlFlow::Break` when they differ. Without that, each rebuild
  would leave another 500ms timer driving widgets that are no longer on screen.
  Anything new that registers a timer needs the same guard.
- **`HudContent::teardown` exists because popovers are not box children.** A
  `GtkPopover` attached with `set_parent` must be unparented explicitly, and
  the confirm prompts are themselves rebuilt on every scale change — so the
  teardown reads the *current* popover through the `Rc<RefCell<..>>` rather
  than one captured at build time.

Changing anchors on an already-mapped layer surface does not move it; the
surface has to be rebuilt with an unmap → reconfigure → remap, the same dance
the output switch uses.

## The vertical HUD (`--anchor left`, issue #171)

On hardware or activities where a strip down the side costs less of the screen
than a bar across the top, the HUD runs vertically. The specification is "the
HUD rotated 90 degrees to the left", and the code takes that literally: same
widgets, same update loop, same stylesheet, with the flow axis swapped and the
order reversed.

The reversal is the whole of the layout difference and lives in one place —
`HudOrientation::flow_append`, which prepends instead of appending. Rotating
the bar to the left maps its **right** end to the **top** of the screen, so the
end-session button that sits at the far right lands at the top, and the
page-turn buttons that sit at the far left land at the bottom. Nothing else in
the construction code is reordered, so the two layouts cannot drift apart.

A group *within* the bar (a mute button and its slider) follows the bar's axis
but is **not** reversed: an icon labels the control it sits above.

Three things are genuinely different rather than rotated, because rotation
alone would not work:

- **The activity title** (`rotated_label.rs`). GTK4 removed
  `gtk_label_set_angle`, so this is a `GtkWidget` subclass that wraps a real
  `GtkLabel`, swaps the axes in `measure`, and hands the child a
  `translate(0, height) · rotate(-90°)` in `size_allocate`. Keeping a real
  label is what preserves the CSS-driven font size *and* the `EllipsizeMode`
  that stops a long book title pushing the end-session button off the bar.
- **The wall clock** (`ClockFace`, in `lunchbox-widgets`). `HH:MM` is wider
  than the bar, and a clock read sideways is worse than none, so it becomes a
  round face — the one form of a clock as wide as it is tall. It started here
  and moved out to the shared crate when the launcher wanted one too, on a
  compartment's floor (review on #208); it is drawn rather than styled, so the
  bar passes its size in (alongside the icon `set_pixel_size` calls in the
  scale timer) and names its colour in CSS like everything else.
- **The warning banner** (`WarningBanner` in `app.rs`). Warning text is
  operator-authored prose of no fixed length, a 48px bar cannot hold a sentence
  laid out horizontally, and GTK clips rather than wraps. So the bar keeps the
  icon — which carries the severity colour and the critical blink on its own —
  and the message drops out of it as a popover.

Two further notes:

- **Axis-specific CSS has to be swapped, not inherited.** Rules written for a
  horizontal bar name one axis — 80px of slider length, a 4px-thick trough,
  `padding: 0 4px` separating a control group. Left alone on a vertical bar
  they are demanded *across* the bar, and the surface measured **124px** wide
  instead of the intended 48. The `.hud-vertical` rules in `CSS_TEMPLATE` turn
  each of them; anything axis-specific added later needs the same treatment.
- **Swap the axis, but never restate the length.** A CSS minimum is a *floor*
  that GTK takes the maximum of against the widget's own size request, so the
  `min-height: 80px` the swapped slider rule originally carried outranked the
  shorter request a reading session asks for. The entire #160 overflow response
  was inert on the vertical bar because of it, and the page-turn buttons were
  clipped off the bottom on any screen under about 720 logical pixels tall
  (issue #178). The sliders have since left the bar entirely, so that rule is
  gone — but the trap applies to anything else given a swapped rule *and* a
  size request.

### The bar is thicker than its exclusive zone, in both layouts

`--height 48` sets the layer-shell **exclusive zone**. The surface itself is
sized to its content, and the content is thicker than that: an
`.indicator-button` is 32px plus its own 4px padding plus the bar's 6px, so
both bars actually render **54px** and overhang their reserved zone by 6px.
That is pre-existing behaviour, not something the vertical layout introduced —
the note on `.page-button` about a taller child pushing the window past the
exclusive zone is the same effect. The vertical bar was deliberately brought to
the same 54px rather than to a nominal 48.

## HUD scale factor (the XWayland DPI hack)

While an `xwayland_native_resolution` activity runs, lunchboxd drops every sway
output to `scale 1.0` and broadcasts the captured pre-launch scale as
`HudScaleChanged { factor }` (issue #45). The HUD is layer-shell and lives in
logical pixels, so it counter-scales by that factor to keep its physical size.

The event only fires on *change*, so the HUD also fetches the current factor with
`get_hud_scale` on every (re)connect — otherwise a HUD that started late, or
whose connection dropped mid-activity, would render un-counter-scaled for the
rest of the session with nothing to correct it (issue #118).

**The rule for anything you add to the HUD:** a dimension only follows the factor
if it is either

1. an `Npx` literal in `CSS_TEMPLATE` — `apply_scale` multiplies every one of
   them (`scale_px_literals`), or
2. rescaled explicitly in the 500ms timer in `build_hud_content`, next to the
   icon `set_pixel_size`, slider `width_request`, and `gtk4::Box` spacing calls.

Anything else — a size the GTK theme supplies, or a widget property left at its
constructor value — keeps its logical-pixel value and renders 1/factor too small
on a HiDPI panel. That is issue #114 and its follow-ups; note that a theme rule
on the element (e.g. `button { font-size }`) beats an inherited value, so a size
"inherited from the root" is not scaled unless the more specific rule states it.

One further trap, from #118: GTK validates a widget's style when it is **mapped**
and leaves it alone while hidden. A widget that is hidden across a
`HudScaleChanged` therefore keeps the *previous* factor's style — it measures,
and can paint, at the old size while the always-mapped bar around it is already
correct. Re-rooting it does not clear that; only building it fresh does (a new
widget has no cached style and takes the current stylesheet immediately). Hence
`build_confirm_prompt`, which is called again on every scale change instead of
restyling the prompt in place. Anything else that lives hidden across a scale
change needs the same treatment.

## Layer-Shell Details

```rust
// Layer-shell configuration
layer: Overlay           // Always above normal windows
anchor: Top              // Attached to top edge
exclusive_zone: 48       // Reserves space (optional)
keyboard_interactivity: OnDemand  // Only when focused
```

## State Management

The HUD maintains local state synchronized with the service:

```rust
struct HudState {
    // From service
    session: Option<SessionInfo>,
    volume: VolumeInfo,
    
    // Local
    battery: BatteryInfo,    // From UPower
    connected: bool,
}
```

**Key principle**: The HUD never independently computes time remaining. All timing comes from the service.

## Dependencies

- `gtk4` - GTK4 bindings
- `gtk4-layer-shell` - Wayland layer-shell support
- `tokio` - Async runtime
- `lunchbox-api` - Protocol types
- `lunchbox-ipc` - Client implementation
- `upower` - Battery monitoring
- `clap` - Argument parsing
- `tracing` - Logging

## Building

```bash
cargo build --release -p lunchbox-hud
```

Requires GTK4 development libraries and a Wayland compositor with layer-shell support (e.g., Sway, Hyprland).
