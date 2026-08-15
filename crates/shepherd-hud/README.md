# shepherd-hud

Always-visible HUD overlay for Shepherd.

## Overview

`shepherd-hud` is a GTK4 layer-shell overlay that remains visible during active sessions. It provides essential information and controls that must always be accessible, regardless of what fullscreen application is running underneath.

## Features

- **Time remaining** - Authoritative countdown from the service
- **Battery level** - Current charge percentage and status
- **Volume control** - Adjust system volume with enforced limits
- **Brightness control** - Adjust screen brightness on hosts with a backlight (hidden otherwise)
- **Session controls** - End session button (confirms first unless the activity opts out via `confirm_on_close = false`)
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
shepherd-hud

# With custom socket path
shepherd-hud --socket /run/shepherdd/shepherdd.sock

# Custom position and size
shepherd-hud --anchor top --height 48
```

### Command-Line Options

| Option | Default | Description |
|--------|---------|-------------|
| `-s, --socket` | `$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock` | Service socket path |
| `-l, --log-level` | `info` | Log verbosity |
| `-a, --anchor` | `top` | Screen edge (`top` or `bottom`) |
| `--height` | `48` | HUD bar height in pixels |

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
- Click to adjust (sends commands to service)
- Volume maximum may be restricted by policy

### Controls

- **End Session** - Stops the current session (if allowed)
- **Power** - Opens menu with Suspend/Shutdown/Restart

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

## HUD scale factor (the XWayland DPI hack)

While an `xwayland_native_resolution` activity runs, shepherdd drops every sway
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
- `shepherd-api` - Protocol types
- `shepherd-ipc` - Client implementation
- `upower` - Battery monitoring
- `clap` - Argument parsing
- `tracing` - Logging

## Building

```bash
cargo build --release -p shepherd-hud
```

Requires GTK4 development libraries and a Wayland compositor with layer-shell support (e.g., Sway, Hyprland).
