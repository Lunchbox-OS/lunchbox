# lunchbox-launcher-ui

Main launcher grid interface for Lunchbox.

## Overview

`lunchbox-launcher-ui` is the primary user-facing shell for the Lunchbox kiosk environment. It presents a grid of available entries (applications, games, media) and allows users to launch them. 

This is what users see when no session is active—the "home screen" of the environment.

## Features

- **The field** - One sunk compartment per category, activities inside them
- **Time badges** - What a category or activity has banked, needs, or earns
- **Availability display** - Locked activities stay visible, wearing their badge
- **Launch requests** - Send launch commands to the service
- **State synchronization** - Always reflects service's authoritative state

## Architecture

```
┌───────────────────────────────────────────────────────┐
│                 Sway / Wayland Compositor             │
│                                                       │
│  ┌──────────────────────────────────────────────────┐ │
│  │            Launcher UI (fullscreen)              │ │
│  │                                                  │ │
│  │  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐     │ │
│  │  │        │ │        │ │        │ │        │     │ │
│  │  │Minecraf│ │GCompris│ │ Movies │ │ Books  │     │ │
│  │  │        │ │        │ │        │ │        │     │ │
│  │  └────────┘ └────────┘ └────────┘ └────────┘     │ │
│  │                                                  │ │
│  │  ┌────────┐ ┌────────┐                           │ │
│  │  │        │ │        │                           │ │
│  │  │ ScummVM│ │Bedtime │                           │ │
│  │  │        │ │        │                           │ │
│  │  └────────┘ └────────┘                           │ │
│  │                                                  │ │
│  └──────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────┘
```

## Usage

### Running

```bash
# With default socket path
lunchbox-launcher

# With custom socket path
lunchbox-launcher --socket /custom/path/lunchboxd.sock
```

### Command-Line Options

| Option | Default | Description |
|--------|---------|-------------|
| `-s, --socket` | `$XDG_RUNTIME_DIR/lunchboxd/lunchboxd.sock` | Service socket path |
| `-l, --log-level` | `info` | Log verbosity |
| `--stop-current` | — | One-shot: send `StopCurrent` to lunchboxd and exit |
| `--screen-off` | — | One-shot: blank the displays unless an activity is up (swayidle `timeout`) |
| `--screen-on` | — | One-shot: wake the displays (swayidle `resume`) |
| `--volume-up [STEP]` | step `5` | One-shot: send `VolumeUp` to lunchboxd (XF86AudioRaiseVolume binding) |
| `--volume-down [STEP]` | step `5` | One-shot: send `VolumeDown` to lunchboxd (XF86AudioLowerVolume binding) |
| `--toggle-mute` | — | One-shot: send `ToggleMute` to lunchboxd (XF86AudioMute binding) |

The volume one-shots are wired up in `sway.conf` so that pressing
`XF86AudioRaiseVolume` / `XF86AudioLowerVolume` / `XF86AudioMute` on a
keyboard or handheld device's hardware volume buttons goes through
lunchboxd. The service applies the configured `[volume]` policy
(`max_volume`, `min_volume`, `allow_mute`, `allow_change`) and broadcasts
a `VolumeChanged` event so the HUD slider follows the change.

## Field Behaviour

The home screen is a row of **compartments**, one per category, in config order
(`src/field.rs`). Entries belonging to no category fall into a trailing
"Everything else". A category with nothing to show is not drawn at all.

Lunchbox is a sectioned tin, and the launcher is that tin at screen size: an
enamel field with cream compartments sunk into it. The design tokens and the
mark live in `assets/branding`; `src/theme.rs` is their Rust half.

### Compartments

Each compartment shows its category's name, at most one badge, its members in
columns that spill *rightwards*, and — when the category is on a schedule —
its closing time today on the floor, behind a little clock face
whose hands point at that hour (`ClockFace`, from `lunchbox-widgets`, shared
with the HUD's vertical bar). A child who cannot yet read "6:00 PM" can still
see where the hand is going to be.

Height never grows: a category with more members gets wider, and if the row
overflows the screen the row scrolls **horizontally**. The field never scrolls
vertically.

A category only claims the height its own items need, so the field is a row of
**columns** rather than of compartments: two short categories stand one above
the other in a single column instead of each wasting most of a screen
(`compartment::pack_into_slots`). The packing is greedy and strictly in config
order, so reading a column downwards and then moving right reads the categories
in the order the configuration lists them; a cleverer fit would have to shuffle
them, and a home screen whose sections move about when one of them gains an
activity is worse than one with a gap in it. Everything in a column is as wide
as the widest of them, and the column fills the field's height, so the row
still reads as one tin.

When the row misses fitting the screen by **less than half a cell**, the cells
give up the difference instead of the field scrolling
(`LauncherField::squish_to_fit`). Scrolling is the right answer for a row that
genuinely does not fit; it is a poor one for a row forty pixels too wide, where
the child gets a chevron, a fade and a gesture to learn in order to reach a
strip of screen narrower than an icon. Above half a cell the row really is too
big and scrolling is what it is for.

Squishing a cell takes three things, because a GTK minimum is the largest of
everything that asks for one: a **ceiling** on the natural width (a size
request is a floor, and the name's own `NAME_MAX_CHARS` is what makes a cell as
wide as it is), the cell's own request, and the name's request — the floor
*inside* the cell, and the one that actually bites. It is safe to do after the
fact because a cell's width feeds nothing decided earlier: the rows, the
columns and the pairing all came from the height budget.

The D-pad model follows the *columns*, not the compartments: one navigable
stack per x position, carrying the items of every compartment at that x, so
Down runs off the end of the upper category straight into the start of the
lower one.

A compartment is never narrower than **two item columns**, whatever it holds.
The header carries the category's name and a badge pushed to the far end of
the same line; at one column wide there is not room for both, and since the
name does not ellipsize it is the compartment that gives — stretching to
whatever the words need, which puts every badge at a different offset again.
It buys a row of headers that all work.

The members are **dealt across** into those columns in reading order — the top
row left to right, then the row under it — so a category that does not fill its
compartment leaves the gap along the bottom rather than down the right-hand
side, where the two-column floor would otherwise leave an empty column standing
in the tin. The *columns* are what `Compartment::stacks` returns, because that
is what the D-pad moves between; only the dealing runs the other way, and the
compartment is the same width either way.

How tall a stack may be is **measured, not fixed**
(`compartment::rows_that_fit`). The design hands down three (`space.rows`),
which is what a 1280×720 screen has room for, but `scale_for` scales by the
narrower axis so the row never reflows — so a screen taller than 16:9 has
height under the compartments that a fixed three would waste, and a shorter one
clips. The field measures a probe compartment against its own height once per
layout and hands every compartment the same answer, so they all agree on where
their items start. The design's three is what that returns before the window
knows its size.

### Items

Each item shows the activity's own icon at 64px with an ink keyline traced
around its silhouette, its name, and its badge.

### Time badges

Time is shown where it applies — on the compartment when the gate belongs to the
category, on the item when it is the item's own — and never in a panel of its
own:

| Badge | Means |
|---|---|
| `+` on yellow | Time spent here earns time toward something else |
| `25m` on deep teal | Banked and ready to spend |
| `5/10` on putty | Have against need; not yet |
| `8m` on putty | Cooling down |

### Locked activities

An activity the policy has switched off is **still drawn**, at 50% opacity, with
its badge — because a child who cannot see Celeste cannot learn that ten minutes
of Tux Math would open it. It can be selected but not launched; a press shakes
its badge instead.

That applies to obstacles the child can act on: a closed window, a spent quota,
a cooldown, an unmet gate, a gamepad to plug in. Configuration and capability
problems — an entry the caregiver disabled, a kind this host cannot run, a
protection that cannot be applied — are hidden instead, because nothing the
child does changes them and a permanently dead icon teaches them to ignore
dimmed items.

**A permanent blocker wins.** Several reasons can block one activity at once,
and an entry switched off in the configuration stays off however many clocks
also happen to be against it. `is_shown_when_locked` in `src/item.rs` splits the
two kinds, and a new `ReasonCode` has to be classified in both.

### Selection

**At most one item is selected, and not until something has selected it.** The
launcher comes up with no selection at all and wakes on the first direction
press, hover or tap; that first press *reveals* the selection where it already
is rather than moving it, and a tap on empty space puts it away again. Pressing
A/Enter with nothing selected reveals rather than launches — starting something
the child cannot see is the worse failure.

The branding asks for exactly one item focused at all times, which is right for
a D-pad and wrong for a touchscreen: there is no cursor there to explain a
standing highlight, and it claims a choice nobody has made.

Left/right move between stacks and across compartments; up/down move within a
stack and wrap. Running off either end of the row nudges the scroll rather than
wrapping — by most of a screenful, since a chevron is a "next page" control.
Hovering with a pointer selects, so the pointer and the D-pad produce the same
single state.

The selected look is carried by a CSS class the field manages, not by `:focus`
— see the note on `.lb-item--selected` in `src/theme.rs` for why.

### Scaling

The stylesheet is written for 1280x720 and every `px` literal in it is
multiplied for the output the launcher actually lands on (`theme::stylesheet`).
The layout is one row at every size; scaling keeps the ratios rather than
reflowing. **A size that should scale has to be written in `px` in that
stylesheet** — anything left to the GTK theme keeps its logical value and so
shrinks on screen as everything around it grows.

### Launch Flow

```
User presses an activity
      │
      ▼
Press animates (120ms), then Launch goes out
      │
      ▼
Loading view replaces the field
      │
      ▼
┌─────┴─────┐
│           │
▼           ▼
Success     Failure
│           │
▼           ▼
Launcher    Error message,
hides       field restored
```

Nothing is desensitised while a launch is in flight: every launch path checks
the state before it acts, so the field being behind another view is already
enough to make it inert. A locked activity never launches at all — its badge
shakes instead.

## State Management

The launcher maintains a reactive state model — one enum, in `src/state.rs`,
of which view is on screen:

```rust
enum LauncherState {
    Disconnected,
    Connecting,
    Idle { entries: Vec<EntryView>, groups: Vec<GroupView> },
    Launching { entry_id: String },
    Closing { entry_label: String },
    SessionActive { .. },
    AdminMode,
    Error { message: String },
    Suspending,
}
```

`Idle` carries the categories alongside the entries because the field draws one
compartment per category: the two are one picture, and fetching them separately
would let a compartment's badge disagree with the items sitting in it.

### Event Handling

| Event | Launcher Response |
|-------|-------------------|
| `StateChanged` | Update entry grid |
| `SessionStarted` | Hide launcher |
| `SessionEnded` | Show launcher |
| `PolicyReloaded` | Refresh entry list |

### Visibility Rules

The launcher is visible when:
- No session is running, OR
- User explicitly returns to home (via HUD)

The launcher hides when:
- A session is actively running
- (Fullscreen app is in front)

## Error Handling

### Service Unavailable

If the service is not running at startup:

```
┌────────────────────────────────────────┐
│                                        │
│          System Not Ready              │
│                                        │
│    Waiting for Lunchbox service...     │
│                                        │
│           [Retry]                      │
│                                        │
└────────────────────────────────────────┘
```

### Launch Failure

If launching fails:

1. Error notification appears
2. Grid is restored to interactive state
3. User can try again or choose another entry

### Connection Loss

If connection to service is lost:

1. Entries become disabled
2. Reconnection attempted automatically
3. State refreshed on reconnection

## Accessibility

- **Touch-first** - Large touch targets (minimum 44px)
- **High contrast** - Clear visual hierarchy
- **Minimal text** - Icon-first design
- **Keyboard navigation** - Arrow keys and Enter
- **No hover-only interactions** - All actions accessible via tap

## Styling

Everything visual comes from `assets/branding/tokens.json`, the hand-off from
the design canvas. `build.rs` turns it into constants that `src/theme.rs`
includes, and the stylesheet reaches the same table through `@name@`
placeholders. **There is no second copy to keep in step**: change a colour, a
radius or a type size in the token file and it changes here, or the build fails
saying which token the stylesheet wanted and the design file does not define.

Generated at build time rather than committed, unlike the wire codegen: nothing
here leaves Rust, so there is no artifact to go stale and no drift test to need.
What is *not* generated is what the design file does not decide — the shape of
the CSS, the two sizes the brief gives only in prose, and the few places the
implementation deliberately departs from the design. Each of those says so, and
why.

The display face is **Baloo 2** (SIL Open Font License), shipped in
`assets/fonts` because no Ubuntu release packages it and the kiosk is offline by
default. `lunchbox install` puts it under `/usr/share/fonts`; for a run out of
`target/debug`, `lunchbox deps install dev` links it into this user's font
directory. The launcher names it first in a fallback stack, so a device without
it still comes up in an ordinary sans.

If the lettering is ordinary when you expect Baloo 2, suspect a stale fontconfig
cache before a missing file: run `fc-cache -f` and look again. That failure is
silent and looks exactly like the font was never installed.

Administrator mode's application picker is the *same* `LauncherField`, handed a
single synthetic category holding everything installed. It had its own widget
once; sharing one means the sunk wells, the selected cell, the scrolling and its
fades cannot drift apart between the two surfaces.

## Dependencies

- `gtk4` - GTK4 bindings
- `cairo-rs` - Drawing the coin glyph on a badge pill
- `tokio` - Async runtime
- `lunchbox-api` - Protocol types
- `lunchbox-ipc` - Client implementation
- `clap` - Argument parsing
- `tracing` - Logging

## Building

```bash
cargo build --release -p lunchbox-launcher-ui
```

The resulting binary is named `lunchbox-launcher`.

## Relationship to Service

**Critical**: The launcher is purely a presentation layer. It:
- Displays what the service allows
- Sends launch requests
- Shows service state

It does *not*:
- Tracks time independently
- Decide availability
- Enforce policy

If the launcher crashes, the service continues enforcement. If the launcher is replaced, the system still works.
