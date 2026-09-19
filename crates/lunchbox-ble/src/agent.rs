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
use bluer::{DeviceEvent, DeviceProperty};
use futures_util::{FutureExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Which SMP pairing method we landed in, so the display can render
/// the right instruction copy. The number on screen is the same
/// either way; only the user action differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingMethod {
    /// LESC Numeric Comparison — both sides display, user confirms
    /// match on the phone.
    Compare,
    /// Passkey Entry, responder-displays — we display, user types
    /// the number into the phone. Selected on LE Legacy Pairing and
    /// on some LESC IO-cap combinations.
    Enter,
}

/// Visual surface for the 6-digit pairing passkey.
///
/// Implementations should render the passkey somewhere the user can
/// see it from where they are pairing — for Lunchbox's primary use
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
    fn show(&self, device_address: &str, passkey: u32, method: PairingMethod);
    fn hide(&self);
}

/// No-op display for headless tests and the period before a real
/// display has been wired in. Pairing still proceeds; the user just
/// has no on-device visual to verify against (so this should never be
/// used in production).
pub struct NoopPairingDisplay;

impl PairingDisplay for NoopPairingDisplay {
    fn show(&self, _device_address: &str, _passkey: u32, _method: PairingMethod) {}
    fn hide(&self) {}
}

/// Upper bound on how long the passkey overlay stays visible after
/// the agent has answered BlueZ. Normally we hide much sooner — as
/// soon as BlueZ reports `Device1.Paired = true` (see
/// [`spawn_hide_when_paired_or_timeout`]) — but if that signal
/// never arrives (cancel, dropped link, BlueZ bug) the overlay
/// disappears at this deadline so it doesn't sit on screen forever.
const PASSKEY_DISPLAY_HOLD: Duration = Duration::from_secs(60);

/// Build a `bluer::agent::Agent` that handles both SMP outcomes our
/// `DisplayYesNo` capability can land in.
///
/// `request_default: true` asks BlueZ to make us the default agent so
/// system-initiated pairing dialogs route here too.
///
/// `session` is captured so the handlers can subscribe to BlueZ's
/// `Device1.Paired` property and hide the overlay the moment bonding
/// completes — without this the overlay would stay up for the full
/// [`PASSKEY_DISPLAY_HOLD`] window.
pub fn build_agent(session: bluer::Session, display: Arc<dyn PairingDisplay>) -> Agent {
    // The display itself must outlive every spawned hide task; serialize
    // hide calls so an old hide from a previous pairing can't race
    // with a new show.
    let hide_lock = Arc::new(Mutex::new(()));

    let session_for_confirm = session.clone();
    let display_for_confirm = display.clone();
    let hide_lock_for_confirm = hide_lock.clone();

    let session_for_passkey = session;
    let display_for_passkey = display.clone();
    let hide_lock_for_passkey = hide_lock.clone();

    Agent {
        request_default: true,
        request_confirmation: Some(Box::new(move |req| {
            let session = session_for_confirm.clone();
            let display = display_for_confirm.clone();
            let hide_lock = hide_lock_for_confirm.clone();
            async move {
                handle_request_confirmation(
                    session,
                    req.adapter,
                    req.device,
                    req.passkey,
                    display,
                    hide_lock,
                )
                .await
            }
            .boxed()
        })),
        display_passkey: Some(Box::new(move |req| {
            let session = session_for_passkey.clone();
            let display = display_for_passkey.clone();
            let hide_lock = hide_lock_for_passkey.clone();
            async move {
                handle_display_passkey(
                    session,
                    req.adapter,
                    req.device,
                    req.passkey,
                    display,
                    hide_lock,
                )
                .await
            }
            .boxed()
        })),
        ..Default::default()
    }
}

async fn handle_request_confirmation(
    session: bluer::Session,
    adapter_name: String,
    device_address: bluer::Address,
    passkey: u32,
    display: Arc<dyn PairingDisplay>,
    hide_lock: Arc<Mutex<()>>,
) -> ReqResult<()> {
    let device_str = device_address.to_string();
    info!(
        device = %device_str,
        passkey,
        "Numeric Comparison pairing requested; displaying passkey on TV",
    );
    display.show(&device_str, passkey, PairingMethod::Compare);
    spawn_hide_when_paired_or_timeout(session, adapter_name, device_address, display, hide_lock);
    Ok(())
}

async fn handle_display_passkey(
    session: bluer::Session,
    adapter_name: String,
    device_address: bluer::Address,
    passkey: u32,
    display: Arc<dyn PairingDisplay>,
    hide_lock: Arc<Mutex<()>>,
) -> ReqResult<()> {
    let device_str = device_address.to_string();
    info!(
        device = %device_str,
        passkey,
        "Passkey Entry pairing requested; displaying passkey on TV (phone will prompt for entry)",
    );
    display.show(&device_str, passkey, PairingMethod::Enter);
    spawn_hide_when_paired_or_timeout(session, adapter_name, device_address, display, hide_lock);
    Ok(())
}

/// Spawn a task that hides the overlay on the first of:
///
/// - BlueZ reports `Device1.Paired = true` for the peer (success path).
/// - [`PASSKEY_DISPLAY_HOLD`] elapses (cancel / failure fallback).
fn spawn_hide_when_paired_or_timeout(
    session: bluer::Session,
    adapter_name: String,
    device_address: bluer::Address,
    display: Arc<dyn PairingDisplay>,
    hide_lock: Arc<Mutex<()>>,
) {
    tokio::spawn(async move {
        match tokio::time::timeout(
            PASSKEY_DISPLAY_HOLD,
            wait_for_paired(&session, &adapter_name, device_address),
        )
        .await
        {
            Ok(Ok(())) => {
                info!(device = %device_address, "Pairing complete; hiding passkey overlay")
            }
            Ok(Err(e)) => warn!(
                device = %device_address,
                error = %e,
                "Failed to watch for pairing completion; hiding overlay after timeout"
            ),
            Err(_) => info!(
                device = %device_address,
                "Passkey overlay timed out without pairing-completion signal; hiding"
            ),
        }
        let _guard = hide_lock.lock().await;
        display.hide();
    });
}

async fn wait_for_paired(
    session: &bluer::Session,
    adapter_name: &str,
    device_address: bluer::Address,
) -> bluer::Result<()> {
    let adapter = session.adapter(adapter_name)?;
    let device = adapter.device(device_address)?;

    // Check current state in case Paired flipped before we started
    // listening (the event stream is registered after the property
    // read, so a tiny race window exists).
    if device.is_paired().await? {
        return Ok(());
    }

    let mut events = device.events().await?;
    while let Some(event) = events.next().await {
        if let DeviceEvent::PropertyChanged(DeviceProperty::Paired(true)) = event {
            return Ok(());
        }
    }
    // Event stream ended (device removed) without a Paired=true.
    // The outer timeout will catch this; treat as a "no signal" no-op.
    Ok(())
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
        last_method: std::sync::Mutex<Option<PairingMethod>>,
    }

    impl PairingDisplay for CountingDisplay {
        fn show(&self, _addr: &str, _passkey: u32, method: PairingMethod) {
            self.shown.fetch_add(1, Ordering::SeqCst);
            *self.last_method.lock().unwrap() = Some(method);
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

    /// Round-trips a single show() call through CountingDisplay,
    /// verifying the method enum reaches the display correctly. The
    /// agent handlers themselves now take a `bluer::Session` and spawn
    /// a watcher task, which would need a live BlueZ to exercise — so
    /// the show-mapping check is split off here.
    #[test]
    fn counting_display_records_method() {
        let display = CountingDisplay::default();
        display.show("AA:BB:CC:DD:EE:FF", 42, PairingMethod::Enter);
        assert_eq!(display.shown.load(Ordering::SeqCst), 1);
        assert_eq!(
            *display.last_method.lock().unwrap(),
            Some(PairingMethod::Enter)
        );
        display.show("AA:BB:CC:DD:EE:FF", 7, PairingMethod::Compare);
        assert_eq!(display.shown.load(Ordering::SeqCst), 2);
        assert_eq!(
            *display.last_method.lock().unwrap(),
            Some(PairingMethod::Compare)
        );
    }

    /// Regression guard on the agent's IO capability — adding a
    /// `display_passkey` callback should not promote us away from
    /// DisplayYesNo (which would change the SMP pairing-method
    /// negotiation). Requires only a D-Bus system socket, which all
    /// Linux test environments have; doesn't require bluetoothd.
    #[tokio::test]
    async fn build_agent_registers_both_pairing_paths() {
        let Ok(session) = bluer::Session::new().await else {
            eprintln!("Skipping build_agent test: bluer session unavailable");
            return;
        };
        let display = Arc::new(NoopPairingDisplay);
        let agent = build_agent(session, display);
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
