# lunchbox-host-linux

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
  unprivileged lunchboxd). Skipped on hosts without a backlight.
- **Ambient light sensor reads** via the IIO sysfs `in_illuminance_raw`
  channel (`LinuxLightSensor`), used by the automatic-brightness feature.
  Read-only and world-readable, so no helper or privilege is needed. Absent
  on hosts without an ALS.
- **Network status reads** (`network.rs`) — `NetworkInfoProvider` for the
  management UIs (issue #182): connectivity, interfaces, addresses, gateway,
  DNS, and the SSID / signal / band of an associated wireless network. Reads
  NetworkManager over the system D-Bus (every property it needs is readable
  unprivileged — no polkit rule, nothing to configure), bounded by a 3s
  timeout, and falls back to `getifaddrs` for interfaces and addresses on a
  host with no NetworkManager. Read-only: nothing here joins a network or
  changes one.
- **Compositor IPC** (`sway_ipc.rs`) — a client for sway's own socket
  (`sway-ipc(7)`): one connection behind a mutex for requests, a second per
  event subscription. Replaces the `swaymsg` subprocess every compositor call
  used to spawn, which stopped being defensible once the escape sweep started
  reading the window tree twice a second (issue #147). Losing an established
  connection is terminal — lunchboxd is `exec`'d by sway and dies with it.
- **Compositor output primitives** (`sway.rs`) — query/enable/disable outputs,
  set modes and scales, and pick a mirror mode; behind the `OutputBackend`
  trait so the docking state machine in `lunchboxd` is unit-testable. Used for
  external monitor / docking support (issue #87). The parsing is separate from
  the transport and is tested against literals, with no compositor.
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
use lunchbox_host_linux::LinuxHost;

let host = LinuxHost::new();

// Check capabilities
let caps = host.capabilities();
assert!(caps.can_kill_forcefully);
```

### Spawning Processes

```rust
use lunchbox_host_api::{SpawnOptions, EntryKind};

let entry_kind = EntryKind::Process {
    command: "/usr/bin/game".to_string(),
    args: vec!["--fullscreen".to_string()],
    env: Default::default(),
    cwd: None,
};

let options = SpawnOptions {
    capture_stdout: true,
    capture_stderr: true,
    log_path: Some("/var/log/lunchboxd/sessions".into()),
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
use lunchbox_host_api::StopMode;
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
use lunchbox_host_linux::LinuxVolumeController;

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
2. After timeout, SIGKILL is sent to the process group, plus a by-name sweep
   for anything that escaped it — scoped to spare other live sessions
3. Orphaned children are cleaned up

The "exactly one" matters, and it is easy to lose. The graceful path once sent
three SIGTERMs — a `pkill -f` by command name, the process-group kill, and one
per descendant — and an app whose handler *counts* signals reads the second as
"the user is impatient". RetroArch's calls `exit(1)` on it, which skips
flushing the in-game save and writing the save state. It regressed once
afterwards, to two signals, when a fix elsewhere added a second group kill
beside `ManagedProcess::terminate` (the same syscall); that cost about a fifth
of save states until it was measured. Sandboxed kinds (snap, flatpak, Steam)
still get their cgroup- or app-id-based delivery, because the real process is
not in our child's process group; `GracefulSignal` in `adapter.rs` is the
single place that decision is made.

The by-name sweep in step 2 is scoped for a related reason. `pkill -f
retroarch` cannot tell which RetroArch it is looking at, so it would also kill
a game the child started afterwards in a different session — and a `SIGKILL`
runs no shutdown path, so that loses the save outright. `kill_by_command`
therefore matches with `pgrep`, resolves each candidate's process group, and
skips every group belonging to a session the host is still tracking
(`LinuxHost::tracked_pgids`).

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
## Ebook

`EntryKind::Ebook` entries are launched through `ebook.rs`, which generates the
reader's entire configuration into a per-entry `XDG_CONFIG_HOME` /
`XDG_DATA_HOME` / `XDG_CACHE_HOME` and re-renders it before every launch —
Okular rewrites its own config on exit, so a one-time seed would decay. The
admin's own KDE configuration is never touched.

Two mechanisms, because Okular has no single kiosk switch: KDE's Kiosk *action
restrictions* in `kdeglobals` (immutable via `[$i]`, enforced by
`KActionCollection` so the menu item, toolbar button and shortcut all die
together), and the view settings in `okularrc`/`okularpartrc` (no menubar,
sidebar or scrollbars; a page at a time, fitted to the screen).

The toolbar takes a third mechanism, and not the documented one: the XMLGUI
`hidden` attribute that is supposed to control it does nothing here (measured on
a device, not assumed), so the generated `okularrc` asks Okular to start in its
own full-screen mode — which hides menubar and toolbar together — and sets
`shouldShow{MenuBar,ToolBar}ComingFromFullScreen=false` so that leaving the mode
restores neither. The compositor refuses the fullscreen state to keep the HUD
visible, which is exactly what makes Okular leave the mode, and the window keeps
its ordinary geometry throughout. The `fullscreen` action must therefore stay
unrestricted, since the chrome hiding hangs off it. Reading positions live in
`<data>/okular/docdata/` and are never written by shepherd. See
`docs/ebooks.md`.

## Closing a window before signalling it

`SIGTERM` is a request to die; `xdg_toplevel.close` is a request to finish. A
large class of desktop applications saves its state in the window's close
handler and installs no signal handler at all — Okular is the measured case: it
writes the page it was on only on a clean close, so a signal-only stop loses the
child's place every session.

So a graceful stop for a kind that asks for it (`EntryKind::wants_polite_close`)
first asks the compositor to close every window attributed to the session
(`[con_id=N] kill` over the sway IPC the daemon already holds), waits up to
`POLITE_CLOSE_TIMEOUT`, and only then falls through to the unchanged
`SIGTERM` → `SIGKILL` ladder. Measured at 0.33 s (PDF) to ~2 s (EPUB).

It is opt-in per kind rather than universal because a close request is not
always a quit: Steam reads it as "hide to tray", and RetroArch already has a
verified single-`SIGTERM` shutdown that writes its save state. An activity with
no window costs nothing — the wait only happens if a window was found.

## RetroArch

`EntryKind::Retroarch` entries are launched through `retroarch.rs`, which
renders a config fragment (`--appendconfig`) around the launch: a per-entry
save-state directory, save-state-on-close/restore-on-open, a periodic in-game
save flush, kiosk mode, the native Wayland context (so the picture is the
panel's own pixel grid rather than an upscaled XWayland one), and
`config_save_on_exit = "false"` so none of it leaks back into the user's own
`retroarch.cfg`. The in-game save (`.srm`) is deliberately left wherever the
user's own `savefile_directory` puts it — which is *not* beside the content on
a default install — so a game has one save however it was launched and a save
predating the entry is still found. Those sessions also get a longer
graceful-stop floor (`retroarch::STOP_TIMEOUT`), since their shutdown has to
unload the core and write both kinds of save.

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
/var/log/lunchboxd/sessions/
├── 2025-01-15-abc123-minecraft.log
├── 2025-01-15-def456-gcompris.log
└── ...
```

## Network firewall

When `SpawnOptions::firewall` is set, the adapter applies a per-session
network filter via systemd's BPF address controls
(`IPAddressAllow=`/`IPAddressDeny=`).

- **Process kind**: the spawn argv is wrapped in `pkexec
  lunchbox-firewall-helper apply-process`, which execs `systemd-run --scope
  --property=...` against the **system** manager, so the firewall is in place
  from the first instruction. The system manager is required: attaching the
  `cgroup_skb` programs behind `IPAddress*=` needs privileges the per-user
  manager does not have.
- **Flatpak / Snap**: the runtime creates its own scope
  (`app-flatpak-<id>-*.scope`, `snap.<name>.<name>-*.scope`). The adapter
  spawns the app, polls for that scope, and then has
  `lunchbox-firewall-helper apply-cgroup` attach a `cgroup_skb/egress` BPF
  program to it. (`systemctl --user --runtime set-property` was the older
  approach and silently did nothing: the per-user systemd manager has neither
  `CAP_NET_ADMIN` nor `CAP_BPF`, so it accepted the property without
  attaching a program.) Small race window during early app startup, in which
  the app *is* running unfiltered — nothing can attach a filter to a cgroup
  the runtime has not created yet.
- **Fail closed** (`spawn_firewall_guard`): if the scope never appears, or the
  attach fails, the adapter kills the activity and ends the session as
  `HostEvent::LaunchFailed` rather than letting it run unfiltered. Issue #151
  is why: a misaligned BPF object made every attach fail, and the only trace
  was one `warn!` per launch while firewalled activities browsed freely. The
  cost of the trade is that a host where the runtime never creates a scope
  (no systemd user manager, say) loses these activities ~5s in, loudly,
  instead of running them unprotected, quietly.
- **Steam**: not yet supported (logged as a warning).

## Every activity gets a cgroup of its own (issue #144)

Independent of the firewall, and for a different reason: `lunchboxd` accepts a
client on its management socket only from its own cgroup, and an activity
launched by a plain `fork`/`exec` inherits that cgroup *exactly* — not merely
hard to tell from the launcher, but the same string. So the adapter makes sure
every activity is somewhere else before it starts:

- **Firewalled Process kind**: already handled — the helper's system-manager
  scope above.
- **Snap / Flatpak**: already handled — the runtime scopes them under
  `user@<uid>.service/app.slice`, which is the same fact
  `apply_firewall_to_existing_scope` relies on to find them. Wrapping them again
  would nest a scope around a launcher that immediately hands off elsewhere.
- **Everything else, Steam included**: wrapped in
  `systemd-run --user --scope --collect` (`user_scope_argv_prefix`).
  Unprivileged — no helper, no polkit — because all this has to achieve is "not
  shepherd's cgroup", which the *user* manager can do even though it cannot
  attach BPF.

Like the firewall helper's `systemd-run --scope`, this execs the command in its
own process rather than forking one, so the pid the adapter records is the
activity's, already inside the scope, and every pid, pgid and kill path is
unchanged.

### Steam is wrapped at both ends, and why it looks redundant

Steam reaches the same place by a different route, so the wrapper around it is
easy to mistake for dead code. It is deliberate, and the reasoning is worth
keeping:

- A Steam entry launches `snap run steam steam://rungameid/<id>`, which is a
  short-lived request to the **preloaded** client. `snap run` re-scopes itself
  into `snap.steam.steam-<uuid>.scope` almost immediately, so the
  `shepherd-<session>.scope` around it empties and `--collect` reaps it. Measured:
  the scope is `inactive` within a second and no units accumulate.
- The game is a child of the preloaded client, not of that request — so
  `preload_steam` is the launch a game actually inherits its cgroup from, and it
  is wrapped too (`shepherd-steam-preload-<pid>.scope`).

Both scopes empty out the moment `snap run` hands off. What the wrapping buys is
that "nothing shepherd starts for an activity is ever in shepherd's cgroup"
holds because of what this crate does, rather than because snapd happens to move
the process quickly enough. Removing either wrapper would restore a window —
short, and not obviously reachable, but one whose width is set by a third party.

Neither wrapper changes where Steam ends up: the client and its games live in
snapd's `snap.steam.steam-*` scope under `user@<uid>.service/app.slice`, which is
already outside shepherd's cgroup. `snap run` does not preserve the pid either,
with or without the wrapper, which is why Steam sessions are tracked by
`find_steam_game_pids` rather than by the pid the adapter recorded.

`activity_isolation_status()` probes whether the user manager can be reached at
all (a temp `XDG_RUNTIME_DIR` with no bus, as in the e2e harness, cannot), and
caches the answer. When it cannot, the activity is launched anyway rather than
lost — the trade the compositor hardening makes — and `lunchboxd` raises the
`ipc_socket_not_hardened` diagnostic, because the socket check has nothing left
to tell apart.

## Shepherd's own helpers get one too (issue #144)

Being in lunchboxd's cgroup is what the management socket trusts, so it is worth
knowing what else is in there. Most of shepherd's helper subprocesses are
uninteresting — fixed argv, output read straight back: `wpctl`/`pactl`/`amixer`,
`pw-dump`, `brightnessctl`, `pgrep`, `pkcheck`, `flatpak --version`.

`yt-dlp` is the exception, and it is scoped like an activity.
`helper_scope_argv_prefix` builds the wrapper; `lunchboxd` injects it into
`lunchbox-media-cache` at startup, because that crate is shared with the player
and the Android build and must not depend on this one. It applies to the two
invocations that touch the network — the download and the playlist fetch — and
not to the `yt-dlp --version` liveness probe, which parses no remote input and
runs on every diagnostics pass.

The reasoning is not that yt-dlp is untrusted code: it is shepherd's own choice
of binary with shepherd's own argv. It is that yt-dlp runs on a background
prefetch timer, with no activity launched, parsing whatever a remote host
returns — so a parser bug there would be a peer the daemon trusts. The URLs come
from admin-configured libraries, so an activity cannot choose the target.

## Administrator mode's launches get one too (issues #144, #154)

`launch_unsupervised` — the spawn behind administrator mode's `.desktop` picker
— wraps its argv in `admin_scope_argv_prefix`, the same `systemd-run --user
--scope` an activity gets. It is the launch path with the *weakest* claim to
shepherd's cgroup, not the strongest: the program is arbitrary third-party code,
chosen from `.desktop` files that an activity can itself write into
`~/.local/share/applications`. As a plain child of the daemon it would be a peer
the management socket believes, holding `unlock_device` and `launch` for as long
as it ran — and, since these launches are `setsid`, potentially long after the
mode ended.

It keeps `setsid` as well: `systemd-run --scope` execs the program in the same
process, so the pid the reaper waits on is still the application's.

Still unscoped, and deliberately: the input-compat sidecars (`sidecar.rs`),
`wl-mirror`, and the pairing overlay. All three are shepherd's own furniture
with no remote input, and two of them need the session's own devices.

## Helper binaries come from trusted directories, not `$PATH` (issue #144)

`lunchboxd` execs a good deal it did not write — `systemd-run`, `pkexec`,
`snap`, `flatpak`, `systemctl`, `pgrep`, `pkcheck`, `script`, `wpctl`, `pactl`,
`amixer`, `pw-dump`, `wl-mirror`. Every one used to be named bare and resolved
through `$PATH`.

That is not safe here, because on a stock 26.04 + GDM host **the kiosk user
chooses the session's environment**: `/etc/pam.d/gdm-password` and
`gdm-autologin` carry `pam_env.so … user_readenv=1`, and `libpam-modules` still
honours it, so `~/.pam_environment` sets `PATH` outright. Every activity runs as
that uid. An activity could write one file, drop its own `systemd-run` on the
resulting `PATH`, and at the next login have shepherd exec it — as a direct
child of the daemon, in the daemon's cgroup, which the management socket accepts
as `Admin`. The same substitution turns `user_scope_argv_prefix` into a no-op,
so every activity would land in shepherd's cgroup too, and nothing would fail
loudly.

`helpers::resolve` therefore does not read the environment at all. It searches a
**compiled-in** list of root-owned directories (`/usr/local/sbin`,
`/usr/local/bin`, `/usr/sbin`, `/usr/bin`, `/sbin`, `/bin`, `/snap/bin`) and
returns an absolute path. Lookups are lazy and cached — `pactl` and `wl-mirror`
are optional, so eager resolution would only have to decide what to do about
tools that are legitimately absent.

- A name that already contains `/` is a caller's deliberate path and is returned
  untouched.
- A name found nowhere resolves to `/usr/bin/<name>` rather than the bare name.
  Returning the bare name would hand the lookup back to `$PATH`; an absolute
  path under a root-owned directory fails at spawn exactly as a missing tool
  always did, and an activity cannot satisfy it.
- `helpers::resolve_daemon_sibling` is the variant for shepherd's *own*
  binaries: `current_exe()`'s directory first (where both an install and a
  `cargo build` put them), then the trusted directories. The input-compat
  sidecars and the pairing overlay use it.
- **In a development session `$PATH` is searched first**, which is exactly the
  behaviour from before #144. It has to be: stubbing a helper by putting a fake
  one on `$PATH` is how the e2e suite tests the flatpak and polkit paths without
  installing either. A device never takes that branch —
  `helpers::set_trust_environment` is off unless `--trust-environment` was
  passed, and `shepherd install sway-config` strips that flag and refuses to
  finish if the strip did not take. Only the e2e suite passes it, because it
  stubs `flatpak`, `pkcheck` and `pkexec` on `$PATH`; an ordinary dev session
  leaves it off and so resolves helpers exactly as a device does.

`SHEPHERD_*_BIN`, `SHEPHERD_FIREWALL_HELPER` and `SHEPHERD_BROWSER_ROOT` all go
through the single gate `helpers::env_override` and are **ignored by default**.
The first two are binary-substitution primitives; the third redirects where the
Chrome managed-policy JSON is written, so leaving it open would let an activity
land the policy somewhere Chrome never reads — the browser lockdown silently not
applying, while the daemon still logs that it did. `lunchboxd` enables them with
`helpers::set_trust_environment`, from its own flag `--trust-environment`.

That is deliberately *not* the flag that disarms the peer check. Both are
development opt-outs, but they are different risks wanted at different times:
`--no-restrict-ipc-peers` decides who may **drive** the daemon, this one decides
which code the daemon **runs**. Coupling them meant every dev and e2e run took
binaries from `$PATH`, so the trusted-directory path was exercised only by unit
tests and on a device.

An activity's own command from `[entries]` is still resolved however the admin
wrote it. `config.toml` is owned by the same uid the activities run as, so that
is the same class of problem — tracked as #156/#157, and a policy decision
rather than a lookup bug.

### The rule is enforced, not just documented

`clippy.toml` disallows `std::process::Command::new` and
`tokio::process::Command::new` workspace-wide, pointing at
`helpers::command()` / `helpers::tokio_command()` instead. Prose in a README
does not survive the next person adding a call site; a denied method does.

Spawning something that is *not* a shepherd-chosen helper is still legitimate
and takes an `#[allow(clippy::disallowed_methods)]` with a comment saying which
exception it is. There are five kinds, and they are the whole list:

- `ManagedProcess::spawn` — `argv[0]` is the activity's own command from
  `config.toml`, the admin's string and not shepherd's to reinterpret.
- The input sidecars and the pairing overlay — already resolved by
  `resolve_daemon_sibling`.
- `lunchbox-firewall-helper` — only ever runs under `pkexec`, which replaces the
  environment with a minimal one; measured, its `PATH` is root-owned throughout.
- `lunchbox-media-cache` — must not depend on this crate, which is why its
  resolver is injected instead.
- Tests, fixtures and build scripts — spawning stand-ins by name is what they
  are for.

The ban caught a live gap the moment it was armed: `brightness.rs` was exec'ing
`brightnessctl` through `$PATH`, missed by the original sweep because the name
was in a `const` rather than a string literal.

The measurements are in
`docs/ai/history/2026-08-29 004 ipc-peer-cgroup-hole-hunt.md`; the regression
tests are `crates/lunchbox-host-linux/tests/helper_resolution.rs`.

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
- `lunchbox-host-api` - Trait definitions
- `lunchbox-api` - Entry types
