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
stacks of three that spill *rightwards*, and — when the category is on a
schedule — its closing time today on the floor.

Height never grows: a category with more members gets wider, and if the row
overflows the screen the row scrolls **horizontally**. The field never scrolls
vertically.

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
problems (a disabled entry, a kind this host cannot run, a protection that
cannot be applied) are hidden instead, because nothing the child does changes
them, and a permanently dead icon teaches them to ignore dimmed items.
`is_shown_when_locked` in `src/item.rs` is the list, and a new `ReasonCode` has
to be classified there.

### Selection

Exactly one item is selected whenever the field is showing. Left/right move
between stacks and across compartments; up/down move within a stack and wrap.
Running off either end of the row nudges the scroll rather than wrapping.
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
User taps tile
      │
      ▼
Launcher sends Launch command
      │
      ▼
Grid input disabled
"Starting..." overlay shown
      │
      ▼
┌─────┴─────┐
│           │
▼           ▼
Success     Failure
│           │
▼           ▼
Launcher    Error message
hides       Grid restored
```

## State Management

The launcher maintains a reactive state model:

```rust
struct LauncherState {
    entries: Vec<EntryView>,   // From service
    current_session: Option<SessionInfo>,
    connected: bool,
    launching: Option<EntryId>,
}
```

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

Everything visual comes from `src/theme.rs`, which is the Rust half of
`assets/branding/tokens.json`. Keep the two in step.

The display face is **Baloo 2** (SIL Open Font License), shipped in
`assets/fonts` because no Ubuntu release packages it and the kiosk is offline by
default. `lunchbox install` puts it under `/usr/share/fonts`; for a run out of
`target/debug`, `lunchbox deps install dev` links it into this user's font
directory. The launcher names it first in a fallback stack, so a device without
it still comes up in an ordinary sans.

If the lettering is ordinary when you expect Baloo 2, suspect a stale fontconfig
cache before a missing file: run `fc-cache -f` and look again. That failure is
silent and looks exactly like the font was never installed.

Administrator mode's application picker (`src/grid.rs`) deliberately keeps a
wrapping flow box rather than the field: it is a searchable list of every
`.desktop` file on the system, for a caregiver rather than a child.

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
