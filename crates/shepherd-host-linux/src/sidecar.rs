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
use tracing::{debug, info, warn};

/// Locate the `shepherd-touch-bridge` binary.
///
/// Resolution order:
/// 1. `SHEPHERD_TOUCH_BRIDGE_BIN` env var (absolute or PATH-resolvable).
/// 2. A sibling of the running daemon binary (`current_exe()`'s directory).
/// 3. The literal name `shepherd-touch-bridge`, which `Command` will look
///    up via `PATH` at spawn time.
pub fn touch_bridge_binary() -> PathBuf {
    if let Ok(val) = std::env::var("SHEPHERD_TOUCH_BRIDGE_BIN")
        && !val.is_empty()
    {
        return PathBuf::from(val);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("shepherd-touch-bridge");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("shepherd-touch-bridge")
}

/// Spawn the touch-to-mouse bridge as a child of the daemon. Returns the
/// `Child` so callers can track and terminate it. Errors here are
/// non-fatal: the activity should still launch even if the bridge fails.
pub fn spawn_touch_bridge() -> std::io::Result<Child> {
    let bin = touch_bridge_binary();
    debug!(binary = %bin.display(), "Launching touch-to-mouse bridge");
    let child = Command::new(&bin)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    info!(pid = child.id(), "Touch-to-mouse bridge spawned");
    Ok(child)
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
