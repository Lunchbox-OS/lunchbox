//! Per-activity sidecar processes (e.g., the touch-to-mouse bridge).
//!
//! These run alongside an activity for the duration of its session and are
//! terminated when the activity stops. They never have access to the
//! activity's sandbox; they're plain host processes.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use shepherd_api::{InputCompatMode, InputCompatOptions};
use tracing::{debug, info, warn};

/// Locate a sidecar binary by name.
///
/// Resolution order:
/// 1. The matching `SHEPHERD_*_BIN` env var override — **development only**,
///    gated by [`crate::helpers::env_override`]. It names a binary the daemon
///    will exec as a direct child, and on a device the environment is chosen by
///    the kiosk user (issue #144).
/// 2. A sibling of the running daemon binary (`current_exe()`'s directory),
///    which is where an install puts them and where a `cargo build` leaves them.
/// 3. A trusted system directory.
///
/// Step 3 used to be the bare name, resolved through `$PATH`. That is the hole
/// #144's peer check exists to close: a sidecar is a direct child of the daemon,
/// so it lands in the daemon's cgroup and is accepted on the management socket.
fn sidecar_binary(name: &str, env_override: &str) -> PathBuf {
    if let Some(path) = crate::helpers::env_override(env_override) {
        return path;
    }
    crate::helpers::resolve_daemon_sibling(name)
}

/// Locate the `shepherd-touch-bridge` binary.
pub fn touch_bridge_binary() -> PathBuf {
    sidecar_binary("shepherd-touch-bridge", "SHEPHERD_TOUCH_BRIDGE_BIN")
}

/// Locate the `shepherd-tablet-bridge` binary.
pub fn tablet_bridge_binary() -> PathBuf {
    sidecar_binary("shepherd-tablet-bridge", "SHEPHERD_TABLET_BRIDGE_BIN")
}

/// Locate the `shepherd-gamepad-bridge` binary.
pub fn gamepad_bridge_binary() -> PathBuf {
    sidecar_binary("shepherd-gamepad-bridge", "SHEPHERD_GAMEPAD_BRIDGE_BIN")
}

/// A command for a sidecar binary whose path is already resolved.
///
/// `Command::new` is banned workspace-wide so that naming a binary is always a
/// deliberate act (issue #144). This is the sanctioned exception for the input
/// sidecars: `bin` comes from [`sidecar_binary`], which has already resolved it
/// through `helpers::resolve_daemon_sibling` — a sibling of the running daemon,
/// else a trusted system directory, never a bare name `$PATH` could reinterpret.
#[allow(clippy::disallowed_methods)]
fn sidecar_command(bin: &std::path::Path) -> Command {
    Command::new(bin)
}

/// Spawn the touch-to-mouse bridge as a child of the daemon.
///
/// The bridge maps absolute coordinates onto the output's logical space, which
/// is scale-correct on its own, so no output scale is passed (issue #47).
pub fn spawn_touch_bridge() -> std::io::Result<Child> {
    let bin = touch_bridge_binary();
    debug!(binary = %bin.display(), "Launching touch-to-mouse bridge");
    let child = sidecar_command(&bin)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    info!(pid = child.id(), "Touch-to-mouse bridge spawned");
    Ok(child)
}

/// Spawn the touch bridge in grab-only mode as a child of the daemon.
///
/// This grabs every touchscreen and discards its events, disabling the
/// touchscreen for the lifetime of the activity. It reuses the touch-bridge
/// binary's device discovery and grab logic but emits no synthetic events, so
/// no `/dev/uinput` access is required.
pub fn spawn_disable_touch() -> std::io::Result<Child> {
    let bin = touch_bridge_binary();
    debug!(binary = %bin.display(), "Launching touchscreen grab (disable touch)");
    let child = sidecar_command(&bin)
        .arg("--grab-only")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    info!(pid = child.id(), "Touchscreen-disable grab spawned");
    Ok(child)
}

/// Spawn the tablet-to-touch bridge as a child of the daemon.
///
/// Like the touch bridge, the synthesized device's range maps onto the
/// output's logical space, so no output scale is passed (issue #47).
pub fn spawn_tablet_bridge() -> std::io::Result<Child> {
    let bin = tablet_bridge_binary();
    debug!(binary = %bin.display(), "Launching tablet-to-touch bridge");
    let child = sidecar_command(&bin)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    info!(pid = child.id(), "Tablet-to-touch bridge spawned");
    Ok(child)
}

/// Spawn the gamepad-to-keyboard+mouse bridge for the given preset, with the
/// supplied tunables forwarded as CLI flags.
pub fn spawn_gamepad_bridge(
    preset: GamepadPreset,
    options: &InputCompatOptions,
) -> std::io::Result<Child> {
    let bin = gamepad_bridge_binary();
    let mut cmd = sidecar_command(&bin);
    cmd.arg("--preset").arg(preset.as_cli());
    if let Some(v) = options.gamepad_deadzone {
        cmd.arg("--deadzone").arg(format!("{v}"));
    }
    if let Some(v) = options.gamepad_mouse_speed {
        cmd.arg("--mouse-speed").arg(format!("{v}"));
    }
    if let Some(v) = options.gamepad_scroll_speed {
        cmd.arg("--scroll-speed").arg(format!("{v}"));
    }
    info!(
        binary = %bin.display(),
        preset = preset.as_cli(),
        "Launching gamepad bridge"
    );
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .inspect_err(|e| {
            // Log path + error explicitly so a missing/unreachable binary
            // doesn't disappear into a generic "spawn failed" message.
            warn!(
                binary = %bin.display(),
                error = %e,
                "Failed to exec gamepad bridge binary"
            );
        })?;
    info!(
        pid = child.id(),
        preset = preset.as_cli(),
        "Gamepad bridge spawned"
    );
    Ok(child)
}

/// Gamepad preset selection for the sidecar CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamepadPreset {
    Productivity,
    Gpd,
}

impl GamepadPreset {
    pub fn from_mode(mode: InputCompatMode) -> Option<Self> {
        match mode {
            InputCompatMode::GamepadProductivity => Some(Self::Productivity),
            InputCompatMode::GamepadGpd => Some(Self::Gpd),
            InputCompatMode::TouchToMouse
            | InputCompatMode::TabletToTouch
            | InputCompatMode::DisableTouch => None,
        }
    }

    pub fn as_cli(self) -> &'static str {
        match self {
            Self::Productivity => "productivity",
            Self::Gpd => "gpd",
        }
    }
}

/// Send SIGTERM, wait briefly for graceful exit, then SIGKILL if needed.
/// Always reaps the child to avoid zombies.
pub fn terminate_sidecar(mut child: Child, label: &str) {
    let pid = child.id();
    if kill(Pid::from_raw(pid as i32), Signal::SIGTERM).is_err() {
        // Already gone — just reap.
        let _ = child.wait();
        return;
    }

    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                debug!(label = %label, pid = pid, status = ?status, "Sidecar exited");
                return;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                warn!(label = %label, pid = pid, error = %e, "Failed to wait on sidecar");
                return;
            }
        }
    }

    warn!(label = %label, pid = pid, "Sidecar did not exit on SIGTERM; sending SIGKILL");
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
    let _ = child.wait();
}
