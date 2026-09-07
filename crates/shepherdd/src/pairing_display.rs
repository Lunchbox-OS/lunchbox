//! [`PairingDisplay`] backed by the `shepherd-pairing-display`
//! subprocess.
//!
//! The BLE pairing agent (in shepherd-ble) is `Send + Sync` and calls
//! [`show`](shepherd_ble::PairingDisplay::show) /
//! [`hide`](shepherd_ble::PairingDisplay::hide) synchronously. We
//! launch the overlay as a child process so we don't have to drag
//! GTK into the daemon; killing the child tears the overlay down.

use shepherd_ble::{PairingDisplay, PairingMethod};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Name of the binary that renders the overlay. Resolved via `PATH`,
/// which matches the rest of the project (HUD, bridges, etc. are
/// invoked the same way).
const BINARY: &str = "shepherd-pairing-display";

/// `PairingDisplay` implementation that fans out to the
/// `shepherd-pairing-display` subprocess.
pub struct SwayPairingDisplay {
    current: Mutex<Option<Child>>,
}

impl SwayPairingDisplay {
    pub fn new() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }
}

impl Default for SwayPairingDisplay {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingDisplay for SwayPairingDisplay {
    fn show(&self, device_address: &str, passkey: u32, method: PairingMethod) {
        // Replace any previous overlay first — a second pairing attempt
        // before the first hide timed out should still show the new
        // passkey rather than stack windows.
        self.hide();

        let method_arg = match method {
            PairingMethod::Compare => "compare",
            PairingMethod::Enter => "enter",
        };

        // Resolved like the input sidecars rather than exec'd by bare name:
        // this is a direct child of the daemon, so `$PATH` deciding which binary
        // runs would put a chosen one in the daemon's cgroup (issue #144).
        // Already resolved to a sibling of the running daemon (or a trusted
        // system directory), so this is not a bare name `$PATH` could
        // reinterpret — the case `Command::new`'s ban is aimed at (issue #144).
        #[allow(clippy::disallowed_methods)]
        let mut cmd = Command::new(shepherd_host_linux::resolve_daemon_sibling(BINARY));
        cmd.args([
            "--passkey",
            &passkey.to_string(),
            "--device",
            device_address,
            "--method",
            method_arg,
        ])
        .stdin(Stdio::null())
        // Inherit stdout/stderr so the sidecar's tracing output (and
        // any GTK / Wayland errors) lands in shepherdd's journal.
        // Silently swallowing stderr previously cost us a debug cycle
        // when a GLib option-parser error killed the sidecar before
        // its activate handler could fire.
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

        match cmd.spawn() {
            Ok(child) => {
                info!(
                    pid = child.id(),
                    passkey, device = %device_address, method = method_arg,
                    "Spawned pairing-display overlay",
                );
                *self.current.lock().expect("display lock poisoned") = Some(child);
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Failed to spawn '{BINARY}' for pairing overlay; pairing will proceed without an on-device visual",
                );
            }
        }
    }

    fn hide(&self) {
        let mut guard = self.current.lock().expect("display lock poisoned");
        if let Some(mut child) = guard.take() {
            match child.kill() {
                Ok(()) => {
                    // Reap so we don't leak a zombie.
                    let _ = child.wait();
                    debug!("Pairing-display overlay terminated");
                }
                Err(e) => {
                    warn!(error = %e, "Failed to kill pairing-display overlay");
                }
            }
        }
    }
}

impl Drop for SwayPairingDisplay {
    fn drop(&mut self) {
        self.hide();
    }
}

/// The web management setup code on the television (issue #156).
///
/// The same binary as the pairing overlay, in its corner-card mode, and a
/// separate handle rather than a second `PairingDisplay`: this one is up for
/// minutes and must not be torn down by a pairing attempt that happens while
/// it is showing, nor tear that pairing overlay down when it goes.
pub struct SetupCodeDisplay {
    child: Option<Child>,
}

impl SetupCodeDisplay {
    /// Show the card. Returns a handle that hides it when dropped.
    ///
    /// A failure to spawn is logged and nothing else: the code is also in the
    /// journal, and a device with no compositor up yet — or no overlay binary
    /// installed — must still finish starting.
    pub fn show(code: &str, urls: &[String], port: Option<u16>) -> Self {
        #[allow(clippy::disallowed_methods)]
        let mut cmd = Command::new(shepherd_host_linux::resolve_daemon_sibling(BINARY));
        cmd.args(["--setup-code", code]);
        // One `--url` per way in. A wildcard bind is reachable at one address
        // per network the device is on (issue #182), and which of them the
        // parent's laptop can use is not something this end can know.
        for url in urls {
            cmd.args(["--url", url]);
        }
        if let Some(port) = port {
            cmd.args(["--port", &port.to_string()]);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        match cmd.spawn() {
            Ok(child) => {
                info!(
                    pid = child.id(),
                    "Showing the management setup code on screen"
                );
                Self { child: Some(child) }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Could not show the setup code on screen; it is in the log instead",
                );
                Self { child: None }
            }
        }
    }
}

impl Drop for SetupCodeDisplay {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
