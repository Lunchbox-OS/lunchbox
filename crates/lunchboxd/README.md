# lunchboxd

The Lunchbox background service.

## Overview

`lunchboxd` is the authoritative policy and enforcement service for the Lunchbox ecosystem. It is the central coordinator that:

- Loads and validates configuration
- Evaluates policy to determine availability
- Manages session lifecycles
- Enforces time limits
- Emits warnings and events
- Serves multiple clients via IPC

**Key principle**: `lunchboxd` is the single source of truth. User interfaces only request actions and display state—they never enforce policy independently.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                         lunchboxd                            │
│                                                              │
│  ┌─────────────┐   ┌─────────────┐   ┌────────────────────┐  │
│  │   Config    │   │    Store    │   │    Core Engine     │  │
│  │   Loader    │──▶│  (SQLite)   │──▶│ (Policy + Session) │  │
│  └─────────────┘   └─────────────┘   └──────────┬─────────┘  │
│                                                 │            │
│  ┌─────────────┐  ┌─────────────┐               │            │
│  │    Host     │  │     IPC     │◀──────────────┘            │
│  │   Adapter   │◀─│   Server    │                            │
│  │  (Linux)    │  │             │                            │
│  └──────┬──────┘  └──────┬──────┘                            │
│         │                │                                   │
│         │      Unix Domain Socket                            │
│         │                │                                   │
└─────────┼────────────────┼───────────────────────────────────┘
          │                │
          ▼                ▼
    Supervised        ┌─────────┐  ┌─────────┐  ┌─────────┐
    Applications      │Launcher │  │   HUD   │  │  Admin  │
                      │   UI    │  │ Overlay │  │  Tools  │
                      └─────────┘  └─────────┘  └─────────┘
```

### Auxiliary managers

- `hidpi` (`XwaylandHidpi`) — temporarily drops the compositor output scale to
  1.0 for XWayland activities that need the native pixel grid (issue #45).
- `display` (`DisplayManager`) + `display_watch` — external monitor / docking
  support (issue #87). Captures the primary output at boot, mirrors it onto a
  docked display via `wl-mirror` by default, and exposes a HUD toggle to switch
  to external-only at native resolution. A sway IPC output-event subscription
  drives reconciliation on hotplug. Exactly one logical output is active in
  every mode, preserving the one-activity-at-a-time invariant.
- `input_devices` (`InputMonitor`) — input-device dependencies (issue #96).
  When an entry declares `requires_input`, enumerates `/dev/input` via `evdev`
  to see which device types (mouse/touch/keyboard/gamepad) are connected, feeds
  the set into the engine, and re-broadcasts availability on hotplug (a `notify`
  watch on `/dev/input` plus a slow fallback re-scan). Gates fail open when
  `/dev/input` isn't readable.

## Usage

### Running

```bash
# With default config location
lunchboxd

# With custom config
lunchboxd --config /path/to/config.toml

# Override socket and data paths
lunchboxd --socket /tmp/lunchboxd.sock --data-dir /tmp/lunchboxd-data

# Debug logging
lunchboxd --log-level debug
```

### Command-Line Options

| Option | Default | Description |
|--------|---------|-------------|
| `-c, --config` | `~/.config/lunchbox/config.toml` | Configuration file path |
| `-s, --socket` | From config | IPC socket path |
| `-d, --data-dir` | From config | Data directory |
| `-l, --log-level` | `info` | Log verbosity |

### Environment Variables

| Variable | Description |
|----------|-------------|
| `LUNCHBOX_SOCKET` | Override socket path (default: `$XDG_RUNTIME_DIR/lunchboxd/lunchboxd.sock`) |
| `LUNCHBOX_DATA_DIR` | Override data directory (default: `$XDG_DATA_HOME/lunchboxd`) |
| `RUST_LOG` | Tracing filter (e.g., `lunchboxd=debug`) |

## Main Loop

The service runs an async event loop that processes:

1. **IPC messages** - Commands from clients
2. **Host events** - Process exits, window events
3. **Timer ticks** - Check for warnings and expiry
4. **System events** - logind suspend/resume, NetworkManager state changes
5. **Signals** - SIGHUP for config reload, SIGTERM for shutdown

```
┌────────────────────────────────────────────────────┐
│                    Main Loop                       │
│                                                    │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐   │
│  │   IPC   │ │  Host   │ │  Timer  │ │ Signal  │   │
│  │ Channel │ │ Events  │ │  Tick   │ │ Handler │   │
│  └────┬────┘ └────┬────┘ └────┬────┘ └────┬────┘   │
│       │           │           │           │        │
│       └───────────┴─────┬─────┴───────────┘        │
│                         │                          │
│                         ▼                          │
│              ┌──────────────────┐                  │
│              │  Process Event   │                  │
│              └──────────────────┘                  │
│                         │                          │
│                         ▼                          │
│              ┌──────────────────┐                  │
│              │ Broadcast Events │                  │
│              └──────────────────┘                  │
└────────────────────────────────────────────────────┘
```

## Command Handling

### Client Commands

| Command | Description | Role Required |
|---------|-------------|---------------|
| `GetState` | Get full state snapshot | Any |
| `ListEntries` | Get available entries | Any |
| `Launch` | Start a session | Shell/Admin |
| `StopCurrent` | End current session | Shell/Admin |
| `ReloadConfig` | Hot-reload configuration | Admin |
| `SubscribeEvents` | Subscribe to event stream | Any |
| `GetHealth` | Health check | Any |
| `SetVolume` | Set system volume | Shell/Admin |
| `VolumeUp` | Increase volume by a step (clamped to policy) | Shell/Admin |
| `VolumeDown` | Decrease volume by a step (clamped to policy) | Shell/Admin |
| `ToggleMute` | Toggle mute state | Shell/Admin |
| `GetVolume` | Get volume info | Any |

### Response Flow

```
Client Request
      │
      ▼
Role Check ──────▶ Denied Response
      │
      ▼
Command Handler
      │
      ▼
Core Engine
      │
      ▼
Response + Events ──────▶ Broadcast to Subscribers
```

## Session Lifecycle

### Launch

1. Client sends `Launch { entry_id }`
2. Core engine evaluates policy
3. If denied: respond with reasons
4. If approved: create session plan
5. Host adapter spawns process
6. Session transitions to Running
7. `SessionStarted` event broadcast

### Enforcement

1. Timer ticks every 100ms
2. Core engine checks warnings and expiry
3. At warning thresholds: `WarningIssued` event
4. At deadline: initiate graceful stop
5. After grace period: force kill
6. `SessionEnded` event broadcast

### Across a suspend (issue #155)

The countdown is monotonic, so sleeping never spends a child's time. But the
wall clock moves on, and two things follow that the tick loop cannot see:

- The displayed deadline (`SessionInfo::deadline`, which every countdown is
  derived from) is now stale by the length of the sleep, and can read 0:00 while
  the session runs on.
- The session may have outlived the availability window that bounded it at
  launch — unbounded, for an overnight sleep.

`system_events.rs` already watches logind's `PrepareForSleep` for the suspend
cover, so the resume arm of the main loop calls `CoreEngine::notify_resumed`
before broadcasting the fresh snapshot. That re-derives the wall deadline and,
if the machine woke outside the activity's allowed hours, clamps the session to
its `save_grace` (default 2 minutes) with a `Critical` warning. The snapshot is
broadcast *before* the warning: clients rebuild their countdown from it, so a
warning sent first would be overwritten by the state that followed.

### Termination

1. Stop triggered (expiry, user, admin, process exit)
2. Host adapter signals process (SIGTERM)
3. Wait for grace period
4. Force kill if needed (SIGKILL)
5. Record usage in store
6. Set cooldown if configured, unless the session was shorter than
   `cooldown_min_session` (default 2 minutes, for unstable activities)
7. Clear session state

## Configuration Reload

On SIGHUP or `ReloadConfig` command:

1. Parse new configuration file
2. Validate completely
3. If invalid: keep old config, log error
4. If valid: atomic swap to new policy
5. Emit `PolicyReloaded` event
6. Current session continues with original plan

## Health Monitoring

The service exposes health status via `GetHealth`:

```json
{
  "status": "healthy",
  "policy_loaded": true,
  "store_healthy": true,
  "host_healthy": true,
  "uptime_seconds": 3600,
  "current_session": null
}
```

## Logging

Uses structured logging via `tracing`:

```
2025-01-15T14:30:00.000Z INFO  lunchboxd: Starting Lunchbox service
2025-01-15T14:30:00.050Z INFO  lunchbox_config: Configuration loaded entries=5
2025-01-15T14:30:00.100Z INFO  lunchbox_ipc: IPC server listening path=/run/lunchboxd/lunchboxd.sock
2025-01-15T14:30:15.000Z INFO  lunchbox_core: Session started session_id=abc123 entry_id=minecraft
2025-01-15T14:59:45.000Z WARN  lunchbox_core: Warning issued session_id=abc123 threshold=60
2025-01-15T15:00:45.000Z INFO  lunchbox_core: Session expired session_id=abc123
```

## Persistence

State is persisted to SQLite:

```
/var/lib/lunchboxd/
├── lunchboxd.db       # SQLite database
└── logs/
    └── sessions/      # Session stdout/stderr
```

## Signals

| Signal | Action |
|--------|--------|
| `SIGHUP` | Reload configuration |
| `SIGTERM` | Graceful shutdown |
| `SIGINT` | Graceful shutdown |

## Dependencies

This binary wires together all the library crates:

- `lunchbox-config` - Configuration loading
- `lunchbox-core` - Policy engine
- `lunchbox-host-api` - Host adapter trait
- `lunchbox-host-linux` - Linux implementation
- `lunchbox-ipc` - IPC server
- `lunchbox-store` - Persistence
- `lunchbox-api` - Protocol types
- `lunchbox-util` - Utilities
- `tokio` - Async runtime
- `clap` - CLI parsing
- `tracing` - Logging
- `anyhow` - Error handling

## Building

```bash
cargo build --release -p lunchboxd
```

## Installation

The service is typically started by the compositor:

`sway.conf`
```conf
# Start lunchboxd FIRST - it needs to create the socket before HUD/launcher connect
# Running inside sway ensures all spawned processes use the nested compositor
exec ./target/debug/lunchboxd -c ./config.example.toml
```

See [CONTRIBUTING.md](../../CONTRIBUTING.md) for development setup.
