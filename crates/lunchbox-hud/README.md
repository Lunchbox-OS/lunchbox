# lunchbox-hud

Always-visible HUD overlay for Lunchbox.

## Overview

`lunchbox-hud` is a GTK4 layer-shell overlay that remains visible during active sessions. It provides essential information and controls that must always be accessible, regardless of what fullscreen application is running underneath.

## Features

- **Whose device this is, and what it is doing** - the mark and the wordmark while nothing is running; the running activity's own keylined icon and name, in the mark's place, while something is (issue #209, [Styling](#styling)).
- **Time remaining** - Authoritative countdown from the service, in words: `12:40 left`
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
- **Warning display** - A three-second toast and an audio cue when a time warning fires ([Warnings](#warnings))

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
| `--height` | `48` (`space.hud-h`) | HUD bar thickness in pixels — its height when horizontal, its width when it runs down the side. The default is the branding's own token, not a number chosen here (issue #209). |

## Display Elements

### Time Remaining

Shows how long is left in the current session, in words — `12:40 left` (issue
#209). It used to be a bare monospace `MM:SS`, which is a stopwatch: a thing
that counts, with no opinion about what the number is for.

- `M:SS left` under an hour, `H:MM:SS left` above it. The leading unit loses its
  zero, because that is how the time would be said out loud.
- Cream until a warning fires, then that warning's own colour for the rest of
  the session — yellow, or red and blinking (see [Warnings](#warnings)). It has
  no opinion about the clock of its own.
- In the middle of the bar, the way a desktop shell puts the clock there — and
  the one thing that ever takes its place is a warning (see
  [Warnings](#warnings)).
- Nothing at all when there is nothing to count: no session, or an activity with
  no time limit.
- The vertical bar keeps its own three-character format (`90m`, `1h`), which has
  no room for the word — see `format_compact`.

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

### Popovers

Three of them — the two flyouts and the close confirmation, plus the message the
vertical bar cannot fit in itself. All four are cream panels with a 4 px ink
keyline, and none of them has GTK's arrow: `set_has_arrow(false)`, because a
popover here drops from the control that opened it and has nowhere else it could
have come from, and the wedge is the one part of a panel a keyline cannot follow.

They stand **8 px** off the control instead, which is a little less than the
wedge reserved and enough to keep two dark edges from touching. That offset
scales with the HUD factor, and goes on the axis the popover drops along.

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

When the service emits a `WarningIssued` event the bar raises a **toast**: a
pill with an ink outline, in the middle of the bar, **in the countdown's own
place** and for ten seconds (issue #209). It used to be a banner beside the
countdown that stayed for the rest of the session.

Standing where the countdown stands is the point: a warning is the only thing on
this bar more important than how long is left. Ten seconds rather than the
brief's three, because the time remaining is *away* for that long and three
seconds is short for a sentence a new reader is sounding out.

Nothing persistent is lost by it — **the countdown keeps that warning's own
colour for the rest of the session**. Three configurable severities, two
appearances, and one mapping between them (`theme::Urgency`) that the toast and
the countdown both go through, so they cannot disagree:

| Severity | The toast | The countdown, from then on |
| --- | --- | --- |
| `Info`, `Warn` | Yellow pill | Yellow |
| `Critical` | Red pill, blinking | Red, blinking |

The red is `color.alert`, and a critical time warning is the only thing on this
bar allowed to wear it (see [Styling](#styling)). It keeps the blink as well: a
colour says "this is different" and a blink says "now". `Info` is loud rather
than quiet on purpose — an operator who configures a warning at all wants the bar
to change — so what separates it from `Warn` is what it says.

The consequence worth knowing: **an activity with no warnings configured has a
cream countdown all the way down.** The bar says what the policy says.
`config.example.toml` ships `[[service.default_warnings]]`, so a device built
from it is unaffected.

A second warning raises a second toast rather than extending the first — the
timer is read from `warning_issued_at`, which the state records once per event.

On the **vertical** bar the message has nowhere to go (a sentence will not fit
across the bar, and GTK clips rather than wraps), so the bar keeps the icon and
the message drops out of it as a popover wearing the same pill colours.

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

Everything visual comes from `assets/branding/tokens.json` by way of
[`lunchbox-branding`](../lunchbox-branding/README.md), which the launcher uses
too (issues #207 and #209). `src/theme.rs` is this crate's half of it: the
stylesheet reaches the tokens through `@name@` placeholders, and two tests hold
the line in both directions — no placeholder may survive into GTK, and no hex
may appear that the token file does not define.

The bar is ink, opaque, 48 px, with cream type set in **Baloo 2** at 18/700.
Read the note at the top of `src/theme.rs` before changing a colour; the two
things it settles that a reader will otherwise trip over are:

- **There is exactly one red, and it means a critical time warning.** The
  hand-off had none; `color.alert` (`#FF5A4E`) was added to the token file for
  this bar, chosen on contrast against the near-black rather than by eye — 5.99:1
  as type on the bar, 5.60:1 for the ink set on it, so the countdown and the
  toast's fill can be the same colour. Nothing else is red: the end-session "X"
  is cream like every other control (what guards it is the confirmation, issue
  #78) and offline is putty. A test holds that line.
- **The controls the brief never drew** — brightness, the flyouts, network,
  display, lock, reset, log out, the confirm prompt, the administrator taskbar,
  the page turners, the clock — keep their positions and their glyphs and take
  the palette, the type and the chip: 6 px corners, a cream wash on hover,
  yellow when a toggle is engaged. A popover is a cream panel with an ink
  keyline, because it is a surface over the activity rather than part of the
  bar.

What the bar carries, in the two states §8 of the branding brief draws:

| | The bar |
| --- | --- |
| Idle | `[mark] Lunchbox` … `3:00 PM` … `[vol] [batt] 97% [×]` |
| In an activity | `[app icon, keylined] Celeste` … `12:40 left` … `3:12 PM [vol] [batt] [×]` |
| A warning | `[app icon] Celeste` … `⚠ 1 minute remaining!` … `3:12 PM [vol] [batt] [×]` |
| Administrator mode | `[mark] [window] [window]` … `3:12 PM` … `[vol] [batt] [lock] [×]` |

The middle column holds exactly one thing, in that order of precedence: a
warning, else the countdown, else **the wall clock**, which moves in from the
end of the bar whenever the other two are absent — which is most of the time,
because most of the time nothing is running. The clock is moved rather than
duplicated, so there is only ever one of it to keep wound.

It is centred against the **whole bar**, not against what is left between the
two side groups, so the countdown does not drift as the activity's name grows. That is why the bar is a `CenterBox` rather than a `Box`. When the
sides genuinely do not fit — a long name, a narrow screen, the page-turn buttons
— the centre gives way rather than anything being clipped.

The mark is `assets/branding/icon/lunchbox-mono-white.svg`, compiled into
`lunchbox-branding` and rasterized at whatever size the current HUD scale factor
asks for. One colour, with its compartments knocked out in the bar's own
`color.hud`: the colour version was what the bar wore first, and at this size
its four colours come to a few dozen pixels each and read as a smudge.

It is drawn at **the activity icon's size**, because the two stand in the same
place and never at the same time — the mark while nothing is running, the
activity's own icon while something is. §8 says 26 px; that was a detail of the
bar rather than the thing the bar belongs to, next to a wordmark set in 18 px
type.

The one place it appears during a session is administrator mode, where it is the
taskbar's home button — a shell's home button is the thing the shell is called.
That taskbar is otherwise unreachable from a development session, so debug builds
take `LUNCHBOX_HUD_DEBUG_FORCE_ADMIN_MODE=1` to look at it (issue #154); it makes
the HUD believe the mode is on and grants nothing, since lunchboxd refuses every
action of the mode unless lunchboxd agrees it is on. The app icon is 34 px, drawn by
`lunchbox_widgets::IconArt` — the same widget the launcher draws its 64 px icons
with, keylined in cream here because an ink keyline on an ink bar would be no
keyline at all.

Still missing from §8: **the jar pill**, and the "Celeste is ready" toast that
goes with it. The HUD has no concept of token gates yet; that is its own issue.

Underneath the branding the old rules still hold — a small footprint that does
not cover content, contrast against whatever is behind it, touch-sized targets,
and icons over text where an icon will do.

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
  instead of the intended 56. The `.hud-vertical` rules in `CSS_TEMPLATE` turn
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
sized to its content, and the content comes to a little more than that: an
`.indicator-button` is 32px plus its own 4px padding plus the bar's 4px, which
is 48, and the GTK theme's own 1px button border on each side takes it to
**50** — measured, on the virtual output, in both orientations. So the bar
overhangs what it reserved by 2px and the top of the activity behind it is under
the bar.

That is pre-existing behaviour rather than something the branding introduced;
what changed is the size of it. Before the branding, 6px of bar padding against
the same 48px zone rendered 54px and overhung by 6. The note on `.page-button`
about a taller child pushing the window past the exclusive zone is the same
effect, and is why the page-turn buttons grow only sideways.

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

1. an `Npx` literal in `CSS_TEMPLATE` (`src/theme.rs`) — `apply_scale`
   multiplies every one of them (`lunchbox_branding::scale_px_literals`), or
2. rescaled explicitly in the 500ms timer in `build_hud_content`, next to the
   icon `set_pixel_size`, slider `width_request`, and `gtk4::Box` spacing calls.
   The mark is here too, and is re-*rasterized* rather than re-measured: it is a
   texture from an SVG, so it is redrawn at the size it will be shown at.

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
- `lunchbox-branding` - The design tokens, the mark, and the two text passes the stylesheet is built with
- `lunchbox-widgets` - The clock face and the keylined activity icon, both shared with the launcher
- `upower` - Battery monitoring
- `clap` - Argument parsing
- `tracing` - Logging

## Building

```bash
cargo build --release -p lunchbox-hud
```

Requires GTK4 development libraries and a Wayland compositor with layer-shell support (e.g., Sway, Hyprland).
