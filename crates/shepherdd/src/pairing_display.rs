//! [`PairingDisplay`] backed by the `shepherd-pairing-display`
//! subprocess.
//!
//! The BLE pairing agent (in shepherd-ble) is `Send + Sync` and calls
//! [`show`](shepherd_ble::PairingDisplay::show) /
//! [`hide`](shepherd_ble::PairingDisplay::hide) synchronously. We
//! launch the overlay as a child process so we don't have to drag
//! GTK into the daemon; killing the child tears the overlay down.

use shepherd_ble::PairingDisplay;
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
    fn show(&self, device_address: &str, passkey: u32) {
        // Replace any previous overlay first — a second pairing attempt
        // before the first hide timed out should still show the new
        // passkey rather than stack windows.
        self.hide();

        let mut cmd = Command::new(BINARY);
        cmd.args([
            "--passkey",
            &passkey.to_string(),
            "--device",
            device_address,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

        match cmd.spawn() {
            Ok(child) => {
                info!(
                    pid = child.id(),
                    passkey, device = %device_address,
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
