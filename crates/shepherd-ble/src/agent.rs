//! `bluer` pairing agent and the [`PairingDisplay`] trait that lets
//! the daemon plug in a Sway overlay (or any other display surface)
//! without this crate depending on Wayland.
//!
//! The agent handles two SMP outcomes:
//!
//! - **Numeric Comparison** (LESC; BT 4.2+) — both sides show the same
//!   6-digit number, user confirms it matches. Wired through
//!   `request_confirmation`.
//! - **Passkey Entry, responder-displays** (LE Legacy, also LESC if the
//!   IO caps land there) — we display the 6-digit passkey, the phone
//!   asks the user to type it. Wired through `display_passkey`.
//!
//! Older controllers (the leibniz dev box runs an HCI 4.0 Marvell
//! adapter) lack LESC entirely, so the second path is the only one
//! that can complete in practice. Both call into [`PairingDisplay`];
//! the on-device UX is identical — show the number, ask the user to
//! match or type it on the phone — and the security property is the
//! same MITM-protected passkey exchange.

use bluer::agent::{Agent, ReqError, ReqResult};
use futures_util::FutureExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::info;

/// Visual surface for the 6-digit pairing passkey.
///
/// Implementations should render the passkey somewhere the user can
/// see it from where they are pairing — for shepherd's primary use
/// case, that's a full-screen Sway overlay on the TV. See
/// `docs/ai/history/2026-06-20 002 ble-management.md`.
///
/// The agent calls [`show`](Self::show) at pairing time and
/// [`hide`](Self::hide) once the display window expires (or pairing
/// is cancelled).
///
/// Implementations must be `Send + Sync` because the agent owns its
/// own task.
pub trait PairingDisplay: Send + Sync + 'static {
    fn show(&self, device_address: &str, passkey: u32);
    fn hide(&self);
}

/// No-op display for headless tests and the period before a real
/// display has been wired in. Pairing still proceeds; the user just
/// has no on-device visual to verify against (so this should never be
/// used in production).
pub struct NoopPairingDisplay;

impl PairingDisplay for NoopPairingDisplay {
    fn show(&self, _device_address: &str, _passkey: u32) {}
    fn hide(&self) {}
}

/// How long the passkey overlay stays visible after the agent has
/// answered BlueZ. For Passkey Entry this is the upper bound on how
/// long the user has to type the number on the phone; for Numeric
/// Comparison it's just the visual hold so they can finish reading.
const PASSKEY_DISPLAY_HOLD: Duration = Duration::from_secs(60);

/// Build a `bluer::agent::Agent` that handles both SMP outcomes our
/// `DisplayYesNo` capability can land in.
///
/// `request_default: true` asks BlueZ to make us the default agent so
/// system-initiated pairing dialogs route here too.
pub fn build_agent(display: Arc<dyn PairingDisplay>) -> Agent {
    // The display itself must outlive every spawned hide task; serialize
    // hide calls so an old hide from a previous pairing can't race
    // with a new show.
    let hide_lock = Arc::new(Mutex::new(()));

    let display_for_confirm = display.clone();
    let hide_lock_for_confirm = hide_lock.clone();

    let display_for_passkey = display.clone();
    let hide_lock_for_passkey = hide_lock.clone();

    Agent {
        request_default: true,
        request_confirmation: Some(Box::new(move |req| {
            let display = display_for_confirm.clone();
            let hide_lock = hide_lock_for_confirm.clone();
            async move {
                handle_request_confirmation(req.device.to_string(), req.passkey, display, hide_lock)
                    .await
            }
            .boxed()
        })),
        display_passkey: Some(Box::new(move |req| {
            let display = display_for_passkey.clone();
            let hide_lock = hide_lock_for_passkey.clone();
            async move {
                handle_display_passkey(req.device.to_string(), req.passkey, display, hide_lock)
                    .await
            }
            .boxed()
        })),
        ..Default::default()
    }
}

async fn handle_request_confirmation(
    device_address: String,
    passkey: u32,
    display: Arc<dyn PairingDisplay>,
    hide_lock: Arc<Mutex<()>>,
) -> ReqResult<()> {
    info!(
        device = %device_address,
        passkey,
        "Numeric Comparison pairing requested; displaying passkey on TV",
    );
    display.show(&device_address, passkey);
    spawn_hide_after_hold(display, hide_lock);
    Ok(())
}

async fn handle_display_passkey(
    device_address: String,
    passkey: u32,
    display: Arc<dyn PairingDisplay>,
    hide_lock: Arc<Mutex<()>>,
) -> ReqResult<()> {
    info!(
        device = %device_address,
        passkey,
        "Passkey Entry pairing requested; displaying passkey on TV (phone will prompt for entry)",
    );
    display.show(&device_address, passkey);
    spawn_hide_after_hold(display, hide_lock);
    Ok(())
}

fn spawn_hide_after_hold(display: Arc<dyn PairingDisplay>, hide_lock: Arc<Mutex<()>>) {
    tokio::spawn(async move {
        tokio::time::sleep(PASSKEY_DISPLAY_HOLD).await;
        let _guard = hide_lock.lock().await;
        display.hide();
    });
}

/// Convert a numeric passkey to its canonical 6-digit zero-padded
/// representation. The BlueZ passkey is always 0–999_999 but display
/// surfaces want the same width every time.
pub fn format_passkey(passkey: u32) -> String {
    format!("{passkey:06}")
}

/// IO capability our agent advertises to BlueZ.
///
/// Note this stays "DisplayYesNo" even after adding `display_passkey`:
/// bluer's capability resolution treats `display_passkey` together
/// with `request_confirmation` as a single "we can show things and
/// answer yes/no" surface, and intentionally does not promote us to
/// `KeyboardDisplay` (which would imply we can take user input — we
/// can't). See `bluer::agent::Agent::capability` for the table.
pub const AGENT_IO_CAPABILITY: &str = "DisplayYesNo";

// Reference so the `ReqError` import doesn't bitrot if we add an
// explicit error path later.
#[allow(dead_code)]
fn _reject() -> ReqResult<()> {
    Err(ReqError::Rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Default)]
    struct CountingDisplay {
        shown: AtomicU32,
        hidden: AtomicU32,
    }

    impl PairingDisplay for CountingDisplay {
        fn show(&self, _addr: &str, _passkey: u32) {
            self.shown.fetch_add(1, Ordering::SeqCst);
        }
        fn hide(&self) {
            self.hidden.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn passkey_zero_padded() {
        assert_eq!(format_passkey(0), "000000");
        assert_eq!(format_passkey(42), "000042");
        assert_eq!(format_passkey(123456), "123456");
        assert_eq!(format_passkey(999_999), "999999");
    }

    #[tokio::test]
    async fn confirmation_handler_shows_and_returns_ok() {
        let display = Arc::new(CountingDisplay::default());
        let hide_lock = Arc::new(Mutex::new(()));
        let result = handle_request_confirmation(
            "AA:BB:CC:DD:EE:FF".to_string(),
            123456,
            display.clone(),
            hide_lock,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(display.shown.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn display_passkey_handler_shows_and_returns_ok() {
        let display = Arc::new(CountingDisplay::default());
        let hide_lock = Arc::new(Mutex::new(()));
        let result = handle_display_passkey(
            "AA:BB:CC:DD:EE:FF".to_string(),
            42,
            display.clone(),
            hide_lock,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(display.shown.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn build_agent_registers_both_pairing_paths() {
        let display = Arc::new(NoopPairingDisplay);
        let agent = build_agent(display);
        assert!(agent.request_default);
        // Numeric Comparison (LESC) and Passkey Entry: responder-displays
        // (Legacy or LESC) both terminate at our agent.
        assert!(agent.request_confirmation.is_some());
        assert!(agent.display_passkey.is_some());
        // We never take user input on the device side, so the keyboard /
        // PIN callbacks stay unset — capability resolves to DisplayYesNo.
        assert!(agent.request_pin_code.is_none());
        assert!(agent.request_passkey.is_none());
        assert!(agent.display_pin_code.is_none());
    }
}
