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
/// 1. The matching `SHEPHERD_*_BIN` env var override, if set and non-empty.
/// 2. A sibling of the running daemon binary (`current_exe()`'s directory).
/// 3. The literal binary name, which `Command` resolves via `PATH`.
fn sidecar_binary(name: &str, env_override: &str) -> PathBuf {
    if let Ok(val) = std::env::var(env_override)
        && !val.is_empty()
    {
        return PathBuf::from(val);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from(name)
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

/// Spawn the touch-to-mouse bridge as a child of the daemon.
///
/// `output_scale` is the compositor's current output scale; the bridge
/// divides its absolute coordinates by it so the synthesized cursor lands in
/// logical (scaled) coordinates. Pass `1.0` when scaling is unknown.
pub fn spawn_touch_bridge(output_scale: f64) -> std::io::Result<Child> {
    let bin = touch_bridge_binary();
    debug!(binary = %bin.display(), output_scale, "Launching touch-to-mouse bridge");
    let child = Command::new(&bin)
        .arg("--output-scale")
        .arg(format!("{output_scale}"))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    info!(pid = child.id(), "Touch-to-mouse bridge spawned");
    Ok(child)
}

/// Spawn the tablet-to-touch bridge as a child of the daemon.
///
/// `output_scale` is the compositor's current output scale; the bridge divides
/// its absolute coordinates by it so synthesized contacts land in logical
/// (scaled) coordinates. Pass `1.0` when scaling is unknown.
pub fn spawn_tablet_bridge(output_scale: f64) -> std::io::Result<Child> {
    let bin = tablet_bridge_binary();
    debug!(binary = %bin.display(), output_scale, "Launching tablet-to-touch bridge");
    let child = Command::new(&bin)
        .arg("--output-scale")
        .arg(format!("{output_scale}"))
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
    let mut cmd = Command::new(&bin);
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
            InputCompatMode::TouchToMouse | InputCompatMode::TabletToTouch => None,
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
