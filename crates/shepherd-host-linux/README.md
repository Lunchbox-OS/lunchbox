# shepherd-host-linux

Linux host adapter for Shepherd.

## Overview

This crate implements the `HostAdapter` trait for Linux systems, providing:

- **Process spawning** with process group isolation
- **Process termination** via graceful (SIGTERM) and forceful (SIGKILL) signals
- **Exit observation** through async process monitoring
- **Snap application support** via systemd scope-based management
- **stdout/stderr capture** to log files
- **Volume control** with auto-detection of sound systems (PipeWire, PulseAudio, ALSA)
- **Screen brightness control** via `/sys/class/backlight` for reads and
  `brightnessctl` for writes (required runtime dep — its udev rules grant
  the `video` group write access, which is the only supported path for an
  unprivileged shepherdd). Skipped on hosts without a backlight.

## Capabilities

The Linux adapter reports these capabilities:

```rust
HostCapabilities {
    // Supported entry kinds
    spawn_kind_supported: [Process, Snap],
    
    // Enforcement capabilities
    can_kill_forcefully: true,    // SIGKILL
    can_graceful_stop: true,      // SIGTERM
    can_group_process_tree: true, // Process groups (pgid)
    can_observe_exit: true,       // async wait
    
    // Optional features (not yet implemented)
    can_observe_window_ready: false,
    can_force_foreground: false,
    can_force_fullscreen: false,
    can_lock_to_single_app: false,
}
```

## Usage

### Creating the Adapter

```rust
use shepherd_host_linux::LinuxHost;

let host = LinuxHost::new();

// Check capabilities
let caps = host.capabilities();
assert!(caps.can_kill_forcefully);
```

### Spawning Processes

```rust
use shepherd_host_api::{SpawnOptions, EntryKind};

let entry_kind = EntryKind::Process {
    command: "/usr/bin/game".to_string(),
    args: vec!["--fullscreen".to_string()],
    env: Default::default(),
    cwd: None,
};

let options = SpawnOptions {
    capture_stdout: true,
    capture_stderr: true,
    log_path: Some("/var/log/shepherdd/sessions".into()),
    fullscreen: false,
    foreground: false,
};

let handle = host.spawn(session_id, &entry_kind, options).await?;
```

### Spawning Snap Applications

Snap applications are managed using systemd scopes for proper process tracking:

```rust
let entry_kind = EntryKind::Snap {
    snap_name: "mc-installer".to_string(),
    command: None,  // Defaults to snap_name
    args: vec![],
    env: Default::default(),
};

// Spawns via: snap run mc-installer
// Process group is isolated within a systemd scope
let handle = host.spawn(session_id, &entry_kind, options).await?;
```

### Spawning Steam Games

Steam games are launched via the Steam snap:

```rust
let entry_kind = EntryKind::Steam {
    app_id: 504230,
    args: vec![],
    env: Default::default(),
};

// Spawns via: snap run steam steam://rungameid/504230
let handle = host.spawn(session_id, &entry_kind, options).await?;
```

### Stopping Sessions

```rust
use shepherd_host_api::StopMode;
use std::time::Duration;

// Graceful: SIGTERM, wait 5s, then SIGKILL
host.stop(&handle, StopMode::Graceful {
    timeout: Duration::from_secs(5),
}).await?;

// Force: immediate SIGKILL
host.stop(&handle, StopMode::Force).await?;
```

### Monitoring Exits

```rust
let mut events = host.subscribe();

tokio::spawn(async move {
    while let Some(event) = events.recv().await {
        match event {
            HostEvent::Exited { handle, status } => {
                println!("Session {} exited: {:?}", handle.session_id(), status);
            }
            _ => {}
        }
    }
});
```

## Volume Control

The crate includes `LinuxVolumeController` which auto-detects the available sound system:

```rust
use shepherd_host_linux::LinuxVolumeController;

let controller = LinuxVolumeController::new().await?;

// Get current volume (0-100)
let volume = controller.get_volume().await?;

// Set volume with enforcement of configured maximum
controller.set_volume(75).await?;

// Mute/unmute
controller.set_muted(true).await?;
```

### Sound System Detection Order

1. **PipeWire** (`wpctl` or `pw-cli`) - Modern default on Ubuntu 22.04+, Fedora
2. **PulseAudio** (`pactl`) - Legacy but widely available
3. **ALSA** (`amixer`) - Fallback for systems without a sound server

## Process Group Handling

All spawned processes are placed in their own process group:

```rust
// Internally uses setsid() or setpgid()
// This allows killing the entire process tree
```

When stopping a session:
1. SIGTERM is sent to the process group (`-pgid`)
2. After timeout, SIGKILL is sent to the process group
3. Orphaned children are cleaned up

## Log Capture

stdout and stderr can be captured to session log files:

```
/var/log/shepherdd/sessions/
├── 2025-01-15-abc123-minecraft.log
├── 2025-01-15-def456-gcompris.log
└── ...
```

## Network firewall

When `SpawnOptions::firewall` is set, the adapter applies a per-session
network filter via systemd's BPF address controls
(`IPAddressAllow=`/`IPAddressDeny=`).

- **Process kind**: the spawn argv is wrapped in
  `systemd-run --user --scope --collect --quiet --property=...` so the
  firewall is in place from the first instruction.
- **Flatpak / Snap**: the runtime creates its own scope
  (`app-flatpak-<id>-*.scope`, `snap.<name>.<name>-*.scope`). The adapter
  spawns the app, polls for the scope, and then applies the firewall via
  `systemctl --user --runtime set-property`. Small race window during
  early app startup.
- **Steam**: not yet supported (logged as a warning).

## Browser policy

When `SpawnOptions::browser` is set and the entry is a Chromium-capable kind
(`flatpak` — the supported `com.google.Chrome` path — or `process`), the
adapter materializes the policy before spawning (`browser.rs`):

- **Managed-policy JSON**: a `<policy_id>.json` file is written (regenerated
  each spawn) under
  `~/.var/app/com.google.Chrome/config/chromium/policies/managed/`, carrying
  `URLAllowlist`/`URLBlocklist` and the lockdown switches
  (`DeveloperToolsAvailability`, `IncognitoModeAvailability`,
  `ExtensionInstallBlocklist`). A non-empty allowlist injects a catch-all
  `"*"` blocklist so the allowlist is authoritative.
- **Launch flags**: the window mode + start URL become Chrome flags
  (`--kiosk <url>`, `--app=<url>`, or a bare `<url>`), appended to the argv.
  For `process` kind these flags ride inside the firewall helper's wrapped
  argv; for flatpak they follow the app id as app arguments.

A failed policy write is logged and the launch continues. Profile management
(`--user-data-dir`, `wipe_on_exit`) is a separate step and not yet wired here.

## Future Enhancements

Planned features (hooks are designed in):

- **cgroups v2** - CPU/memory/IO limits per session
- **Namespace isolation** - Optional sandboxing
- **Sway/Wayland integration** - Focus and fullscreen control
- **D-Bus monitoring** - Window readiness detection
- **Steam firewall support** - apply IPAddressAllow/Deny to Steam game scopes

## Dependencies

- `nix` - Unix system calls
- `tokio` - Async runtime
- `tracing` - Logging
- `serde` - Serialization
- `shepherd-host-api` - Trait definitions
- `shepherd-api` - Entry types
