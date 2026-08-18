//! `BleServer`: lifecycle for the GATT application, advertising, and
//! the pairing agent.
//!
//! Transport shape:
//!
//! - **Request** is a write characteristic (chunked, length-prefixed
//!   JSON-RPC; encrypt-authenticated link required).
//! - **Response** and **Events** are *read-poll* characteristics, not
//!   notify. Each holds an [`Outbox`] byte queue; the companion app
//!   drains it with repeated GATT reads. Notify was the original
//!   design but proved unreliable for bonded reconnects — BlueZ
//!   caches CCCD state at the bond, Android short-circuits subsequent
//!   CCCD writes, and the per-subscribe notify task from the *first*
//!   session ends up the only one the wire ever reaches. Read-poll
//!   sidesteps all of that.
//! - **DeviceInfo** is an unencrypted read for the pre-pair "what is
//!   this device" probe.
//!
//! A single long-lived subscriber task forwards every
//! [`shepherd_api::Event`] from [`ManagementService::subscribe_events`]
//! into the events outbox throughout the server's lifetime. There's no
//! per-client subscriber, so events that fire while no companion is
//! polling accumulate in the outbox; the companion re-syncs current
//! state by calling `service_state` on every reconnect.
//!
//! Keeping that accumulation *small* is load-bearing, not housekeeping.
//! The companion drains both outboxes to empty inside `connect()` before
//! it sends its first RPC, at 512 bytes per GATT round trip — so backlog
//! is connect latency, and unbounded backlog is a connect that never
//! finishes. `StateChanged` is therefore pushed coalesced (a newer
//! snapshot supersedes the queued one) and the capacity is deliberately
//! tight. See `docs/ai/history/2026-08-01 001
//! ble-connect-drain-unbounded.md`.
//!
//! v1 still assumes a single bonded admin peer at a time (TOFU
//! single-admin model from the design doc).

use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::agent::AgentHandle;
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteMethod, Service,
};
use bluer::{AdapterEvent, AdapterProperty, Address, DeviceEvent, DeviceProperty};
use futures_util::{FutureExt, StreamExt};
use shepherd_api::EventPayload;
use shepherd_management::ManagementService;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::admin::{AdminStore, PendingUnbondStore, check_reset_sentinel};
use crate::agent::{PairingDisplay, build_agent};
use crate::claim::{AuthDecision, ClaimMachine, PeerIdentity};
use crate::framing::{FrameReader, encode_frame};
use crate::outbox::{COALESCE_STATE_CHANGED, CoalesceKey, Outbox};
use crate::protocol::{
    ClaimStateTag, DeviceInfo, ErrorCode, MAX_FRAME_BYTES, PROTOCOL_VERSION, RpcRequest,
    RpcResponse, SHEPHERD_DEVICE_INFO_CHAR_UUID, SHEPHERD_EVENTS_CHAR_UUID,
    SHEPHERD_MANAGEMENT_SERVICE_UUID, SHEPHERD_REQUEST_CHAR_UUID, SHEPHERD_RESPONSE_CHAR_UUID,
};
use crate::rpc::dispatch_management;

/// Cap on the Response outbox. A handful of responses (each a few KiB
/// at most for `service_state`) fit comfortably; this is a soft ceiling
/// that exists only so a buggy or AFK companion can't grow the queue
/// without bound. Backpressure here is unusual in practice — responses
/// are emitted at the rate the companion sends requests.
const RESPONSE_OUTBOX_BYTES: usize = 256 * 1024;

/// Cap on the Events outbox.
///
/// This is a *latency* budget, not a storage budget. Reads are capped at
/// [`GATT_MAX_ATTR_VALUE`] and cost a GATT round trip each, so the
/// companion drains at only ~10–20 KiB/s — and it drains the whole queue
/// during `connect()` before it will talk to us. At 256 KiB (the previous
/// value) that alone was 15–25 seconds of dead time on every reconnect,
/// and under a steady event rate the drain never converged at all: the
/// companion hung in `connect()` indefinitely with nothing logged
/// server-side. See `docs/ai/history/2026-08-01 001
/// ble-connect-drain-unbounded.md`.
///
/// 64 KiB holds one full `StateChanged` snapshot (up to ~20 KiB once a
/// few entries are configured) plus ample room for the smaller
/// session/policy events, and bounds a worst-case cold drain to a few
/// seconds. The real backlog control is
/// [`Outbox::push_coalesced`] — snapshots supersede one another rather
/// than queueing up — so this ceiling is rarely approached.
const EVENTS_OUTBOX_BYTES: usize = 64 * 1024;

/// Bluetooth Core spec ceiling on a single GATT attribute value.
/// Android's stack enforces this strictly: even with an ATT MTU of
/// 517 (allowing 516-byte read responses on the wire), a single
/// `BluetoothGatt.readCharacteristic` call delivers at most 512 bytes
/// to the application, silently truncating the rest. We cap the
/// outbox read chunk here so each ATT response fits inside what the
/// client will actually receive.
const GATT_MAX_ATTR_VALUE: usize = 512;

/// Longest advertised device name, in bytes, that still leaves the
/// 128-bit management service UUID room in a legacy 31-byte advertising
/// PDU. The controller packs, in the same 31 bytes: the mandatory Flags
/// AD (3 bytes), the 128-bit service-UUID AD (2 header + 16 = 18 bytes),
/// and the local-name AD (2 header + the name). Overrun and BlueZ
/// rejects the *whole* advertisement with "Invalid Parameters (0x0d)",
/// so the service UUID never goes on air and the companion's
/// UUID-filtered scan finds nothing — the failure looks like "the device
/// won't pair" but is really "the device is undiscoverable". We advertise
/// a name trimmed to this budget and still serve the full one over GATT.
const MAX_ADV_NAME_BYTES: usize = 31 - 3 - 18 - 2; // = 8

/// Appended to a name trimmed to fit the advertising PDU, so the scan
/// list shows it was shortened. U+2026 is 3 bytes in UTF-8, and is
/// reserved out of [`MAX_ADV_NAME_BYTES`] when truncating.
const ADV_NAME_ELLIPSIS: &str = "…";

/// Pick the controller to serve on.
///
/// `selector` is either a controller address (`"DC:56:7B:1F:7D:EA"`) or
/// an interface name (`"hci1"`); `None` keeps the historical behaviour of
/// taking whichever adapter BlueZ lists first.
///
/// Prefer the address. `hciN` is an enumeration index, not an identity:
/// it tracks probe order, so unplugging a dongle, rebinding the driver,
/// or a boot that walks USB differently renumbers the adapters, and a
/// config pinned to `hci1` silently follows the number onto whichever
/// radio now holds it. BlueZ's `Alias` is no better — it defaults to the
/// hostname plus an order-derived suffix (`shepherd-26.04 #1`) and is
/// user-mutable — and `Modalias` on this hardware reports the same
/// generic `usb:v1D6Bp0246d0555` for every controller, so it can neither
/// identify nor distinguish them. The address is burned into the
/// controller and is the only stable discriminator BlueZ offers.
///
/// A selector that matches nothing is a hard error listing what *is*
/// present, because the alternative — quietly falling back to the first
/// adapter — is how you end up debugging the application while the
/// daemon serves a radio you never meant to use.
async fn resolve_adapter(
    session: &bluer::Session,
    selector: Option<&str>,
) -> anyhow::Result<bluer::Adapter> {
    let Some(selector) = selector else {
        return session.default_adapter().await.map_err(|e| {
            anyhow::anyhow!(
                "No default Bluetooth adapter ({e}); check `bluetoothctl show` and that an HCI controller is attached"
            )
        });
    };

    let names = session.adapter_names().await.map_err(|e| {
        anyhow::anyhow!("Could not list Bluetooth adapters ({e}); is bluetoothd running?")
    })?;

    // Interface name: exact match, no I/O needed.
    if names.iter().any(|n| n == selector) {
        return session
            .adapter(selector)
            .map_err(|e| anyhow::anyhow!("Adapter '{selector}' could not be opened ({e})"));
    }

    // Otherwise treat it as an address. Compare parsed, so formatting
    // and case in the config don't matter.
    let wanted: Option<bluer::Address> = selector.parse().ok();
    let mut available = Vec::new();
    for name in &names {
        let Ok(adapter) = session.adapter(name) else {
            continue;
        };
        let address = adapter.address().await.ok();
        if let (Some(wanted), Some(address)) = (wanted, address)
            && wanted == address
        {
            return Ok(adapter);
        }
        available.push(format!(
            "{name} ({})",
            address.map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
        ));
    }

    let hint = if wanted.is_none() {
        "\n  (not a controller address or `hciN` name — expected e.g. \"DC:56:7B:1F:7D:EA\")"
    } else {
        ""
    };
    anyhow::bail!(
        "Configured Bluetooth adapter '{selector}' is not present.{hint}\n  Available: {}\n  \
         Set [service.ble_management] adapter to one of the addresses above — \
         prefer the address over the hciN name, which changes with probe order.",
        if available.is_empty() {
            "(none)".to_string()
        } else {
            available.join(", ")
        },
    )
}

/// Trim `name` so it fits the advertising PDU beside the service UUID:
/// at most [`MAX_ADV_NAME_BYTES`] bytes, split on a UTF-8 code-point
/// boundary, with a trailing ellipsis when anything was dropped. Returns
/// the name unchanged (borrowed) when it already fits.
fn advertised_name(name: &str) -> Cow<'_, str> {
    if name.len() <= MAX_ADV_NAME_BYTES {
        return Cow::Borrowed(name);
    }
    // Leave room for the ellipsis, then back the cut up to a char boundary.
    let mut end = MAX_ADV_NAME_BYTES - ADV_NAME_ELLIPSIS.len();
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!("{}{ADV_NAME_ELLIPSIS}", &name[..end]))
}

#[derive(Debug, Clone)]
pub struct BleServerConfig {
    /// Advertised local name (also the device name in `DeviceInfo`).
    /// Defaults to the system hostname when constructed via `default`.
    pub device_name: String,
    /// Firmware version string surfaced via `DeviceInfo`. Typically the
    /// crate version of the daemon.
    pub firmware_version: String,
    /// Where to persist the admin record. Should match the daemon's
    /// state directory.
    pub admin_record_path: PathBuf,
    /// Sentinel file that, when present at startup, wipes the admin
    /// record and the BlueZ bond and returns to the unclaimed state.
    pub reset_sentinel_path: PathBuf,
    /// Which controller to serve on: an address (`"DC:56:7B:1F:7D:EA"`)
    /// or an interface name (`"hci1"`). `None` takes whichever BlueZ
    /// lists first. See [`resolve_adapter`].
    pub adapter: Option<String>,
}

impl BleServerConfig {
    /// Retry list for BlueZ bonds we still owe a removal, kept beside the
    /// admin record. Derived rather than configured: it belongs in the
    /// same state directory by definition and has no meaning apart from
    /// the record it shadows.
    fn pending_unbond_path(&self) -> PathBuf {
        self.admin_record_path
            .with_file_name("ble-pending-unbond.toml")
    }
}

pub struct BleServer {
    config: BleServerConfig,
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    display: Arc<dyn PairingDisplay>,
    /// Bonds BlueZ still owes us a removal for. Written to disk before
    /// any removal is attempted and drained in `run` once an adapter
    /// exists, so a failure — or a kill between clearing the record and
    /// removing the bond — is retried on the next startup instead of
    /// stranding the peer bonded to an unclaimed device.
    pending_unbond: PendingUnbondStore,
}

impl BleServer {
    /// Construct a server with state loaded from disk. Honors the
    /// reset sentinel before exposing anything over BLE: if it's
    /// present, the admin record is cleared (the BlueZ bond removal
    /// happens later, once an [`Adapter`] is available in `run`).
    pub fn new(
        config: BleServerConfig,
        svc: Arc<dyn ManagementService>,
        display: Arc<dyn PairingDisplay>,
    ) -> anyhow::Result<Self> {
        let store = AdminStore::new(config.admin_record_path.clone());
        let pending_unbond = PendingUnbondStore::new(config.pending_unbond_path());

        if check_reset_sentinel(&config.reset_sentinel_path) {
            warn!(
                sentinel = %config.reset_sentinel_path.display(),
                "Factory-reset sentinel present at startup; clearing admin record",
            );
            // Record the bond for removal *before* clearing the admin
            // record, and durably. Without the removal the phone stays
            // bonded while the device goes Unclaimed, so every reconnect
            // is accepted at the link layer and then rejected with
            // `not_claimed` — a lockout re-pairing can't clear, because
            // the bond already exists. Doing it in this order means a
            // crash between the two steps leaves a bond queued for
            // removal that never happened, which the next startup fixes;
            // the reverse order would lose the address entirely.
            match store.load() {
                Ok(Some(record)) => {
                    if let Err(e) = pending_unbond.add(&record.identity_address) {
                        warn!(
                            peer = %record.identity_address,
                            error = %e,
                            "Could not queue BlueZ bond removal after reset; the peer may stay bonded",
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => warn!(
                    error = %e,
                    "Could not read admin record before reset; BlueZ bond will not be removed",
                ),
            }
            store.clear()?;
        }

        let claim = Arc::new(ClaimMachine::load(store)?);
        Ok(Self {
            config,
            svc,
            claim,
            display,
            pending_unbond,
        })
    }

    /// Borrow the claim machine so other transports (e.g.
    /// `shepherd-http`) can consult the admin record for the unified
    /// bearer-token auth path. Returns a clone of the same `Arc` the
    /// server holds, so updates from BLE-side claim/factory_reset RPCs
    /// are visible to HTTP immediately.
    pub fn claim_machine(&self) -> Arc<ClaimMachine> {
        self.claim.clone()
    }

    /// Run the server until `shutdown_rx` flips to `true`. Sequence:
    ///
    /// 1. Open the default BlueZ adapter and bring it up + powered on.
    /// 2. If we just consumed the reset sentinel, ask BlueZ to forget
    ///    the previously-bonded admin (best-effort).
    /// 3. Register the Numeric Comparison pairing agent.
    /// 4. Publish the GATT application (Shepherd Management Service +
    ///    DeviceInfo / Request / Response / Events characteristics).
    /// 5. Start LE advertising under the configured device name.
    /// 6. Spawn the events forwarder task.
    /// 7. Wait for shutdown.
    pub async fn run(self, mut shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<()> {
        let session = bluer::Session::new().await.map_err(|e| {
            anyhow::anyhow!(
                "BlueZ D-Bus session unavailable ({e}); is bluetoothd running and reachable on the system bus?"
            )
        })?;
        let adapter = resolve_adapter(&session, self.config.adapter.as_deref()).await?;
        // Resolve to an owned String first: holding the `.await`'s
        // temporaries inside the macro's `Arguments` makes the future
        // non-Send, and this runs inside a spawned task.
        let adapter_address = adapter
            .address()
            .await
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        info!(
            adapter = %adapter.name(),
            address = %adapter_address,
            selector = self.config.adapter.as_deref().unwrap_or("<first listed>"),
            "Selected Bluetooth controller",
        );
        adapter.set_powered(true).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to power on Bluetooth adapter '{}' ({e}); this usually means the daemon user lacks the `bluetooth` group (BlueZ polkit requires it for Adapter1.Set*)",
                adapter.name(),
            )
        })?;
        adapter.set_pairable(true).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to mark adapter '{}' pairable ({e}); same root cause as set_powered — confirm the daemon user is in the `bluetooth` group",
                adapter.name(),
            )
        })?;
        info!(adapter = %adapter.name(), "BLE management server starting");

        // Settle any bond removals we still owe — from this boot's
        // sentinel reset, or from an earlier one whose removal didn't
        // stick. Entries survive until BlueZ confirms the peer is gone.
        drain_pending_unbonds(&adapter, &self.pending_unbond).await;

        let mut agent_handle = register_agent_with_retry(&session, &self.display).await;

        let response_outbox = Arc::new(Outbox::new("response", RESPONSE_OUTBOX_BYTES));
        let events_outbox = Arc::new(Outbox::new("events", EVENTS_OUTBOX_BYTES));

        // Request-side reassembly state, shared with the disconnect
        // monitor so a peer drop can wipe any half-written frame (see
        // `disconnect_monitor`). v1 holds a single reader because we
        // only expect one admin connection at a time.
        let reader: Arc<Mutex<FrameReader>> =
            Arc::new(Mutex::new(FrameReader::new(MAX_FRAME_BYTES)));
        let last_peer: Arc<Mutex<Option<PeerIdentity>>> = Arc::new(Mutex::new(None));

        // A successful `factory_reset` RPC must also forget the BlueZ
        // bond, or the same lockout as the sentinel path results. The RPC
        // handler runs deep in the GATT write path with no adapter access,
        // so it hands the peer address to this task, which owns the
        // adapter and calls `remove_device`.
        let (unbond_tx, mut unbond_rx) = mpsc::channel::<Address>(4);

        // Whether `PreferredBearer=bredr` took, and whether any peer has
        // completed real work on this boot. Together they gate the
        // fallback eviction in `spawn_first_rpc_watchdog`.
        let bearer_pinned = Arc::new(BearerPin::default());
        let had_session = Arc::new(AtomicBool::new(false));

        let mut on_air = go_on_air(
            &adapter,
            &self.config,
            &self.svc,
            &self.claim,
            &reader,
            &last_peer,
            &unbond_tx,
            &response_outbox,
            &events_outbox,
            &had_session,
        )
        .await?;

        let events_task = tokio::spawn(events_forwarder(self.svc.clone(), events_outbox.clone()));

        // Wipe stale transport bytes when a peer drops, so a companion
        // that resumes the same connection across a transient BLE
        // reconnect (no fresh `id == 1` sentinel) doesn't inherit a
        // desynced byte stream.
        // Distinguishes one peer connection from the next, so a
        // watchdog armed for a link that has since dropped cannot act on
        // whatever connection happens to be live when its timer expires.
        let connection_epoch = Arc::new(AtomicU64::new(0));
        let disconnect_task = tokio::spawn(disconnect_monitor(
            adapter.clone(),
            connection_epoch,
            bearer_pinned,
            had_session.clone(),
            reader.clone(),
            last_peer.clone(),
            response_outbox.clone(),
            events_outbox.clone(),
        ));

        // Drain factory-reset unbond requests for the server's lifetime.
        // Same durability contract as the startup path: record the debt
        // before attempting it, clear it only on success, so a failure
        // here is retried at next startup rather than stranding the peer
        // bonded to a device that has already forgotten it.
        let unbond_adapter = adapter.clone();
        let unbond_store = self.pending_unbond.clone();
        let unbond_task = tokio::spawn(async move {
            while let Some(addr) = unbond_rx.recv().await {
                let text = addr.to_string();
                if let Err(e) = unbond_store.add(&text) {
                    warn!(peer = %addr, error = %e, "Could not persist the pending unbond");
                }
                match unbond_adapter.remove_device(addr).await {
                    Ok(()) => {
                        info!(peer = %addr, "Removed BlueZ bond after factory_reset");
                        if let Err(e) = unbond_store.remove(&text) {
                            warn!(peer = %addr, error = %e, "Bond removed but the retry entry stayed");
                        }
                    }
                    Err(e) => warn!(
                        peer = %addr,
                        error = %e,
                        "BlueZ bond removal after factory_reset failed; queued for retry at next startup",
                    ),
                }
            }
        });

        // Hold the air until shutdown — and put it back whenever
        // something underneath us could have taken it off. See
        // [`go_on_air`] for why nothing else notices when that happens.
        //
        // Two triggers, because one is not enough. A controller power
        // cycle announces itself as `Powered(true)`. A *suspend* does
        // not: on the reporter's box a resume produced only bluetoothd's
        // "Controller resume with wake event 0x0" and our own peer
        // disconnect, with no property transition anywhere — and the
        // device was off air afterwards, the phone's every connect
        // ending in `status=147` (connection timeout) with our address
        // in zero scan results. So we also re-arm on the daemon's own
        // `SystemResumed`, which reaches us through the same event
        // stream the outbox forwarder already consumes.
        let mut adapter_events = match adapter.events().await {
            Ok(events) => Some(events),
            Err(e) => {
                warn!(
                    error = %e,
                    "Could not watch the adapter for power cycles; BLE management will still \
                     re-arm on resume, but not on a bare controller power cycle",
                );
                None
            }
        };
        let mut service_events = self.svc.subscribe_events();
        loop {
            if *shutdown_rx.borrow() {
                break;
            }
            // `None` = nothing to do; `Some(reason)` = go back on air.
            let reason: Option<&'static str> = tokio::select! {
                _ = shutdown_rx.changed() => None,
                event = async {
                    match adapter_events.as_mut() {
                        Some(events) => events.next().await,
                        // No adapter stream: park forever and let the
                        // other branches drive the loop.
                        None => std::future::pending().await,
                    }
                } => match event {
                    Some(event) => rearm_reason_for_adapter_event(&event),
                    None => {
                        warn!("Adapter event stream ended; no longer watching for power cycles");
                        adapter_events = None;
                        None
                    }
                },
                event = service_events.recv() => match event {
                    Ok(event) => rearm_reason_for_service_event(&event.payload),
                    // Lagging only means we may have missed events; the
                    // next resume still arrives.
                    Err(broadcast::error::RecvError::Lagged(_)) => None,
                    Err(broadcast::error::RecvError::Closed) => {
                        warn!("Service event stream closed; no longer watching for resume");
                        let _ = shutdown_rx.wait_for(|v| *v).await;
                        break;
                    }
                },
            };
            let Some(reason) = reason else { continue };
            warn!(
                adapter = %adapter.name(),
                reason,
                "Re-registering the GATT application and advertisement",
            );
            // A startup without an agent is survivable but degraded, so
            // take every re-arm as another chance at one.
            if agent_handle.is_none() {
                agent_handle = register_agent_with_retry(&session, &self.display).await;
            }
            // Drop first: the stale handles still own their D-Bus object
            // paths, and BlueZ rejects a second registration under a path
            // it already holds.
            drop(on_air);
            match go_on_air(
                &adapter,
                &self.config,
                &self.svc,
                &self.claim,
                &reader,
                &last_peer,
                &unbond_tx,
                &response_outbox,
                &events_outbox,
                &had_session,
            )
            .await
            {
                Ok(fresh) => on_air = fresh,
                Err(e) => {
                    error!(
                        error = %e,
                        reason,
                        "Failed to re-register BLE management; the device is off air until \
                         the session restarts",
                    );
                    let _ = shutdown_rx.wait_for(|v| *v).await;
                    break;
                }
            }
        }
        info!("BLE management server shutting down");
        events_task.abort();
        let _ = events_task.await;
        disconnect_task.abort();
        let _ = disconnect_task.await;
        unbond_task.abort();
        let _ = unbond_task.await;
        Ok(())
    }
}

/// Forward every event from [`ManagementService::subscribe_events`]
/// into the events outbox for the entire server lifetime. Lagged
/// events are logged and skipped — the companion re-syncs current
/// state via `service_state` on reconnect, so a missed event is
/// recoverable.
///
/// `StateChanged` is pushed *coalesced*: it carries a full snapshot, so
/// a newer one makes every queued older one redundant. This runs whether
/// or not a companion is connected, and without coalescing an idle
/// daemon under activity churn pins the outbox at capacity with hundreds
/// of superseded snapshots — which the next companion to connect then
/// has to drain at 512 bytes per round trip before it can send its first
/// RPC.
async fn events_forwarder(svc: Arc<dyn ManagementService>, outbox: Arc<Outbox>) {
    // The outer `run` aborts this task on shutdown via JoinHandle::abort,
    // so we don't need an explicit shutdown signal here — keeping the
    // loop free of `watch::Ref` (which isn't Send) keeps tokio::spawn
    // happy.
    let mut rx = svc.subscribe_events();
    loop {
        match rx.recv().await {
            Ok(event) => {
                let coalesce_key = coalesce_key_for(&event.payload);
                match serde_json::to_vec(&event) {
                    Ok(payload) => {
                        let framed = encode_frame(&payload);
                        match coalesce_key {
                            Some(key) => outbox.push_coalesced(framed, key).await,
                            None => outbox.push(framed).await,
                        }
                    }
                    Err(e) => warn!(error = %e, "Failed to serialize Event"),
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                warn!(skipped = n, "BLE events forwarder lagged");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                debug!("Event broadcast channel closed");
                return;
            }
        }
    }
}

/// Work through the pending-unbond list, dropping entries only once the
/// peer is provably gone from BlueZ.
///
/// A peer BlueZ has already forgotten counts as done — `remove_device`
/// errors on an unknown address, and treating that as a failure would
/// keep the entry (and the warning) forever.
async fn drain_pending_unbonds(adapter: &bluer::Adapter, store: &PendingUnbondStore) {
    let queued = match store.list() {
        Ok(q) if q.is_empty() => return,
        Ok(q) => q,
        Err(e) => {
            warn!(error = %e, "Could not read the pending-unbond list; bonds may linger");
            return;
        }
    };

    let known: HashSet<Address> = match adapter.device_addresses().await {
        Ok(addrs) => addrs.into_iter().collect(),
        Err(e) => {
            warn!(error = %e, "Could not enumerate BlueZ devices; deferring pending unbonds");
            return;
        }
    };

    for text in queued {
        let addr = match text.parse::<Address>() {
            Ok(a) => a,
            Err(e) => {
                // Unparseable entries can never succeed; drop them rather
                // than warn on every boot forever.
                warn!(address = %text, error = %e, "Discarding unparseable pending-unbond entry");
                let _ = store.remove(&text);
                continue;
            }
        };
        if !known.contains(&addr) {
            info!(peer = %addr, "Pending unbond already settled; BlueZ does not know this peer");
            let _ = store.remove(&text);
            continue;
        }
        match adapter.remove_device(addr).await {
            Ok(()) => {
                info!(peer = %addr, "Removed BlueZ bond");
                if let Err(e) = store.remove(&text) {
                    warn!(peer = %addr, error = %e, "Bond removed but the retry entry stayed");
                }
            }
            Err(e) => warn!(
                peer = %addr,
                error = %e,
                "BlueZ bond removal failed; queued for retry at next startup",
            ),
        }
    }
}

/// The outbox key an event supersedes its predecessors under, if any.
///
/// Only whole-state snapshots qualify: a newer `StateChanged` makes every
/// queued older one redundant, so collapsing them costs nothing and keeps
/// the events outbox — and therefore the companion's connect-time drain —
/// small. Everything else is an incremental fact (a warning fired, a
/// session ended) that the companion needs in order, so it queues
/// normally.
fn coalesce_key_for(payload: &EventPayload) -> Option<CoalesceKey> {
    match payload {
        EventPayload::StateChanged(_) => Some(COALESCE_STATE_CHANGED),
        _ => None,
    }
}

/// Watch the adapter for BLE peer disconnects and reset the transport
/// session state (both read-poll outboxes plus the Request-side frame
/// reader) whenever one drops.
///
/// The outboxes and the reassembly reader carry per-session byte
/// streams. The only *other* reset is the client's `id == 1` sentinel
/// in [`dispatch_frame`], which a companion sends only when it builds a
/// *fresh* [`ShepherdConnection`]. On a transient BLE drop (BT toggle,
/// brief out-of-range) the companion keeps the same connection and
/// resumes its RPC id counter mid-sequence, so `id == 1` never fires —
/// any bytes left over from before the drop (an unread response, a
/// half-written request frame, events that piled up while nothing was
/// polling) would then desync the reassembler on the reused link. This
/// monitor closes that gap by wiping on the disconnect event itself.
///
/// v1 assumes a single admin connection at a time, so clearing on *any*
/// peer disconnect is safe: there is never a second live session whose
/// in-flight bytes we'd disturb. The `Connected(false)` event fires at
/// drop time, well before the reconnect + next write, so the wipe can't
/// race a fresh response into oblivion.
#[allow(clippy::too_many_arguments)]
async fn disconnect_monitor(
    adapter: bluer::Adapter,
    epoch: Arc<AtomicU64>,
    bearer_pinned: Arc<BearerPin>,
    had_session: Arc<AtomicBool>,
    reader: Arc<Mutex<FrameReader>>,
    last_peer: Arc<Mutex<Option<PeerIdentity>>>,
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
) {
    let mut events = match adapter.events().await {
        Ok(e) => e,
        Err(e) => {
            warn!(
                error = %e,
                "BLE disconnect monitor could not subscribe to adapter events; \
                 stale-byte cleanup on transient reconnect is disabled",
            );
            return;
        }
    };

    // Devices BlueZ already knows about at startup — most importantly the
    // bonded admin, which persists across daemon restarts as a known
    // device and therefore won't arrive as a later `DeviceAdded`.
    let mut watched: HashSet<Address> = HashSet::new();
    match adapter.device_addresses().await {
        Ok(addrs) => {
            for addr in addrs {
                if watched.insert(addr) {
                    // Pin bonded peers here as well as on connect. A peer
                    // that is already connected when we start — the usual
                    // case after a daemon restart, since the ACL outlives
                    // it — never produces a `Connected(true)` transition,
                    // so the on-connect path alone would leave the admin
                    // phone unpinned for the whole session.
                    if let Ok(device) = adapter.device(addr)
                        && device.is_paired().await.unwrap_or(false)
                    {
                        pin_peer_to_bredr(&device, addr, &bearer_pinned).await;
                    }
                    spawn_device_watcher(
                        &adapter,
                        addr,
                        epoch.clone(),
                        bearer_pinned.clone(),
                        had_session.clone(),
                        reader.clone(),
                        last_peer.clone(),
                        response_outbox.clone(),
                        events_outbox.clone(),
                    );
                }
            }
        }
        Err(e) => debug!(error = %e, "could not enumerate known devices for disconnect watch"),
    }

    while let Some(ev) = events.next().await {
        match ev {
            AdapterEvent::DeviceAdded(addr) => {
                if watched.insert(addr) {
                    spawn_device_watcher(
                        &adapter,
                        addr,
                        epoch.clone(),
                        bearer_pinned.clone(),
                        had_session.clone(),
                        reader.clone(),
                        last_peer.clone(),
                        response_outbox.clone(),
                        events_outbox.clone(),
                    );
                }
            }
            // A removed device may later be re-added; drop it from the set
            // so we re-attach a watcher if that happens.
            AdapterEvent::DeviceRemoved(addr) => {
                watched.remove(&addr);
            }
            AdapterEvent::PropertyChanged(_) => {}
        }
    }
}

/// Spawn a task that clears the transport session state each time
/// `addr` transitions to disconnected. The task lives until the device
/// object is removed from BlueZ (its event stream ends), which spans
/// many connect/disconnect cycles for a bonded peer.
#[allow(clippy::too_many_arguments)]
fn spawn_device_watcher(
    adapter: &bluer::Adapter,
    addr: Address,
    epoch: Arc<AtomicU64>,
    bearer_pinned: Arc<BearerPin>,
    had_session: Arc<AtomicBool>,
    reader: Arc<Mutex<FrameReader>>,
    last_peer: Arc<Mutex<Option<PeerIdentity>>>,
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
) {
    let device = match adapter.device(addr) {
        Ok(d) => d,
        Err(e) => {
            debug!(peer = %addr, error = %e, "could not open device for disconnect watch");
            return;
        }
    };
    tokio::spawn(async move {
        let mut events = match device.events().await {
            Ok(e) => e,
            Err(e) => {
                debug!(peer = %addr, error = %e, "device event stream unavailable");
                return;
            }
        };
        while let Some(DeviceEvent::PropertyChanged(prop)) = events.next().await {
            match prop {
                // A companion drains both outboxes before it will send
                // its first RPC, so the depth logged here *is* the
                // connect latency it's about to pay. Without this line a
                // stalled drain is entirely invisible: the peer connects
                // (no log), reads thousands of times (debug only), and
                // never writes, so the journal shows nothing at all
                // between "advertising started" and an RPC that may
                // arrive minutes later or not at all.
                DeviceProperty::Connected(true) => {
                    let (response_frames, response_bytes) = response_outbox.depth().await;
                    let (events_frames, events_bytes) = events_outbox.depth().await;
                    info!(
                        peer = %addr,
                        response_frames,
                        response_bytes,
                        events_frames,
                        events_bytes,
                        "BLE peer connected; companion will drain these before its first RPC",
                    );
                    let generation = epoch.fetch_add(1, Ordering::Relaxed) + 1;
                    // Do this on every connect, not just the first: it is
                    // idempotent in BlueZ, and the arming we are undoing
                    // happens on service probe, which can recur.
                    if device.is_paired().await.unwrap_or(false) {
                        pin_peer_to_bredr(&device, addr, &bearer_pinned).await;
                    }
                    spawn_first_rpc_watchdog(
                        device.clone(),
                        addr,
                        generation,
                        epoch.clone(),
                        last_peer.clone(),
                        bearer_pinned.clone(),
                        had_session.clone(),
                    );
                }
                DeviceProperty::Connected(false) => {
                    epoch.fetch_add(1, Ordering::Relaxed);
                    info!(peer = %addr, "BLE peer disconnected; clearing transport session state");
                    reset_transport_session(&reader, &last_peer, &response_outbox, &events_outbox)
                        .await;
                }
                _ => {}
            }
        }
    });
}

/// How long a peer may hold a link without sending a single RPC before
/// we say so in the log.
///
/// Sized above the worst healthy first-RPC latency, not near it: the
/// companion drains both outboxes and lets the link encryption settle
/// (up to three 5 s attempts) before its first write, and the slowest
/// healthy connect measured on hardware — host loaded, cold app start —
/// was 8 s. Note this is *not* long enough to clear a pairing, which
/// waits on a human comparing six digits; that is one of the reasons
/// this no longer acts on the link, only reports it.
const FIRST_RPC_GRACE: Duration = Duration::from_secs(25);

/// How long before the *fallback* eviction acts (see
/// [`spawn_first_rpc_watchdog`]). Far beyond the 30s the OS allows for a
/// Numeric Comparison, so a pairing that is merely slow is never caught
/// by it — the last time this path fired at 25s it hung up on the
/// comparison window and made pairing impossible.
const EVICT_GRACE: Duration = Duration::from_secs(90);

/// Pin a bonded peer to the BR/EDR bearer so BlueZ stops arming the
/// kernel to dial it over LE.
///
/// This is the fix for the week's worst failure, and it is aimed at the
/// mechanism rather than the symptom. Probing an `auto_connect` profile
/// on a bonded device — `a2dp`, `bap`, `hog`, `input`, all of which a
/// phone matches — makes bluetoothd call `device_set_auto_connect(TRUE)`
/// (`src/device.c:5551`), which sends `MGMT_OP_ADD_DEVICE` with
/// `action = 0x02`: *the kernel* then connects that address the moment it
/// advertises. Because our companion is a GATT server, a link the box
/// originates puts the box in the central role — and only a central may
/// start encryption, so the phone can never encrypt it and every read of
/// an `encrypt_authenticated` characteristic comes back
/// `Insufficient Authentication`, forever, on a link that never drops.
///
/// Setting `PreferredBearer = "bredr"` makes `device_set_auto_connect`
/// return before `adapter_auto_connect_add()` — BlueZ's own comment there
/// reads "Remove device from auto-connect list so the kernel does not
/// attempt to auto-connect to it in case it starts advertising". It is
/// stored in the bond record, and both the load and the inhibit are
/// ungated, so it survives restarts and keeps working even where the
/// property itself is hidden. BR/EDR auto-connect is untouched, which
/// matters on a box whose radio is also the user's headphones.
///
/// The property is `experimental` upstream, so on a stock bluetoothd it
/// does not exist and this fails — see the caller for what happens then.
/// bluer does not wrap it, hence the direct D-Bus call; it is blocking,
/// which is why it runs on the blocking pool.
async fn prefer_bredr_bearer(adapter_name: &str, addr: Address) -> Result<(), String> {
    let path = format!(
        "/org/bluez/{adapter_name}/dev_{}",
        addr.to_string().replace(':', "_")
    );
    tokio::task::spawn_blocking(move || {
        use dbus::blocking::Connection;
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;

        let conn = Connection::new_system().map_err(|e| e.to_string())?;
        let proxy = conn.with_proxy("org.bluez", path, Duration::from_secs(5));
        proxy
            .set("org.bluez.Device1", "PreferredBearer", "bredr".to_string())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Tracks the bearer pin across connections: whether it has taken, and
/// whether we have already told the operator how to make it possible.
///
/// Retrying every connect is worth it — the property becomes available
/// the moment bluetoothd is restarted with `Experimental`, and a
/// controller power cycle or resume already brings us back through here —
/// but repeating the same paragraph on every reconnect is not.
#[derive(Default)]
struct BearerPin {
    pinned: AtomicBool,
    warned: AtomicBool,
}

/// Apply [`prefer_bredr_bearer`] and say something useful either way.
///
/// Success is logged once (the flag also tells the watchdog it no longer
/// needs its fallback). Failure is almost always "the property is
/// experimental and this bluetoothd was started without it", which is not
/// something the daemon can fix for the operator — so say exactly what to
/// do about it, once, rather than repeating it on every reconnect.
async fn pin_peer_to_bredr(device: &bluer::Device, addr: Address, pin: &Arc<BearerPin>) {
    if pin.pinned.load(Ordering::Relaxed) {
        return;
    }
    match prefer_bredr_bearer(device.adapter_name(), addr).await {
        Ok(()) => {
            pin.pinned.store(true, Ordering::Relaxed);
            info!(
                peer = %addr,
                "Pinned peer to the BR/EDR bearer; the kernel will no longer auto-connect \
                 it over LE, so the companion keeps the central role and can encrypt",
            );
        }
        Err(e) if pin.warned.swap(true, Ordering::Relaxed) => debug!(
            error = %e,
            peer = %addr,
            "PreferredBearer=bredr still unavailable",
        ),
        Err(e) => warn!(
            error = %e,
            peer = %addr,
            "Could not set PreferredBearer=bredr. The device may dial this phone over LE \
             after any disconnect, taking the central role, after which the phone cannot \
             encrypt the link and every read fails. The property is experimental: enable \
             `Experimental = true` in /etc/bluetooth/main.conf, or add \
             `PreferredBearer=bredr` under [General] in the peer's file in \
             /var/lib/bluetooth/<adapter>/<peer>/info with bluetoothd stopped",
        ),
    }
}

/// Report a bonded peer that connects and then never talks.
///
/// The failure this exists for: if the *box* originates the LE
/// connection, it is the central and the phone is the peripheral — and
/// only a central may start encryption. The phone's stack decides it
/// needs to encrypt and then has no way to act on it, while nothing on
/// our side is asking BlueZ for a secure link either. The result is a
/// link that stays up forever with every read of an
/// `encrypt_authenticated` characteristic answered `Insufficient
/// Authentication (0x05)` and not one SMP frame on the air. Observed on
/// the reporter's box on 2026-08-16; see
/// `docs/ai/history/2026-08-16 001 ble-connect-fails-after-long-session.md`.
///
/// bluetoothd answers those reads itself, so our characteristic
/// callbacks never fire and the daemon cannot see the failure directly.
/// What it *can* see is the absence of any request write — `last_peer`
/// is set by the first one — which is what this reports.
///
/// **It only reports.** It used to drop the link, on the theory that the
/// companion would reconnect and, by initiating, become central. That
/// broke pairing outright: a box holding a stale bond reads as
/// `is_paired()` even while the phone is mid-`BOND_BONDING`, so the
/// timer fired 25 s into the Numeric Comparison window — before the
/// human could compare the digits — and hung up on the pairing
/// (`status=19 GATT_CONN_TERMINATE_PEER_USER` on the phone, three
/// attempts in a row on 2026-08-17). Silence on a fresh link is simply
/// not specific enough to act on: pairing, a slow drain and the
/// role-inversion deadlock all look identical from here. Eviction can
/// come back when the deadlock itself is reproducible and there is a
/// signal that distinguishes it — until then this line is what turns an
/// invisible failure into a diagnosable one.
fn spawn_first_rpc_watchdog(
    device: bluer::Device,
    addr: Address,
    generation: u64,
    epoch: Arc<AtomicU64>,
    last_peer: Arc<Mutex<Option<PeerIdentity>>>,
    bearer_pinned: Arc<BearerPin>,
    had_session: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_RPC_GRACE).await;
        if epoch.load(Ordering::Relaxed) != generation {
            // The link this timer was armed for is long gone; anything
            // live now is a *different* connection with its own timer.
            // Without this check the state below is read against the
            // wrong link: on 2026-08-17 a timer armed at 03:37:47 fired
            // at 03:38:12 against a healthy connection that had come up
            // 1.2s earlier and was about to send its first RPC.
            return;
        }
        if last_peer.lock().await.is_some() {
            return; // It talked to us. Nothing to do.
        }
        if !device.is_connected().await.unwrap_or(false) {
            return; // Already gone.
        }
        let paired = device.is_paired().await.unwrap_or(false);
        warn!(
            peer = %addr,
            paired,
            grace_secs = FIRST_RPC_GRACE.as_secs(),
            "Peer has held a link this long without sending an RPC. If it is bonded and \
             the companion is trying to reach us, the link is probably one the device \
             originated — the phone cannot encrypt those, and every encrypted read on \
             them fails. Pairing in progress looks the same and is fine",
        );

        // Fallback, for boxes where the bearer could not be pinned: take
        // the link away so the phone reconnects and, by initiating,
        // becomes central. Three gates, because this is the mechanism
        // that once hung up on a Numeric Comparison window:
        //
        // - only when the bearer is unpinned, i.e. the real fix is
        //   unavailable and the device really can dial this peer;
        // - only once a session has actually exchanged RPCs on this
        //   boot, which a first pairing never has;
        // - and only after a much longer wait than the report above,
        //   comfortably past the 30s the OS gives a human to compare six
        //   digits, so a slow pairing outlives it.
        if bearer_pinned.pinned.load(Ordering::Relaxed) || !had_session.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(EVICT_GRACE - FIRST_RPC_GRACE).await;
        if epoch.load(Ordering::Relaxed) != generation
            || last_peer.lock().await.is_some()
            || !device.is_connected().await.unwrap_or(false)
        {
            return;
        }
        warn!(
            peer = %addr,
            grace_secs = EVICT_GRACE.as_secs(),
            "Dropping the link: still silent, and the bearer could not be pinned. The \
             companion should reconnect and take the central role",
        );
        if let Err(e) = device.disconnect().await {
            warn!(peer = %addr, error = %e, "Could not drop the silent peer's link");
        }
    });
}

/// Drop every byte of per-session transport state: any half-assembled
/// request frame, the last-peer marker, and both read-poll outboxes.
async fn reset_transport_session(
    reader: &Arc<Mutex<FrameReader>>,
    last_peer: &Arc<Mutex<Option<PeerIdentity>>>,
    response_outbox: &Arc<Outbox>,
    events_outbox: &Arc<Outbox>,
) {
    *reader.lock().await = FrameReader::new(MAX_FRAME_BYTES);
    *last_peer.lock().await = None;
    response_outbox.clear().await;
    events_outbox.clear().await;
}

/// Why, if at all, an adapter event means the service needs to go back
/// on air. Split out from the run loop so the *decision* is testable
/// without an adapter.
fn rearm_reason_for_adapter_event(event: &AdapterEvent) -> Option<&'static str> {
    match event {
        AdapterEvent::PropertyChanged(AdapterProperty::Powered(true)) => {
            Some("the Bluetooth controller powered back on")
        }
        _ => None,
    }
}

/// Same, for the daemon's own event stream.
///
/// `SystemResumed` is here because a suspend/resume does **not** always
/// move `Adapter1.Powered`: on the 2026-08-17 report the resume produced
/// only bluetoothd's "Controller resume" line and our peer disconnect,
/// with no property transition at all — and the device was off air
/// afterwards. Watching the power property alone missed it entirely.
fn rearm_reason_for_service_event(payload: &EventPayload) -> Option<&'static str> {
    match payload {
        EventPayload::SystemResumed => Some("the system resumed from sleep"),
        _ => None,
    }
}

/// The two registrations that stop existing when the controller power
/// cycles: the GATT application and the LE advertisement. Both are
/// handles whose `Drop` unregisters them, so re-arming is "drop, then
/// register again".
struct OnAir {
    _app: ApplicationHandle,
    _adv: AdvertisementHandle,
}

/// Put the management service on air: serve the GATT application and
/// start advertising.
///
/// Called once at startup and again every time the controller is powered
/// back on. **Nothing else notices that transition.** A
/// `bluetoothctl power off/on`, an `rfkill` cycle, or a suspend that
/// resets the controller takes the advertisement off air while BlueZ goes
/// on reporting it as registered — `LEAdvertisingManager1.ActiveInstances`
/// still reads 1, and `Adapter1.Powered` reads true — so a device that has
/// silently vanished looks identical to a healthy one from every property
/// we can query. Verified on the dev box: after a power cycle the
/// companion could not connect at all, and the phone's pairing scan (which
/// filters on our service UUID) listed nothing, while the daemon sat there
/// believing it was advertising. The power-on signal is the only reliable
/// cue, which is why this is driven by an event rather than a poll.
#[allow(clippy::too_many_arguments)]
async fn go_on_air(
    adapter: &bluer::Adapter,
    config: &BleServerConfig,
    svc: &Arc<dyn ManagementService>,
    claim: &Arc<ClaimMachine>,
    reader: &Arc<Mutex<FrameReader>>,
    last_peer: &Arc<Mutex<Option<PeerIdentity>>>,
    unbond_tx: &mpsc::Sender<Address>,
    response_outbox: &Arc<Outbox>,
    events_outbox: &Arc<Outbox>,
    had_session: &Arc<AtomicBool>,
) -> bluer::Result<OnAir> {
    let application = build_application(
        config.clone(),
        svc.clone(),
        claim.clone(),
        reader.clone(),
        last_peer.clone(),
        unbond_tx.clone(),
        response_outbox.clone(),
        events_outbox.clone(),
        had_session.clone(),
    );
    let app: ApplicationHandle = adapter.serve_gatt_application(application).await?;

    // The service UUID must share the 31-byte legacy PDU with the
    // device name; a name that pushes the total over the limit makes
    // BlueZ reject the advertisement outright (0x0d), taking the UUID
    // off air with it. Trim the *advertised* name to fit — the full
    // name still reaches the companion over GATT.
    let adv_name = advertised_name(&config.device_name);
    if adv_name.len() != config.device_name.len() {
        warn!(
            device = %config.device_name,
            advertised = %adv_name,
            max_bytes = MAX_ADV_NAME_BYTES,
            "device name too long for the BLE advertising PDU; advertising a \
             truncated name so the management service UUID still fits",
        );
    }

    let adv: AdvertisementHandle = adapter
        .advertise(Advertisement {
            advertisement_type: bluer::adv::Type::Peripheral,
            service_uuids: [SHEPHERD_MANAGEMENT_SERVICE_UUID].into_iter().collect(),
            local_name: Some(adv_name.to_string()),
            discoverable: Some(true),
            ..Default::default()
        })
        .await?;
    info!(
        device = %config.device_name,
        advertised = %adv_name,
        service = %SHEPHERD_MANAGEMENT_SERVICE_UUID,
        "BLE management advertising started",
    );
    Ok(OnAir {
        _app: app,
        _adv: adv,
    })
}

/// Attempts at registering the pairing agent before giving up on it for
/// now, and the pause before each retry.
///
/// Short and few: a permission problem fails identically every time, and
/// the case worth riding out is a momentarily busy bluetoothd — whose
/// own D-Bus timeout can already make a single attempt take ~25 s.
const AGENT_REGISTER_BACKOFF: [Duration; 2] = [Duration::from_millis(500), Duration::from_secs(2)];

/// Register the Numeric Comparison agent, retrying briefly, and carry on
/// without one rather than taking the whole transport down.
///
/// This used to be fatal to `run`. On 2026-08-17 a single
/// `org.freedesktop.DBus.Error.NoReply` — bluetoothd busy for a moment
/// during startup — killed BLE management for the entire session: no
/// advertising, no retry, and one ERROR line as the only evidence, which
/// is indistinguishable from every other "the device just isn't there"
/// failure we spent this week chasing.
///
/// Losing the agent costs pairing (the TV overlay and the Numeric
/// Comparison flow); losing the server costs *everything*, including a
/// bonded admin phone that only wanted to reconnect to an
/// already-claimed device. The degraded state is the better one, and the
/// re-arm path retries later.
async fn register_agent_with_retry(
    session: &bluer::Session,
    display: &Arc<dyn PairingDisplay>,
) -> Option<AgentHandle> {
    for (attempt, backoff) in AGENT_REGISTER_BACKOFF
        .iter()
        .map(Some)
        .chain(std::iter::once(None))
        .enumerate()
    {
        match register_agent(session, display.clone()).await {
            Ok(handle) => {
                if attempt > 0 {
                    info!(attempt = attempt + 1, "BlueZ pairing agent registered");
                }
                return Some(handle);
            }
            Err(e) => match backoff {
                Some(delay) => {
                    warn!(
                        error = %e,
                        attempt = attempt + 1,
                        retry_in_ms = delay.as_millis() as u64,
                        "Could not register the BlueZ pairing agent; retrying",
                    );
                    tokio::time::sleep(*delay).await;
                }
                None => error!(
                    error = %e,
                    attempts = attempt + 1,
                    "Could not register the BlueZ pairing agent. BLE management stays up and \
                     an already-paired companion still works, but pairing a new phone will \
                     not show the Numeric Comparison code on the TV. If this persists, check \
                     that the daemon user is in the `bluetooth` group",
                ),
            },
        }
    }
    None
}

async fn register_agent(
    session: &bluer::Session,
    display: Arc<dyn PairingDisplay>,
) -> bluer::Result<AgentHandle> {
    let agent = build_agent(session.clone(), display);
    session.register_agent(agent).await
}

#[allow(clippy::too_many_arguments)]
fn build_application(
    config: BleServerConfig,
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    reader: Arc<Mutex<FrameReader>>,
    last_peer: Arc<Mutex<Option<PeerIdentity>>>,
    unbond_tx: mpsc::Sender<Address>,
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
    had_session: Arc<AtomicBool>,
) -> Application {
    Application {
        services: vec![Service {
            uuid: SHEPHERD_MANAGEMENT_SERVICE_UUID,
            primary: true,
            characteristics: vec![
                device_info_characteristic(config, claim.clone()),
                request_characteristic(
                    svc,
                    claim,
                    reader,
                    last_peer,
                    unbond_tx,
                    response_outbox.clone(),
                    events_outbox.clone(),
                    had_session,
                ),
                outbox_read_characteristic(
                    SHEPHERD_RESPONSE_CHAR_UUID,
                    response_outbox,
                    "Response",
                ),
                outbox_read_characteristic(SHEPHERD_EVENTS_CHAR_UUID, events_outbox, "Events"),
            ],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// `DeviceInfo`: GATT read returning the claim state, protocol version,
/// firmware version, and device name. Readable without pairing so the
/// companion app can render an "unclaimed device — tap to set up" UI
/// before initiating the bond.
fn device_info_characteristic(config: BleServerConfig, claim: Arc<ClaimMachine>) -> Characteristic {
    Characteristic {
        uuid: SHEPHERD_DEVICE_INFO_CHAR_UUID,
        read: Some(CharacteristicRead {
            read: true,
            fun: Box::new(move |_req| {
                let claim = claim.clone();
                let config = config.clone();
                async move {
                    let claim_state = if claim.is_claimed() {
                        ClaimStateTag::Claimed
                    } else {
                        ClaimStateTag::Unclaimed
                    };
                    let info = DeviceInfo {
                        protocol_version: PROTOCOL_VERSION,
                        firmware_version: config.firmware_version.clone(),
                        claim_state,
                        device_name: config.device_name.clone(),
                    };
                    Ok(serde_json::to_vec(&info).unwrap_or_default())
                }
                .boxed()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `Request`: client writes length-prefixed JSON-RPC frames.
/// Authenticated-encrypted link required (i.e. MITM-protected bonding).
///
/// We intentionally do **not** require `secure_write` (LE Secure
/// Connections). LESC needs a BT 4.2+ controller, and the leibniz
/// dev box is BT 4.0. With `secure_write: true` BlueZ silently
/// rejects all writes from peers bonded via LE Legacy Pairing, even
/// though the link is MITM-protected via Passkey Entry. The
/// `encrypt_authenticated_write` flag is satisfied by Legacy MITM
/// pairing too — that gives us the property we actually want
/// (encrypted + authenticated link) without requiring LESC.
#[allow(clippy::too_many_arguments)]
fn request_characteristic(
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    reader: Arc<Mutex<FrameReader>>,
    last_peer: Arc<Mutex<Option<PeerIdentity>>>,
    unbond_tx: mpsc::Sender<Address>,
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
    had_session: Arc<AtomicBool>,
) -> Characteristic {
    // `reader`/`last_peer` are the single-connection frame reassembly
    // state (v1 expects one admin connection at a time). They're created
    // in `run` and shared with `disconnect_monitor` so a peer drop can
    // reset a half-written frame; if a second device writes here we wipe
    // the buffer and start fresh.
    Characteristic {
        uuid: SHEPHERD_REQUEST_CHAR_UUID,
        write: Some(CharacteristicWrite {
            write: true,
            write_without_response: true,
            encrypt_authenticated_write: true,
            method: CharacteristicWriteMethod::Fun(Box::new(move |chunk, req| {
                let svc = svc.clone();
                let claim = claim.clone();
                let reader = reader.clone();
                let last_peer = last_peer.clone();
                let unbond_tx = unbond_tx.clone();
                let response_outbox = response_outbox.clone();
                let events_outbox = events_outbox.clone();
                // A write landed on an encrypt-authenticated
                // characteristic, so this link demonstrably works. That is
                // what the fallback eviction waits to see before it will
                // ever act on a silent one.
                had_session.store(true, Ordering::Relaxed);
                async move {
                    let peer = PeerIdentity {
                        address: req.device_address.to_string(),
                        // bluer doesn't expose address type on the
                        // write request; the value the admin record
                        // holds matches what BlueZ stores, which we
                        // synthesize at claim time. v1 single-admin
                        // ignores this field on the inbound side.
                        address_type: "public".to_string(),
                    };
                    handle_write(
                        &peer,
                        chunk,
                        reader,
                        last_peer,
                        claim,
                        svc,
                        &unbond_tx,
                        response_outbox,
                        events_outbox,
                    )
                    .await
                }
                .boxed()
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `Response` / `Events`: read-poll characteristic backed by an
/// [`Outbox`]. Each read returns up to ATT_MTU-1 bytes from the head
/// of the outbox, in order. Empty result means the queue is drained.
///
/// `encrypt_authenticated_read` mirrors the Request characteristic's
/// write gate: the outbox carries RPC responses (which may include
/// admin tokens) and live state events, both of which must only flow
/// over the bonded admin link.
fn outbox_read_characteristic(
    uuid: bluer::Uuid,
    outbox: Arc<Outbox>,
    label: &'static str,
) -> Characteristic {
    Characteristic {
        uuid,
        read: Some(CharacteristicRead {
            read: true,
            encrypt_authenticated_read: true,
            fun: Box::new(move |req| {
                let outbox = outbox.clone();
                async move {
                    // Blob reads (offset > 0) shouldn't happen for our
                    // bounded responses, but if BlueZ ever issues one
                    // we'd return stale head bytes — answer empty so
                    // the client treats the read as complete and polls
                    // again for a fresh drain.
                    if req.offset != 0 {
                        debug!(
                            label,
                            offset = req.offset,
                            "BLE outbox read at non-zero offset (long-read continuation); returning empty"
                        );
                        return Ok(Vec::new());
                    }
                    // Cap at the GATT max-attribute-value of 512
                    // bytes regardless of negotiated MTU. With MTU 517
                    // an ATT read response can technically carry 516
                    // bytes, but Android's stack enforces the spec's
                    // 512-byte ceiling on a single GATT attribute
                    // value and silently truncates anything larger.
                    // Returning 516 here causes the phone to receive
                    // 512 per chunk, lose 4 bytes per chunk, and end
                    // up with a corrupt frame stitched together with
                    // bytes from the *next* response.
                    let max_chunk = (req.mtu as usize)
                        .saturating_sub(1)
                        .clamp(20, GATT_MAX_ATTR_VALUE);
                    let bytes = outbox.read(max_chunk).await;
                    if !bytes.is_empty() {
                        debug!(
                            label,
                            mtu = req.mtu,
                            peer = %req.device_address,
                            len = bytes.len(),
                            "BLE outbox read"
                        );
                    }
                    Ok(bytes)
                }
                .boxed()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_write(
    peer: &PeerIdentity,
    chunk: Vec<u8>,
    reader: Arc<Mutex<FrameReader>>,
    last_peer: Arc<Mutex<Option<PeerIdentity>>>,
    claim: Arc<ClaimMachine>,
    svc: Arc<dyn ManagementService>,
    unbond_tx: &mpsc::Sender<Address>,
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
) -> bluer::gatt::local::ReqResult<()> {
    debug!(
        peer = %peer.address,
        chunk_len = chunk.len(),
        "BLE request chunk arrived"
    );

    // Reset the reader if the peer changed mid-flight — keeps a stuck
    // half-frame from one client from polluting the next client's
    // first request.
    {
        let mut lp = last_peer.lock().await;
        if lp.as_ref() != Some(peer) {
            *lp = Some(peer.clone());
            *reader.lock().await = FrameReader::new(MAX_FRAME_BYTES);
        }
    }

    {
        let mut r = reader.lock().await;
        r.push(&chunk);
        loop {
            match r.pop_frame() {
                Ok(Some(frame)) => {
                    drop(r);
                    dispatch_frame(
                        peer,
                        &frame,
                        &claim,
                        &svc,
                        unbond_tx,
                        &response_outbox,
                        &events_outbox,
                    )
                    .await;
                    r = reader.lock().await;
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "Dropping connection state after framing error");
                    *r = FrameReader::new(MAX_FRAME_BYTES);
                    break;
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_frame(
    peer: &PeerIdentity,
    frame: &[u8],
    claim: &Arc<ClaimMachine>,
    svc: &Arc<dyn ManagementService>,
    unbond_tx: &mpsc::Sender<Address>,
    response_outbox: &Arc<Outbox>,
    events_outbox: &Arc<Outbox>,
) {
    let request: RpcRequest = match serde_json::from_slice(frame) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                peer = %peer.address,
                frame_len = frame.len(),
                error = %e,
                "Dropping BLE frame: not valid JSON-RPC"
            );
            let resp = RpcResponse::err(0, ErrorCode::ParseError, e.to_string());
            push_response(response_outbox, &resp).await;
            return;
        }
    };

    let id = request.id;

    // Every `ShepherdConnection` on the companion starts its RPC id
    // counter at 1, so `id == 1` is a deterministic signal that this
    // write is the first of a fresh BLE session. Wipe the outboxes
    // before queueing the response — any bytes still in the queue
    // are stale (an unread response from the previous session or
    // events that accumulated while no one was polling) and would
    // get mixed into the byte stream the client's reassembler is
    // about to read. The earlier write-gap heuristic was wrong: a
    // 15-second per-RPC timeout naturally produces inter-write gaps
    // exceeding any reasonable threshold, so it kept wiping the
    // queue mid-delivery on a single legitimate session.
    if id == 1 {
        info!(peer = %peer.address, "RPC id=1; clearing outboxes for new session");
        response_outbox.clear().await;
        events_outbox.clear().await;
    }

    info!(
        peer = %peer.address,
        id, method = %request.method,
        "BLE RPC received"
    );
    let response = match request.method.as_str() {
        "claim" => handle_claim_rpc(id, request.params, peer, claim).await,
        "factory_reset" => handle_factory_reset_rpc(id, peer, claim, unbond_tx).await,
        _ => match claim.authorize(peer) {
            AuthDecision::Allow => dispatch_management(svc.as_ref(), request).await,
            AuthDecision::Deny { reason } => {
                let code = if claim.is_claimed() {
                    ErrorCode::PermissionDenied
                } else {
                    ErrorCode::NotClaimed
                };
                RpcResponse::err(id, code, reason)
            }
        },
    };
    info!(
        peer = %peer.address,
        id,
        ok = response.error.is_none(),
        "BLE RPC response queued"
    );
    push_response(response_outbox, &response).await;
}

async fn handle_claim_rpc(
    id: u32,
    params: serde_json::Value,
    peer: &PeerIdentity,
    claim: &Arc<ClaimMachine>,
) -> RpcResponse {
    #[derive(serde::Deserialize)]
    struct ClaimParams {
        device_name: String,
    }
    let parsed: ClaimParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return RpcResponse::err(id, ErrorCode::InvalidParams, e.to_string()),
    };
    match claim.claim(peer.clone(), parsed.device_name) {
        Ok(record) => match serde_json::to_value(&record) {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => RpcResponse::err(id, ErrorCode::Internal, e.to_string()),
        },
        Err(crate::claim::ClaimError::AlreadyClaimed) => {
            RpcResponse::err(id, ErrorCode::AlreadyClaimed, "device already claimed")
        }
        Err(e) => RpcResponse::err(id, ErrorCode::Internal, e.to_string()),
    }
}

async fn handle_factory_reset_rpc(
    id: u32,
    peer: &PeerIdentity,
    claim: &Arc<ClaimMachine>,
    unbond_tx: &mpsc::Sender<Address>,
) -> RpcResponse {
    // factory_reset is admin-gated: only the current admin (or no admin,
    // in which case it's a no-op) may invoke it.
    match claim.authorize(peer) {
        AuthDecision::Allow => match claim.factory_reset() {
            // On a real reset (there was an admin), tell the unbond task
            // to forget the BlueZ bond too — otherwise the peer stays
            // bonded while the device is Unclaimed and every reconnect is
            // link-accepted then rejected with `not_claimed`. The response
            // is queued before this fires, but removing the device
            // disconnects the peer, so delivery of the ok is best-effort —
            // acceptable, since a factory reset ends the session anyway.
            Ok(previous) => {
                if let Some(record) = previous {
                    request_unbond(unbond_tx, &record.identity_address).await;
                }
                RpcResponse::ok(id, serde_json::Value::Null)
            }
            Err(e) => RpcResponse::err(id, ErrorCode::Internal, e.to_string()),
        },
        AuthDecision::Deny { reason } => {
            // If unclaimed, factory_reset is a no-op success; otherwise
            // deny the non-admin peer.
            if !claim.is_claimed() {
                RpcResponse::ok(id, serde_json::Value::Null)
            } else {
                RpcResponse::err(id, ErrorCode::PermissionDenied, reason)
            }
        }
    }
}

/// Ask the unbond task (which owns the adapter) to remove the BlueZ bond
/// for `identity_address`. Best-effort: a parse failure or a closed
/// channel is logged, not surfaced to the caller, since the admin record
/// is already cleared and the reset itself succeeded.
async fn request_unbond(unbond_tx: &mpsc::Sender<Address>, identity_address: &str) {
    match identity_address.parse::<Address>() {
        Ok(addr) => {
            if unbond_tx.send(addr).await.is_err() {
                warn!(
                    peer = %addr,
                    "unbond channel closed; BlueZ bond not removed after factory_reset",
                );
            }
        }
        Err(e) => warn!(
            address = %identity_address,
            error = %e,
            "Could not parse admin identity address; BlueZ bond not removed after factory_reset",
        ),
    }
}

async fn push_response(outbox: &Arc<Outbox>, resp: &RpcResponse) {
    let bytes = match serde_json::to_vec(resp) {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "Failed to serialize RpcResponse");
            return;
        }
    };
    outbox.push(encode_frame(&bytes)).await;
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the two write-path invariants the read-poll
    //! transport depends on (see
    //! `docs/ai/history/2026-06-28 001 ble-read-poll-replaces-notify.md`):
    //! the `id == 1` session-boundary outbox wipe, and the `FrameReader`
    //! reset on peer change / framing error. Both took live-hardware
    //! debugging to land; these guard against regressing them. The GATT/
    //! advertising/bonding lifecycle still needs a real adapter and is
    //! exercised by the manual smoke test, not here.

    use super::*;
    use crate::admin::AdminRecord;
    use crate::agent::NoopPairingDisplay;
    use crate::claim::ClaimState;
    use crate::testsupport::{MockSvc, req};
    use tempfile::TempDir;

    fn peer(address: &str) -> PeerIdentity {
        PeerIdentity {
            address: address.to_string(),
            address_type: "public".to_string(),
        }
    }

    /// A claimed machine that never touches disk: `authorize` reads only
    /// in-memory state, and none of these tests drive a claim/reset that
    /// would persist. Claimed (not Unclaimed) so non-claim RPCs are
    /// allowed through to the service rather than denied with
    /// `not_claimed`.
    fn claimed_machine() -> Arc<ClaimMachine> {
        let record = AdminRecord::new("AA:BB:CC:DD:EE:FF".into(), "public".into(), "tester".into());
        let store = AdminStore::new(PathBuf::from("/nonexistent/shepherd-ble-test/admin.toml"));
        Arc::new(ClaimMachine::new(store, ClaimState::Claimed(record)))
    }

    fn outbox() -> Arc<Outbox> {
        Arc::new(Outbox::new("test", RESPONSE_OUTBOX_BYTES))
    }

    /// A live unbond sender for the write path. None of these tests drive
    /// `factory_reset`, so nothing is ever sent; the background drainer
    /// just keeps the receiver alive so `send` wouldn't fail on a closed
    /// channel if one ever did.
    fn unbond_sender() -> mpsc::Sender<Address> {
        let (tx, mut rx) = mpsc::channel::<Address>(4);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        tx
    }

    /// Encode a request the way the companion does: JSON body wrapped in
    /// a length-prefixed wire frame.
    fn request_frame(id: u32, method: &str) -> Vec<u8> {
        let body = serde_json::to_vec(&req(id, method, serde_json::Value::Null)).unwrap();
        encode_frame(&body)
    }

    /// Drain an outbox the way the companion's poll loop does — repeated
    /// bounded reads through a `FrameReader` — and decode each frame.
    async fn drain_responses(outbox: &Outbox) -> Vec<RpcResponse> {
        let mut reader = FrameReader::new(MAX_FRAME_BYTES);
        loop {
            let bytes = outbox.read(GATT_MAX_ATTR_VALUE).await;
            if bytes.is_empty() {
                break;
            }
            reader.push(&bytes);
        }
        let mut out = Vec::new();
        while let Some(frame) = reader.pop_frame().expect("outbox bytes frame cleanly") {
            out.push(serde_json::from_slice(&frame).expect("response is valid RpcResponse JSON"));
        }
        out
    }

    #[tokio::test]
    async fn id_one_wipes_stale_outboxes_then_queues_fresh_response() {
        let claim = claimed_machine();
        let mock = Arc::new(MockSvc::new());
        let svc: Arc<dyn ManagementService> = mock.clone();
        let response_outbox = outbox();
        let events_outbox = outbox();

        // Bytes the previous BLE session left behind, unread.
        response_outbox.push(encode_frame(b"stale-response")).await;
        events_outbox.push(encode_frame(b"stale-event")).await;

        // The first RPC of a fresh session always carries id == 1.
        let body = serde_json::to_vec(&req(1, "health", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_sender(),
            &response_outbox,
            &events_outbox,
        )
        .await;

        // Events outbox is wiped and nothing re-queues onto it.
        assert_eq!(events_outbox.pending_bytes().await, 0);

        // Response outbox holds exactly the fresh health response — the
        // stale frame was dropped, not stacked in front of it.
        let responses = drain_responses(&response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 1);
        assert!(responses[0].error.is_none());
        assert_eq!(*mock.health_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn non_first_rpc_leaves_pending_events_intact() {
        let claim = claimed_machine();
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let response_outbox = outbox();
        let events_outbox = outbox();

        events_outbox.push(encode_frame(b"pending-event")).await;
        let before = events_outbox.pending_bytes().await;

        // id != 1: a mid-session RPC must not disturb events the companion
        // has not yet polled.
        let body = serde_json::to_vec(&req(2, "health", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_sender(),
            &response_outbox,
            &events_outbox,
        )
        .await;

        assert_eq!(events_outbox.pending_bytes().await, before);
        let responses = drain_responses(&response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 2);
    }

    #[tokio::test]
    async fn peer_change_resets_partial_frame() {
        let claim = claimed_machine();
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let response_outbox = outbox();
        let events_outbox = outbox();
        let reader = Arc::new(Mutex::new(FrameReader::new(MAX_FRAME_BYTES)));
        let last_peer = Arc::new(Mutex::new(None));

        // Peer A writes only the head of a longer frame, then vanishes.
        let partial = request_frame(7, "health")[..3].to_vec();
        handle_write(
            &peer("AA:AA:AA:AA:AA:AA"),
            partial,
            reader.clone(),
            last_peer.clone(),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            response_outbox.clone(),
            events_outbox.clone(),
        )
        .await
        .unwrap();
        assert!(
            drain_responses(&response_outbox).await.is_empty(),
            "an incomplete frame must not produce a response"
        );

        // Peer B writes a complete frame. If A's leftover header bytes
        // weren't dropped on the peer change, they'd be read as B's length
        // prefix and yield a garbage (id=0 parse-error) frame instead.
        handle_write(
            &peer("BB:BB:BB:BB:BB:BB"),
            request_frame(8, "health"),
            reader.clone(),
            last_peer.clone(),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            response_outbox.clone(),
            events_outbox.clone(),
        )
        .await
        .unwrap();

        let responses = drain_responses(&response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 8);
        assert!(responses[0].error.is_none());
    }

    #[tokio::test]
    async fn framing_error_drops_state_and_recovers() {
        let claim = claimed_machine();
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let response_outbox = outbox();
        let events_outbox = outbox();
        let reader = Arc::new(Mutex::new(FrameReader::new(MAX_FRAME_BYTES)));
        let last_peer = Arc::new(Mutex::new(None));
        let p = peer("CC:CC:CC:CC:CC:CC");

        // A length prefix past MAX_FRAME_BYTES is an unrecoverable framing
        // error; the write path must drop the buffered state rather than
        // wedge on it forever.
        let bad = ((MAX_FRAME_BYTES + 1) as u16).to_le_bytes().to_vec();
        handle_write(
            &p,
            bad,
            reader.clone(),
            last_peer.clone(),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            response_outbox.clone(),
            events_outbox.clone(),
        )
        .await
        .unwrap();
        assert!(drain_responses(&response_outbox).await.is_empty());

        // A valid frame from the same peer afterwards still parses — the
        // reader was reset, not left holding the rejected prefix.
        handle_write(
            &p,
            request_frame(2, "health"),
            reader.clone(),
            last_peer.clone(),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            response_outbox.clone(),
            events_outbox.clone(),
        )
        .await
        .unwrap();

        let responses = drain_responses(&response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 2);
        assert!(responses[0].error.is_none());
    }

    /// On a peer disconnect the monitor wipes every scrap of per-session
    /// transport state — both outboxes, the last-peer marker, and any
    /// half-assembled request frame — so a companion that resumes the
    /// same connection across a transient drop (no fresh `id == 1`)
    /// doesn't inherit a desynced byte stream.
    #[tokio::test]
    async fn reset_transport_session_wipes_all_session_state() {
        let response_outbox = outbox();
        let events_outbox = outbox();
        let reader = Arc::new(Mutex::new(FrameReader::new(MAX_FRAME_BYTES)));
        let last_peer = Arc::new(Mutex::new(Some(peer("AA:BB:CC:DD:EE:FF"))));

        // Bytes a dropped session left behind: unread outbox frames plus
        // the head of a request frame whose tail never arrived.
        response_outbox.push(encode_frame(b"stale-response")).await;
        events_outbox.push(encode_frame(b"stale-event")).await;
        reader.lock().await.push(&request_frame(7, "health")[..3]);

        reset_transport_session(&reader, &last_peer, &response_outbox, &events_outbox).await;

        assert_eq!(response_outbox.pending_bytes().await, 0);
        assert_eq!(events_outbox.pending_bytes().await, 0);
        assert!(last_peer.lock().await.is_none());

        // The reader kept no leftover prefix: a full frame pushed now
        // parses as itself rather than stitched onto the discarded head.
        let mut r = reader.lock().await;
        r.push(&request_frame(8, "health"));
        let frame = r
            .pop_frame()
            .expect("frame parses cleanly")
            .expect("a complete frame is present");
        let parsed: RpcRequest = serde_json::from_slice(&frame).unwrap();
        assert_eq!(parsed.id, 8);
    }

    /// A suspend/resume is not always visible as a power transition, so
    /// the run loop watches two independent triggers. The 2026-08-17
    /// report was a resume that moved no adapter property at all and
    /// still left the device off air.
    #[test]
    fn resume_and_power_on_both_re_arm_the_air() {
        assert_eq!(
            rearm_reason_for_service_event(&EventPayload::SystemResumed),
            Some("the system resumed from sleep"),
        );
        assert_eq!(
            rearm_reason_for_adapter_event(&AdapterEvent::PropertyChanged(
                AdapterProperty::Powered(true)
            )),
            Some("the Bluetooth controller powered back on"),
        );
    }

    /// Everything else must leave the registrations alone: re-arming
    /// drops the GATT application, which disconnects whoever is on it.
    #[test]
    fn ordinary_events_do_not_re_arm_the_air() {
        assert_eq!(
            rearm_reason_for_service_event(&EventPayload::SystemSuspending),
            None,
        );
        assert_eq!(
            rearm_reason_for_service_event(&EventPayload::PolicyReloaded { entry_count: 3 }),
            None,
        );
        assert_eq!(
            rearm_reason_for_adapter_event(&AdapterEvent::PropertyChanged(
                AdapterProperty::Powered(false)
            )),
            None,
        );
        assert_eq!(
            rearm_reason_for_adapter_event(&AdapterEvent::DeviceAdded(
                "AA:BB:CC:DD:EE:FF".parse().expect("valid address")
            )),
            None,
        );
    }

    /// Snapshots coalesce; incremental facts don't.
    ///
    /// The events forwarder runs whether or not a companion is connected,
    /// so without this split an idle daemon under activity churn pins the
    /// events outbox at capacity — and the next companion to connect has
    /// to drain all of it at 512 bytes per GATT round trip before it can
    /// send its first RPC. That's what stalled `connect()` indefinitely
    /// in the first place.
    #[test]
    fn only_whole_state_snapshots_coalesce() {
        let snapshot = shepherd_api::ServiceStateSnapshot {
            api_version: 1,
            policy_loaded: true,
            current_session: None,
            entry_count: 0,
            entries: vec![],
            internet_status: vec![],
        };
        assert_eq!(
            coalesce_key_for(&EventPayload::StateChanged(snapshot)),
            Some(COALESCE_STATE_CHANGED),
        );

        // An incremental event the companion needs in order, not merged.
        assert_eq!(
            coalesce_key_for(&EventPayload::PolicyReloaded { entry_count: 3 }),
            None,
        );
    }

    /// A successful `factory_reset` RPC clears the admin record *and*
    /// queues the bonded peer's address for BlueZ bond removal — without
    /// the latter the phone stays bonded but Unclaimed, locking itself
    /// out with `not_claimed` on every reconnect.
    #[tokio::test]
    async fn factory_reset_requests_bond_removal() {
        // `claimed_machine`'s record identity is AA:BB:CC:DD:EE:FF and its
        // store path doesn't exist, so `factory_reset` clears in memory
        // (the file remove is a no-op) and returns the previous record.
        let claim = claimed_machine();
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let response_outbox = outbox();
        let events_outbox = outbox();
        let (unbond_tx, mut unbond_rx) = mpsc::channel::<Address>(4);

        let body = serde_json::to_vec(&req(5, "factory_reset", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_tx,
            &response_outbox,
            &events_outbox,
        )
        .await;

        assert!(!claim.is_claimed(), "device is Unclaimed after reset");
        let addr = unbond_rx.try_recv().expect("bond removal was requested");
        assert_eq!(addr.to_string(), "AA:BB:CC:DD:EE:FF");

        // The reset still returns a success response to the peer.
        let responses = drain_responses(&response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 5);
        assert!(responses[0].error.is_none());
    }

    /// A `factory_reset` on an already-unclaimed device is a no-op success
    /// and must not queue a bond removal (there is no bond to forget).
    #[tokio::test]
    async fn factory_reset_when_unclaimed_requests_no_removal() {
        let store = AdminStore::new(PathBuf::from("/nonexistent/shepherd-ble-test/admin.toml"));
        let claim: Arc<ClaimMachine> = Arc::new(ClaimMachine::new(store, ClaimState::Unclaimed));
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let response_outbox = outbox();
        let events_outbox = outbox();
        let (unbond_tx, mut unbond_rx) = mpsc::channel::<Address>(4);

        let body = serde_json::to_vec(&req(1, "factory_reset", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_tx,
            &response_outbox,
            &events_outbox,
        )
        .await;

        assert!(matches!(
            unbond_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let responses = drain_responses(&response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert!(responses[0].error.is_none());
    }

    /// Addresses are compared parsed, so the config may write them in
    /// any case or the operator may paste them from `bluetoothctl`.
    #[test]
    fn adapter_addresses_compare_case_insensitively() {
        let lower: bluer::Address = "dc:56:7b:1f:7d:ea".parse().unwrap();
        let upper: bluer::Address = "DC:56:7B:1F:7D:EA".parse().unwrap();
        assert_eq!(lower, upper);
        // …and a name is not mistaken for an address.
        assert!("hci1".parse::<bluer::Address>().is_err());
        // A bare index is not a valid selector either — it must be the
        // interface name or the address, so "1" can't silently mean hci1.
        assert!("1".parse::<bluer::Address>().is_err());
    }

    /// The factory-reset *sentinel* path captures the previously-bonded
    /// admin so `run` can remove the BlueZ bond, and clears the record.
    #[test]
    fn advertised_name_fits_the_31_byte_pdu() {
        // The budget is what's left after Flags (3) + 128-bit UUID (18) +
        // the local-name AD header (2) in a 31-byte legacy PDU.
        assert_eq!(MAX_ADV_NAME_BYTES, 8);
        // Every result must fit the budget in bytes.
        let fits = |s: &str| advertised_name(s).len() <= MAX_ADV_NAME_BYTES;

        // Fits already -> returned verbatim (8 bytes is the boundary).
        assert_eq!(advertised_name("shepherd"), "shepherd");
        assert_eq!(advertised_name("pi"), "pi");
        // The hostname that triggered the field report: 10 bytes -> 5-byte
        // prefix + "…" (3 bytes) = 8.
        assert_eq!(advertised_name("copernicus"), "coper…");
        assert!(fits("copernicus"));
        // Never splits a multi-byte code point: reserving 3 bytes for the
        // ellipsis lands the cut at byte 5, a boundary here.
        assert_eq!(advertised_name("aaaaaaaé"), "aaaaa…");
        // When the 5-byte cut would land inside 'é' (bytes 4..6), back off
        // to byte 4, so 4-byte prefix + "…" = 7 bytes.
        assert_eq!(advertised_name("aaaaébbbb"), "aaaa…");
        assert!(fits("aaaaébbbb"));
        // A multi-byte char ending exactly on the budget is kept (6 + 2 = 8).
        assert_eq!(advertised_name("aaaaaaé"), "aaaaaaé");
    }

    #[test]
    fn sentinel_reset_captures_bond_for_removal() {
        let dir = TempDir::new().unwrap();
        let admin_path = dir.path().join("admin.toml");
        let sentinel_path = dir.path().join(".factory-reset-ble");

        let store = AdminStore::new(admin_path.clone());
        store
            .save(&AdminRecord::new(
                "AA:BB:CC:DD:EE:FF".into(),
                "public".into(),
                "phone".into(),
            ))
            .unwrap();
        std::fs::write(&sentinel_path, "").unwrap();

        let config = BleServerConfig {
            device_name: "shepherd".into(),
            firmware_version: "test".into(),
            admin_record_path: admin_path.clone(),
            reset_sentinel_path: sentinel_path,
            adapter: None,
        };
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let display: Arc<dyn PairingDisplay> = Arc::new(NoopPairingDisplay);
        let server = BleServer::new(config, svc, display).unwrap();

        assert!(
            !server.claim.is_claimed(),
            "device is Unclaimed after reset"
        );
        assert!(!admin_path.exists(), "admin record file was removed");

        // The address is queued *on disk*, not just in memory: the
        // removal happens later in `run`, and if it fails (or the daemon
        // is killed first) the next startup has to be able to retry. A
        // fresh store on the same path stands in for that next startup.
        let queued = server.pending_unbond.list().unwrap();
        assert_eq!(queued, vec!["AA:BB:CC:DD:EE:FF".to_string()]);
        let next_boot = PendingUnbondStore::new(server.pending_unbond.path().to_path_buf());
        assert_eq!(next_boot.list().unwrap(), queued);
    }
}
