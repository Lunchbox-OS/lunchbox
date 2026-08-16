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
- **Ambient light sensor reads** via the IIO sysfs `in_illuminance_raw`
  channel (`LinuxLightSensor`), used by the automatic-brightness feature.
  Read-only and world-readable, so no helper or privilege is needed. Absent
  on hosts without an ALS.
- **Compositor output primitives** (`sway.rs`) — query/enable/disable outputs,
  set modes and scales, and pick a mirror mode; behind the `OutputBackend`
  trait so the docking state machine in `shepherdd` is unit-testable. Used for
  external monitor / docking support (issue #87).
- **Audio topology** (`audio.rs`) — parse `pw-dump` into the list of selectable
  audio outputs, identify which one is active, and read its volume. Shared by
  the two consumers below.
- **Audio routing** (`audio_route.rs`) — switch the PipeWire default sink to an
  HDMI/DisplayPort output while docked, via `pw-dump` + `wpctl set-default`.

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

### Audio Outputs

On PipeWire, `current_output()` names the output a reading applies to and
`observe()` returns the volume and that identity from a single `pw-dump`, so the
pair can never describe two different moments. Other backends fall back to the
trait defaults and behave as one anonymous output.

An output is keyed `<device.name>:output:<route.name>` — deliberately the same
key WirePlumber uses in its `default-routes` state file, so our notion of "an
output" cannot drift from the volume PipeWire remembers for it. `device.name`
rather than `node.name` because the node name embeds the card profile; the route
is what separates headphones from speakers, which share one sink node on an
analog jack. Numeric object ids are never identity — PipeWire recycles them.

Volume in `pw-dump` is stored cubed: `channelVolumes` of `0.015625` is 25%,
matching `wpctl get-volume`'s `cbrt`.

The `kind` classification (headphones, speakers, HDMI, ...) is advisory and
drives presentation only. It is often `Unknown`: a generic USB interface reports
the uninformative `analog-output` route and udev sets no `device.form-factor`.
Nothing in policy may depend on it.

## Process Group Handling

All spawned processes are placed in their own process group:

```rust
// Internally uses setsid() or setpgid()
// This allows killing the entire process tree
```

When stopping a session:
1. **Exactly one** SIGTERM is sent to the process group (`-pgid`)
2. After timeout, SIGKILL is sent to the process group, plus a `pkill` by
   command name to sweep up anything that escaped it
3. Orphaned children are cleaned up

The "exactly one" matters. The graceful path used to send three SIGTERMs — a
`pkill -f` by command name, the process-group kill, and one per descendant —
and an app whose handler *counts* signals reads the second as "the user is
impatient". RetroArch's calls `exit(1)` on it, which skips flushing the
in-game save and writing the save state, so no emulator session could close
without losing progress. Sandboxed kinds (snap, flatpak, Steam) still get
their cgroup- or app-id-based delivery, because the real process is not in our
child's process group; `GracefulSignal` in `adapter.rs` is the single place
that decision is made.

## Window attribution

`list_windows` answers "what is on screen"; the compositor can only say which
pid drew each surface. `LinuxHost::list_windows` fills in `WindowInfo::owner`
by matching every window against what the host is actually supervising —
tracked processes and their groups, Steam game pids found by app id, input
sidecars, shepherd's own furniture, and the `escaped` registry:

| `owner` | Meaning |
| --- | --- |
| `shepherd` | Our own UI or a background process we keep warm. |
| `activity` | The session running right now. |
| `escaped` | An activity that outlived teardown; the sweep is still killing it. |
| `unowned` | Nothing we know about. |

Attribution is computed per `list_windows` call rather than on the monitor's
reconciliation sweep: resolving Steam game pids walks `/proc` reading every
process's environment, which is fine on an admin screen someone has open and
wrong on a loop that runs every two seconds regardless. The sweep keeps its own
cheaper check (`report_unowned_windows`), which only knows about pids it
spawned. Closing an `unowned` window stays a human's call:
shepherd will not kill a surface it does not recognize, because a system
dialog on a kiosk a child depends on is worse than the visibility gap.
## RetroArch

`EntryKind::Retroarch` entries are launched through `retroarch.rs`, which
renders a config fragment (`--appendconfig`) around the launch: a per-entry
save-state directory, save-state-on-close/restore-on-open, a periodic in-game
save flush, kiosk mode, and `config_save_on_exit = "false"` so none of it leaks
back into the user's own `retroarch.cfg`. The in-game save (`.srm`) is
deliberately left at RetroArch's own default location, so a game has one save
however it was launched and a save predating the entry is still found. Those sessions also get a
longer graceful-stop floor (`retroarch::STOP_TIMEOUT`), since their shutdown
has to unload the core and write both kinds of save.

The fragment is *appended* to the user's own `retroarch.cfg`, so controller
bindings, video settings and per-core options configured outside shepherd carry
into supervised sessions. RetroArch applies per-core **overrides** after
`--appendconfig`, though, so an override naming one of the settings above wins
over shepherd — `retroarch::conflicting_overrides` detects that at launch and
warns rather than silently losing save-state resume or the menu lock. See
`docs/emulators.md`.

`discard_saved_state` (the `HostAdapter` hook behind the HUD's reset button)
deletes the `*.state.auto` files so the next launch boots from the content's
own start screen. It deliberately leaves the in-game save (`.srm`) alone:
resetting a console returns it to the title screen, it does not wipe the
cartridge. Call it only between the stop and the respawn — against a live
activity it would race RetroArch's own writes.

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

When `SpawnOptions::browser` is set and the entry is the supported
`com.google.Chrome` flatpak, the adapter materializes the policy before
spawning (`browser.rs`). Other entry kinds (and other flatpaks) are not
supported — a warning is logged and the browser policy ignored.

- **Per-user policy injection (not system-wide).** Google Chrome only reads
  managed policy from the root-owned, machine-wide `/etc/opt/chrome/policies/`
  — writing there would hijack Chrome for *every* user on the box. But the
  flatpak's launch wrapper populates that path *inside its own sandbox* (an
  ephemeral, per-launch filesystem). So shepherd writes a `<policy_id>.json`
  (regenerated each spawn) into the app's own per-user tree
  (`~/.var/app/com.google.Chrome/config/shepherd-policies/`) and rebuilds the
  launch as:

  ```
  flatpak run --command=bash --env=SHEPHERD_POLICY=<file> com.google.Chrome \
    -c 'mkdir -p /etc/opt/chrome/policies/managed;
        ln -sf "$SHEPHERD_POLICY" /etc/opt/chrome/policies/managed/shepherd.json;
        exec /app/bin/chrome "$@"' bash <chrome flags>
  ```

  The shim symlinks our policy into the sandbox's own `/etc` and then execs the
  flatpak's normal launcher. The policy applies only to that launch, only for
  this user; the host `/etc` is never touched. (Verified end-to-end against
  real Chrome by the gated test.)

- **Policy contents**: `URLAllowlist`/`URLBlocklist` plus the lockdown switches
  (`DeveloperToolsAvailability`, `IncognitoModeAvailability`,
  `ExtensionInstallBlocklist`). A non-empty allowlist injects a catch-all `"*"`
  blocklist so the allowlist is authoritative.
- **Launch flags**: the window mode + start URL become Chrome flags
  (`--kiosk <url>`, `--app=<url>`, or a bare `<url>`), plus `--user-data-dir`.
- **Profile**: `profile_id` selects a per-profile user-data-dir under
  `~/.var/app/com.google.Chrome/config/google-chrome/<profile_id>/`. Entries
  sharing a `profile_id` share cookies/logins; each unique id is isolated.
- **`wipe_on_exit`**: when set, the user-data-dir is recorded against the
  activity's pid and deleted once the activity exits (detected by the process
  monitor, so the wipe waits for the Chrome instance to be gone). Wiping
  happens in the host adapter, never inside Chrome.

A failed policy write is logged and Chrome launches without the policy.

## Running this crate's tests

The tests here drive real processes, and some of the code under test kills by
command name (`kill_by_command` runs `pkill -f <name>`). `pkill -f` matches
against whole command lines, including your shell's.

`test_spawn_and_kill` spawns `sleep 60` and stops it by the command name
"sleep", so `pkill -f sleep` runs during the suite. **If the shell you launch
`cargo test` from has "sleep" anywhere in its command line, that shell is
killed too** (it exits 144, mid-command, with no obvious cause). The same
applies to any wrapper script or CI step whose invocation contains the word.

Keep the invocation free of the names the suite pkills, or run the tests from
a command line you do not mind losing. The equivalent trap for headless
fixtures — never give a fixture entry `command = "sleep"` — is documented in
the `headless-dev` skill.

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
