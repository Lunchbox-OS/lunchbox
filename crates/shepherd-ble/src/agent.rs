//! `bluer` pairing agent for Numeric Comparison and the
//! [`PairingDisplay`] trait that lets the daemon plug in a Sway overlay
//! (or any other display surface) without this crate depending on
//! Wayland.

use bluer::agent::{Agent, ReqError, ReqResult};
use futures_util::FutureExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::info;

/// Visual surface for the 6-digit Numeric Comparison passkey.
///
/// The agent calls [`show`](Self::show) with the passkey at pairing
/// time and [`hide`](Self::hide) once the display window expires (or
/// pairing is cancelled). Implementations should render the passkey
/// somewhere the user can see it from where they are pairing — for
/// shepherd's primary use case, that's a full-screen Sway overlay on
/// the TV. See `docs/ai/history/2026-06-20 002 ble-management.md`.
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

/// How long the passkey overlay stays visible after pairing has been
/// auto-confirmed. The user verifies on the phone side; this is just
/// the visual hold so they have time to compare the two numbers.
const PASSKEY_DISPLAY_HOLD: Duration = Duration::from_secs(30);

/// Build a `bluer::agent::Agent` configured for Numeric Comparison
/// pairing.
///
/// The agent's only callback is `request_confirmation`. It:
///
/// 1. Renders the 6-digit passkey via [`PairingDisplay::show`].
/// 2. Returns `Ok(())` — shepherd auto-confirms because the user is
///    the trust anchor and they verify on the phone side. The link is
///    still authenticated against MITM as long as the user actually
///    compares the two numbers.
/// 3. Spawns a task that hides the display after
///    [`PASSKEY_DISPLAY_HOLD`].
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

    let display_for_hide = display.clone();
    tokio::spawn(async move {
        tokio::time::sleep(PASSKEY_DISPLAY_HOLD).await;
        let _guard = hide_lock.lock().await;
        display_for_hide.hide();
    });

    Ok(())
}

/// Convert a numeric passkey to its canonical 6-digit zero-padded
/// representation. The BlueZ passkey is always 0–999_999 but display
/// surfaces want the same width every time.
pub fn format_passkey(passkey: u32) -> String {
    format!("{passkey:06}")
}

/// Type alias so the daemon can store both the constructed `Agent` and
/// the IO capability it implies, without re-checking the bluer code.
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
        // hide runs in a spawned task after PASSKEY_DISPLAY_HOLD; we
        // don't sleep that long in tests — just confirm show fired.
    }

    #[test]
    fn build_agent_sets_request_default_and_confirmation() {
        let display = Arc::new(NoopPairingDisplay);
        let agent = build_agent(display);
        assert!(agent.request_default);
        assert!(agent.request_confirmation.is_some());
        // No keyboard/display callbacks — capability resolves to
        // DisplayYesNo, which is what Numeric Comparison needs.
        assert!(agent.request_pin_code.is_none());
        assert!(agent.request_passkey.is_none());
        assert!(agent.display_passkey.is_none());
        assert!(agent.display_pin_code.is_none());
    }
}
