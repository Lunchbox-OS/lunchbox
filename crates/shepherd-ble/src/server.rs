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
use shepherd_api::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSink, DiagnosticSubject, EventPayload,
};
use shepherd_management::ManagementService;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::admin::{AdminStore, PendingUnbondStore, check_reset_sentinel};
use crate::agent::{PairingDisplay, build_agent};
use crate::claim::{AuthDecision, ClaimError, ClaimMachine, PeerIdentity};
use crate::framing::{FrameReader, encode_frame};
use crate::outbox::{COALESCE_DIAGNOSTICS, COALESCE_STATE_CHANGED, CoalesceKey, Outbox};
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

#[derive(Clone)]
pub struct BleServerConfig {
    /// Advertised local name (also the device name in `DeviceInfo`).
    /// Defaults to the system hostname when constructed via `default`.
    pub device_name: String,
    /// Firmware version string surfaced via `DeviceInfo`. Typically the
    /// crate version of the daemon.
    pub firmware_version: String,
    /// Which controller to serve on: an address (`"DC:56:7B:1F:7D:EA"`)
    /// or an interface name (`"hci1"`). `None` takes whichever BlueZ
    /// lists first. See [`resolve_adapter`].
    pub adapter: Option<String>,
    /// Where the admin record, the unbond queue and the reset sentinel live.
    ///
    /// On a device the state custodian, at a uid activities do not have, which
    /// matters most for the admin record — it carries the minted HTTP token, so
    /// a copy at the shared uid is a credential every activity can read and
    /// present to the management API. In a dev stack and the tests,
    /// `LocalProtectedFiles` over a directory.
    ///
    /// Not an `Option` and not a set of paths. Both were the same mistake: they
    /// let the three files live somewhere the custodian does not serve, which
    /// on a device is somewhere an activity can reach.
    pub files: Arc<dyn shepherd_util::ProtectedFiles>,
}

impl std::fmt::Debug for BleServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BleServerConfig")
            .field("device_name", &self.device_name)
            .field("firmware_version", &self.firmware_version)
            .field("adapter", &self.adapter)
            .finish()
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
    /// Where to report administrator-facing conditions (issue #143). `None`
    /// outside the daemon — the server runs in tests and tools that have no
    /// registry, and a missing sink must not change its behaviour.
    diagnostics: Option<Arc<dyn DiagnosticSink>>,
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
        let store = AdminStore::new(Arc::clone(&config.files));
        let pending_unbond = PendingUnbondStore::new(Arc::clone(&config.files));
        let reset_requested = check_reset_sentinel(config.files.as_ref());

        if reset_requested {
            warn!("Factory-reset sentinel present at startup; clearing admin record");
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
                // Every admin, not just the first: a device with two bonded
                // phones has two bonds to forget, and leaving one behind is
                // the asymmetric-bond lockout for whichever phone was missed.
                Ok(stored) => {
                    for record in &stored.admins {
                        if let Err(e) = pending_unbond.add(&record.identity_address) {
                            warn!(
                                peer = %record.identity_address,
                                error = %e,
                                "Could not queue BlueZ bond removal after reset; the peer may stay bonded",
                            );
                        }
                    }
                }
                Err(e) => warn!(
                    error = %e,
                    "Could not read admin record before reset; BlueZ bonds will not be removed",
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
            diagnostics: None,
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

    /// Report administrator-facing conditions through `sink` (issue #143).
    ///
    /// Opt-in rather than a constructor argument: every other caller of
    /// [`Self::new`] — tests, tools — has no registry to hand over, and
    /// threading an `Option` through all of them would buy nothing.
    pub fn with_diagnostics(mut self, sink: Arc<dyn DiagnosticSink>) -> Self {
        self.diagnostics = Some(sink);
        self
    }

    /// Report whether the pairing agent is registered.
    ///
    /// Both directions, from the same place: the agent is re-registered when
    /// the adapter powers back on, so a failure that resolves on its own must
    /// clear on its own too.
    fn report_agent_status(&self, registered: bool) {
        let Some(sink) = &self.diagnostics else {
            return;
        };
        if registered {
            sink.clear(
                DiagnosticCode::BlePairingAgentUnavailable,
                &DiagnosticSubject::Service,
            );
        } else {
            sink.raise(Diagnostic {
                code: DiagnosticCode::BlePairingAgentUnavailable,
                subject: DiagnosticSubject::Service,
                severity: DiagnosticSeverity::Warning,
                message: "The Bluetooth pairing agent could not be registered, so pairing a \
                          new phone will not show the confirmation code on the TV"
                    .to_string(),
                remedy: Some("Check that the daemon user is in the `bluetooth` group.".to_string()),
                since: shepherd_util::now(),
            });
        }
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
        self.report_agent_status(agent_handle.is_some());

        // Everything the GATT characteristics, the disconnect monitor
        // and the first-RPC watchdog share about the peer session: the
        // outboxes, the request reassembler, and the facts each of them
        // reasons about. See [`TransportState`].
        let state = Arc::new(TransportState::new());

        // A successful `factory_reset` RPC must also forget the BlueZ
        // bond, or the same lockout as the sentinel path results. The RPC
        // handler runs deep in the GATT write path with no adapter access,
        // so it hands the peer address to this task, which owns the
        // adapter and calls `remove_device`.
        let (unbond_tx, mut unbond_rx) = mpsc::channel::<Address>(4);

        let mut on_air = go_on_air(
            &adapter,
            &self.config,
            &self.svc,
            &self.claim,
            &unbond_tx,
            &state,
        )
        .await?;

        let events_task = tokio::spawn(events_forwarder(
            self.svc.clone(),
            state.events_outbox.clone(),
        ));

        // Wipe stale transport bytes when a peer drops, so a companion
        // that resumes the same connection across a transient BLE
        // reconnect (no fresh `id == 1` sentinel) doesn't inherit a
        // desynced byte stream.
        let disconnect_task = tokio::spawn(disconnect_monitor(adapter.clone(), state.clone()));

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
                // Same identity-vs-path resolution as the startup drain: the
                // address came from an admin record, and the live BlueZ object
                // may sit at the random address the peer connected under.
                let removed = match device_path_for(&unbond_adapter, addr).await {
                    Some(path) => unbond_adapter.remove_device(path).await,
                    None => {
                        info!(peer = %addr, "BlueZ already has no such peer; nothing to unbond");
                        Ok(())
                    }
                };
                match removed {
                    Ok(()) => {
                        info!(peer = %addr, "Removed BlueZ bond");
                        if let Err(e) = unbond_store.remove(&text) {
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
            // Info, not warn: on a box that suspends this is routine and
            // it succeeds. Logging expected success at warning level is
            // how a journal trains its readers to skim past warnings —
            // and this project debugs from the journal. The failure path
            // below is the part worth raising your voice about.
            info!(
                adapter = %adapter.name(),
                reason,
                "Re-registering the GATT application and advertisement",
            );
            // A startup without an agent is survivable but degraded, so
            // take every re-arm as another chance at one.
            if agent_handle.is_none() {
                agent_handle = register_agent_with_retry(&session, &self.display).await;
                self.report_agent_status(agent_handle.is_some());
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
                &unbond_tx,
                &state,
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

/// Find the BlueZ device object for a peer we know by its identity address.
///
/// `Adapter::remove_device` addresses a device by its D-Bus object path, and
/// that path is the address the object was *created* under — for a phone using
/// privacy, the random address its link came up on. The admin record holds the
/// identity address instead, deliberately (see [`resolve_peer`]), so handing it
/// straight to `remove_device` asks BlueZ about an object that does not exist.
///
/// Observed on the bench (2026-09-07): revoking an administrator logged
/// `Bluetooth device does not exist: Does Not Exist` for `64:11:A4:B0:7B:D9`
/// while the live object sat at `dev_53_B5_E3_8D_D6_49`. Worse, the startup
/// drain then read the same absence as "already settled" and dropped the queue
/// entry — so the bond a revocation was supposed to remove would have survived
/// forever, with nothing left saying it should not have.
///
/// So: try the address as a path first, and otherwise ask each known device for
/// its identity. `None` means BlueZ genuinely has no such peer, which is the
/// one case where the debt really is settled.
async fn device_path_for(adapter: &bluer::Adapter, wanted: Address) -> Option<Address> {
    let addresses = match adapter.device_addresses().await {
        Ok(addresses) => addresses,
        Err(e) => {
            warn!(error = %e, "Could not enumerate BlueZ devices while looking for a bond");
            return None;
        }
    };
    if addresses.contains(&wanted) {
        return Some(wanted);
    }
    for address in addresses {
        let Ok(device) = adapter.device(address) else {
            continue;
        };
        if device.remote_address().await.is_ok_and(|id| id == wanted) {
            debug!(
                identity = %wanted,
                path = %address,
                "Bond is recorded by identity but its BlueZ object is at another address",
            );
            return Some(address);
        }
    }
    None
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
        // By identity, not by the queued string: the entry came from an admin
        // record, and BlueZ may still hold the object under the random address
        // the peer last connected with. See [`device_path_for`].
        let Some(path) = device_path_for(adapter, addr).await else {
            info!(peer = %addr, "Pending unbond already settled; BlueZ does not know this peer");
            let _ = store.remove(&text);
            continue;
        };
        match adapter.remove_device(path).await {
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
/// Only whole-state payloads qualify: a newer one makes every queued older
/// one redundant, so collapsing them costs nothing and keeps the events
/// outbox — and therefore the companion's connect-time drain — small.
/// Everything else is an incremental fact (a warning fired, a session ended)
/// that the companion needs in order, so it queues normally.
///
/// `DiagnosticsChanged` (issue #143) carries the whole diagnostic set rather
/// than a raise/clear delta, which is what earns it a key here.
fn coalesce_key_for(payload: &EventPayload) -> Option<CoalesceKey> {
    match payload {
        EventPayload::StateChanged(_) => Some(COALESCE_STATE_CHANGED),
        EventPayload::DiagnosticsChanged(_) => Some(COALESCE_DIAGNOSTICS),
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
async fn disconnect_monitor(adapter: bluer::Adapter, state: Arc<TransportState>) {
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
    // Keyed by address and holding the task, not just the address. A
    // `HashSet` let a `DeviceRemoved`/`DeviceAdded` pair — which BlueZ
    // emits routinely for a bonded peer as private addresses rotate —
    // free the slot while the old watcher was still running, so a second
    // one attached to the same peer. Observed on 2026-08-18: every
    // connect and disconnect handled exactly twice, for the whole
    // session. Idempotent work, so it was invisible, but the tasks and
    // their D-Bus subscriptions are never reclaimed and a long-running
    // kiosk would keep accruing them.
    let mut watchers: HashMap<Address, tokio::task::JoinHandle<()>> = HashMap::new();
    match adapter.device_addresses().await {
        Ok(addrs) => {
            for addr in addrs {
                if let std::collections::hash_map::Entry::Vacant(slot) = watchers.entry(addr) {
                    // Pin bonded peers here as well as on connect. A peer
                    // that is already connected when we start — the usual
                    // case after a daemon restart, since the ACL outlives
                    // it — never produces a `Connected(true)` transition,
                    // so the on-connect path alone would leave the admin
                    // phone unpinned for the whole session.
                    if let Ok(device) = adapter.device(addr)
                        && device.is_paired().await.unwrap_or(false)
                    {
                        pin_peer_to_bredr(&device, addr, &state.bearer, PinFailure::Quiet).await;
                    }
                    slot.insert(spawn_device_watcher(&adapter, addr, state.clone()));
                }
            }
        }
        Err(e) => debug!(error = %e, "could not enumerate known devices for disconnect watch"),
    }

    while let Some(ev) = events.next().await {
        match ev {
            AdapterEvent::DeviceAdded(addr) => {
                // Re-attach only if nothing live is already watching this
                // peer. `is_finished` covers the case where the watcher's
                // own stream ended without us seeing a `DeviceRemoved`.
                let live = watchers.get(&addr).is_some_and(|h| !h.is_finished());
                if !live {
                    let handle = spawn_device_watcher(&adapter, addr, state.clone());
                    if let Some(stale) = watchers.insert(addr, handle) {
                        stale.abort();
                    }
                }
            }
            // A removed device may later be re-added. Stop the watcher
            // rather than just forgetting it: the device event stream does
            // not reliably end with the object, and leaving the task
            // running is what produced duplicate watchers.
            AdapterEvent::DeviceRemoved(addr) => {
                if let Some(handle) = watchers.remove(&addr) {
                    handle.abort();
                }
            }
            AdapterEvent::PropertyChanged(_) => {}
        }
    }
}

/// Spawn a task that clears the transport session state each time
/// `addr` transitions to disconnected. The task lives until the device
/// object is removed from BlueZ (its event stream ends), which spans
/// many connect/disconnect cycles for a bonded peer.
fn spawn_device_watcher(
    adapter: &bluer::Adapter,
    addr: Address,
    state: Arc<TransportState>,
) -> tokio::task::JoinHandle<()> {
    let device = match adapter.device(addr) {
        Ok(d) => d,
        Err(e) => {
            debug!(peer = %addr, error = %e, "could not open device for disconnect watch");
            // A handle that is already finished, so callers can store it
            // uniformly and the liveness check reads false.
            return tokio::spawn(async {});
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
        // Act on *transitions*, not on every delivery. BlueZ (or bluer's
        // subscription to it) reports each `Connected` change twice,
        // fractions of a millisecond apart — verified by tagging the
        // watcher task and seeing one instance log the same connect
        // twice. Everything below is idempotent, so the duplicates were
        // harmless, but they doubled every epoch bump (leaving the first
        // watchdog of each pair to no-op on a stale generation) and put
        // two identical lines in the journal for every event, which is
        // its own cost when the journal is the debugging tool.
        //
        // The event payload is *not* the state, and this is the whole
        // reason the loop reads the property back. `bluer::Device::events`
        // keeps only the changed properties, throwing away the interface
        // that emitted them (`device.rs`: `Event::PropertiesChanged {
        // changed, .. }`), and then matches on the property *name*. On
        // BlueZ >= 5.82 with `Experimental` — which shepherd requires for
        // `PreferredBearer`, so every device has it — one device object
        // carries `org.bluez.Device1`, `org.bluez.Bearer.LE1` *and*
        // `org.bluez.Bearer.BREDR1`, and all three have a `Connected`
        // property. So a dual-mode phone bringing its classic profiles up
        // and down (A2DP, AVRCP, HFP — captured on the wire) arrives here
        // as `Connected(true)` then `Connected(false)` for a bearer that
        // has nothing to do with the LE link carrying our GATT service,
        // which never went anywhere.
        //
        // `is_connected()` asks for `Device1`'s own property, so it
        // answers for the device rather than for whichever bearer last
        // twitched. Reading it back costs one round trip per event and
        // turns "something about connectivity changed" into the fact.
        let mut connected: Option<bool> = None;
        while let Some(DeviceEvent::PropertyChanged(prop)) = events.next().await {
            if !matches!(prop, DeviceProperty::Connected(_)) {
                continue;
            }
            let now = match device.is_connected().await {
                Ok(now) => now,
                // The object is going away — the stream is about to end
                // anyway, and guessing from the payload is what this is
                // here to avoid.
                Err(e) => {
                    debug!(peer = %addr, error = %e, "could not read Connected; ignoring the event");
                    continue;
                }
            };
            if connected.replace(now) == Some(now) {
                continue;
            }
            match DeviceProperty::Connected(now) {
                // A companion drains both outboxes before it will send
                // its first RPC, so the depth logged here *is* the
                // connect latency it's about to pay. Without this line a
                // stalled drain is entirely invisible: the peer connects
                // (no log), reads thousands of times (debug only), and
                // never writes, so the journal shows nothing at all
                // between "advertising started" and an RPC that may
                // arrive minutes later or not at all.
                DeviceProperty::Connected(true) => {
                    // Anything queued while nobody was connected is stale:
                    // the companion discards its entire post-connect drain
                    // and opens with a fresh `service_state`. Handing it
                    // over costs a GATT round trip per 512 bytes and buys
                    // nothing — on the reporter's box ~4 KiB of queued
                    // snapshots stretched a 1.5s reconnect to 3.3s, which
                    // is the "sometimes it takes a few seconds" they saw.
                    // `clear_if_aligned` declines if the peer has already
                    // started reading, so a drain in flight is never cut
                    // mid-frame.
                    let response = state.response_outbox.clear_if_aligned().await;
                    let events = state.events_outbox.clear_if_aligned().await;
                    let (response_frames, response_bytes) = response.unwrap_or((0, 0));
                    let (events_frames, events_bytes) = events.unwrap_or((0, 0));
                    info!(
                        peer = %addr,
                        response_frames,
                        response_bytes,
                        events_frames,
                        events_bytes,
                        kept_for_drain = response.is_none() || events.is_none(),
                        "BLE peer connected; discarded what had queued up while it was away",
                    );
                    let generation = state.epoch.fetch_add(1, Ordering::Relaxed) + 1;
                    // Do this on every connect, not just the first: it is
                    // idempotent in BlueZ, and the arming we are undoing
                    // happens on service probe, which can recur.
                    if device.is_paired().await.unwrap_or(false) {
                        pin_peer_to_bredr(&device, addr, &state.bearer, PinFailure::Loud).await;
                    }
                    spawn_first_rpc_watchdog(device.clone(), addr, generation, state.clone());
                }
                // "Reported gone", not "gone": this property lags the
                // traffic it describes, and the bearer pin above provokes
                // one of these on every fresh pairing while the link is
                // still carrying ATT. Bumping the epoch is
                // safe either way — it only retires watchdogs — and
                // `reset_session` is careful about what it discards.
                DeviceProperty::Connected(false) => {
                    state.epoch.fetch_add(1, Ordering::Relaxed);
                    info!(peer = %addr, "BLE peer reported disconnected; releasing session state");
                    state.reset_session().await;
                }
                _ => {}
            }
        }
    })
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

/// Everything the transport layers share about the current peer session.
///
/// These fields have always travelled together — the request
/// reassembler, the peer marker, both outboxes, and the facts the
/// watchdog reasons about — through the GATT characteristics, the
/// disconnect monitor, the per-device watchers and back. Passing them
/// individually meant nine-parameter functions and a `too_many_arguments`
/// allow on almost everything that touched them, which buried the one or
/// two arguments that actually varied per call.
struct TransportState {
    /// Request-side reassembly. v1 holds a single reader because only one
    /// admin connection is expected at a time.
    reader: Mutex<FrameReader>,
    /// Who currently holds the session, and when they last wrote.
    ///
    /// Set by the first write of a session, cleared when the peer drops —
    /// which is what the watchdog reads as "this link has never carried
    /// anything". It is also the turn-taking lock: everything below this
    /// struct is single-session (one reader, one pair of outboxes), so a
    /// second administrator writing here would interleave frames into the
    /// same reassembler and read replies out of the same queue. See
    /// [`SESSION_HANDOVER_IDLE`].
    last_peer: Mutex<Option<SessionOwner>>,
    /// The read-poll queues the companion drains over GATT: RPC replies
    /// and live state events. `Arc`, not plain, because each is also
    /// captured by its own read characteristic and — for events — by the
    /// forwarder task that runs whether or not anyone is connected.
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
    /// Distinguishes one peer connection from the next, so a watchdog
    /// armed for a link that has since dropped cannot act on whatever
    /// connection happens to be live when its timer expires.
    epoch: AtomicU64,
    /// Whether any peer has exchanged real RPCs on this boot. A first
    /// pairing never has, which is what keeps the fallback eviction away
    /// from the Numeric Comparison window.
    had_session: AtomicBool,
    /// Which peers have been pinned to the BR/EDR bearer. Together with
    /// `had_session` this gates the fallback eviction in
    /// [`spawn_first_rpc_watchdog`]; see [`pin_peer_to_bredr`] for why
    /// the pin is the actual fix.
    bearer: BearerPin,
    /// Peers resolved to their identity address this session, keyed by the
    /// address their requests arrive under. See [`resolve_peer`].
    identities: Mutex<HashMap<Address, PeerIdentity>>,
}

/// The peer holding the session, and when it last proved it.
struct SessionOwner {
    peer: PeerIdentity,
    last_write: std::time::Instant,
}

/// How long a silent session owner keeps its claim on the transport.
///
/// The normal handover is a disconnect: BlueZ reports the peer gone and
/// [`TransportState::reset_session`] releases the session. This is the backstop
/// for when that report never comes — a phone that walked out of range without
/// a clean teardown must not lock the other parent out of the device forever.
///
/// Comfortably past the companion's own 15-second per-RPC timeout, so a slow
/// call in progress is never mistaken for an abandoned session.
const SESSION_HANDOVER_IDLE: Duration = Duration::from_secs(30);

impl TransportState {
    fn new() -> Self {
        Self {
            reader: Mutex::new(FrameReader::new(MAX_FRAME_BYTES)),
            last_peer: Mutex::new(None),
            response_outbox: Arc::new(Outbox::new("response", RESPONSE_OUTBOX_BYTES)),
            events_outbox: Arc::new(Outbox::new("events", EVENTS_OUTBOX_BYTES)),
            epoch: AtomicU64::new(0),
            had_session: AtomicBool::new(false),
            bearer: BearerPin::default(),
            identities: Mutex::new(HashMap::new()),
        }
    }

    /// Drop per-session transport state after BlueZ reports the peer
    /// gone — *except* an outbox the peer is visibly still draining.
    ///
    /// `Device.Connected` is not a trustworthy account of whether the
    /// GATT link still carries ATT traffic. It arrives late (over a
    /// second behind the reads and writes it purports to describe), and
    /// [`pin_peer_to_bredr`] provokes a spurious `Connected(false)` of
    /// its own: pinning a freshly-paired peer tears down the kernel's LE
    /// auto-connect, BlueZ reports the device disconnected, and it never
    /// reports it back — while the companion carries on reading and
    /// writing over the same link for the rest of the session.
    ///
    /// Wiping a mid-delivery outbox on that report is the whole defect.
    /// The response the companion is halfway through
    /// draining vanishes underneath it; it has no way to notice, because
    /// a truncated read is indistinguishable from an idle one, so it
    /// polls an empty characteristic until its 15-second RPC timeout.
    /// After a first pairing that response is the opening `service_state`
    /// and the companion's first screen is empty.
    ///
    /// So the outboxes go through [`Outbox::clear_if_aligned`], which
    /// declines while a peer holds a partial frame — the same rule
    /// [`Outbox::push_inner`] and the connect handler already follow, and
    /// for the same reason: bytes a peer is in the middle of reading are
    /// not ours to throw away. Anything genuinely stale is disposed of
    /// twice over, by the companion's post-connect drain and by the
    /// `id == 1` clear in [`handle_request`].
    ///
    /// The request-side reader is reset unconditionally, and that
    /// asymmetry is deliberate. A surviving partial *request* has no such
    /// second chance: nothing on this side can resync a byte stream that
    /// starts mid-frame, so the next session's writes would stitch onto
    /// the orphan and every frame after it would be garbage. Requests are
    /// small enough to arrive in a single ATT write in practice, so there
    /// is next to nothing in flight to protect.
    /// Whether `addr` may drain the outboxes right now.
    ///
    /// Nobody owning the session means yes: the companion drains both outboxes
    /// inside `connect()`, before it has written anything, so the first reads
    /// of every session necessarily arrive without an owner. Once someone has
    /// written, the queues are theirs — a second phone polling the same
    /// characteristics would otherwise carry off replies addressed to the
    /// first, and neither side could tell.
    async fn may_read(&self, addr: Address) -> bool {
        match &*self.last_peer.lock().await {
            None => true,
            Some(owner) => owner.peer.address.eq_ignore_ascii_case(&addr.to_string()),
        }
    }

    /// Take the session for `peer`, or report who has it.
    ///
    /// `Ok(fresh)` — the session is theirs; `fresh` means the previous owner
    /// was someone else, so the request reassembler has to be reset before
    /// their bytes are pushed into it.
    async fn take_session(&self, peer: &PeerIdentity) -> Result<bool, String> {
        let mut owner = self.last_peer.lock().await;
        let now = std::time::Instant::now();
        if let Some(current) = owner.as_mut() {
            if current.peer == *peer {
                current.last_write = now;
                return Ok(false);
            }
            let idle = now.duration_since(current.last_write);
            if idle < SESSION_HANDOVER_IDLE {
                return Err(current.peer.address.clone());
            }
        }
        *owner = Some(SessionOwner {
            peer: peer.clone(),
            last_write: now,
        });
        Ok(true)
    }

    async fn reset_session(&self) {
        *self.reader.lock().await = FrameReader::new(MAX_FRAME_BYTES);
        *self.last_peer.lock().await = None;
        // Cheap to rebuild — one property read on the next session's first
        // write — and holding it across a re-pair would answer with the
        // identity of a bond that no longer exists.
        self.identities.lock().await.clear();
        for (label, outbox) in [
            ("response", &self.response_outbox),
            ("events", &self.events_outbox),
        ] {
            if outbox.clear_if_aligned().await.is_none() {
                let (frames, bytes) = outbox.depth().await;
                info!(
                    outbox = label,
                    frames,
                    bytes,
                    "peer reported gone mid-delivery; keeping what it is still reading",
                );
            }
        }
    }
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
    /// Peers pinned so far, by address.
    ///
    /// Per-peer rather than a single flag: a box carries more than one
    /// bond (a stale object from an earlier pairing, headphones, a
    /// controller), and a shared flag means the first peer to succeed
    /// suppresses the attempt for everyone after it. On 2026-08-18 that
    /// came within one enumeration-order coin flip of skipping the admin
    /// phone and silently restoring the whole dialling failure, with a
    /// reassuring "Pinned peer" line in the log naming a different device.
    pinned: std::sync::Mutex<HashSet<Address>>,
    /// Whether the operator has been told how to make the property
    /// available. Once per server is plenty.
    warned: AtomicBool,
}

impl BearerPin {
    fn is_pinned(&self, addr: Address) -> bool {
        self.pinned
            .lock()
            .expect("bearer pin lock poisoned")
            .contains(&addr)
    }

    fn mark_pinned(&self, addr: Address) {
        self.pinned
            .lock()
            .expect("bearer pin lock poisoned")
            .insert(addr);
    }
}

/// How loudly [`pin_peer_to_bredr`] should complain when it cannot pin.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PinFailure {
    /// A peer we are actively serving: unprotected means the next
    /// disconnect can hand the device the central role, so say so.
    Loud,
    /// A sweep over everything BlueZ knows, where failures are expected —
    /// stale objects from old pairings have no `Device1` to set. Warning
    /// about those buries the case that matters, and did: every session
    /// opened with an alarming paragraph about a device that never
    /// connects, immediately before the admin phone pinned fine.
    Quiet,
}

/// Apply [`prefer_bredr_bearer`] and say something useful either way.
///
/// Success is logged once (the flag also tells the watchdog it no longer
/// needs its fallback). Failure is almost always "the property is
/// experimental and this bluetoothd was started without it", which is not
/// something the daemon can fix for the operator — so say exactly what to
/// do about it, once, rather than repeating it on every reconnect.
async fn pin_peer_to_bredr(
    device: &bluer::Device,
    addr: Address,
    pin: &BearerPin,
    on_failure: PinFailure,
) {
    if pin.is_pinned(addr) {
        return;
    }
    match prefer_bredr_bearer(device.adapter_name(), addr).await {
        Ok(()) => {
            pin.mark_pinned(addr);
            info!(
                peer = %addr,
                "Pinned peer to the BR/EDR bearer; the kernel will no longer auto-connect \
                 it over LE, so the companion keeps the central role and can encrypt",
            );
        }
        Err(e) if on_failure == PinFailure::Quiet || pin.warned.swap(true, Ordering::Relaxed) => {
            debug!(
                error = %e,
                peer = %addr,
                "PreferredBearer=bredr unavailable for this peer",
            )
        }
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
    state: Arc<TransportState>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_RPC_GRACE).await;
        if state.epoch.load(Ordering::Relaxed) != generation {
            // The link this timer was armed for is long gone; anything
            // live now is a *different* connection with its own timer.
            // Without this check the state below is read against the
            // wrong link: on 2026-08-17 a timer armed at 03:37:47 fired
            // at 03:38:12 against a healthy connection that had come up
            // 1.2s earlier and was about to send its first RPC.
            return;
        }
        if state.last_peer.lock().await.is_some() {
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
        if state.bearer.is_pinned(addr) || !state.had_session.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(EVICT_GRACE - FIRST_RPC_GRACE).await;
        if state.epoch.load(Ordering::Relaxed) != generation
            || state.last_peer.lock().await.is_some()
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
async fn go_on_air(
    adapter: &bluer::Adapter,
    config: &BleServerConfig,
    svc: &Arc<dyn ManagementService>,
    claim: &Arc<ClaimMachine>,
    unbond_tx: &mpsc::Sender<Address>,
    state: &Arc<TransportState>,
) -> bluer::Result<OnAir> {
    let application = build_application(
        adapter.clone(),
        config.clone(),
        svc.clone(),
        claim.clone(),
        unbond_tx.clone(),
        state.clone(),
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

fn build_application(
    adapter: bluer::Adapter,
    config: BleServerConfig,
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    unbond_tx: mpsc::Sender<Address>,
    state: Arc<TransportState>,
) -> Application {
    Application {
        services: vec![Service {
            uuid: SHEPHERD_MANAGEMENT_SERVICE_UUID,
            primary: true,
            characteristics: vec![
                device_info_characteristic(config, claim.clone()),
                request_characteristic(adapter, svc, claim, unbond_tx, state.clone()),
                outbox_read_characteristic(
                    SHEPHERD_RESPONSE_CHAR_UUID,
                    state.response_outbox.clone(),
                    "Response",
                    state.clone(),
                ),
                outbox_read_characteristic(
                    SHEPHERD_EVENTS_CHAR_UUID,
                    state.events_outbox.clone(),
                    "Events",
                    state.clone(),
                ),
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
fn request_characteristic(
    adapter: bluer::Adapter,
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    unbond_tx: mpsc::Sender<Address>,
    state: Arc<TransportState>,
) -> Characteristic {
    // The reassembly state in `state` is single-connection (v1 expects
    // one admin connection at a time). It's created in `run` and shared
    // with `disconnect_monitor` so a peer drop can reset a half-written
    // frame; if a second device writes here we wipe the buffer and start
    // fresh.
    Characteristic {
        uuid: SHEPHERD_REQUEST_CHAR_UUID,
        write: Some(CharacteristicWrite {
            write: true,
            write_without_response: true,
            encrypt_authenticated_write: true,
            method: CharacteristicWriteMethod::Fun(Box::new(move |chunk, req| {
                let adapter = adapter.clone();
                let svc = svc.clone();
                let claim = claim.clone();
                let unbond_tx = unbond_tx.clone();
                let state = state.clone();
                // A write landed on an encrypt-authenticated
                // characteristic, so this link demonstrably works. That is
                // what the fallback eviction waits to see before it will
                // ever act on a silent one.
                state.had_session.store(true, Ordering::Relaxed);
                async move {
                    // The request names its peer by D-Bus object path, which
                    // for a phone using privacy is not the address its bond is
                    // filed under. Ask BlueZ before anything compares it
                    // against a record — see [`resolve_peer`].
                    let peer = resolve_peer(&adapter, req.device_address, &state).await;
                    handle_write(&peer, chunk, claim, svc, &unbond_tx, &state).await
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
    state: Arc<TransportState>,
) -> Characteristic {
    Characteristic {
        uuid,
        read: Some(CharacteristicRead {
            read: true,
            encrypt_authenticated_read: true,
            fun: Box::new(move |req| {
                let outbox = outbox.clone();
                let state = state.clone();
                async move {
                    // The other half of the turn-taking rule in `handle_write`
                    // (issue #149): while one administrator holds the session,
                    // a second bonded phone polling these characteristics
                    // would carry off replies meant for the first. An empty
                    // read is what an idle queue looks like, so the newcomer
                    // simply sees nothing until its own write is refused and
                    // tells it why.
                    if !state.may_read(req.device_address).await {
                        return Ok(Vec::new());
                    }
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

/// Ask BlueZ who a request actually came from.
///
/// A GATT request names its peer by the address the D-Bus device object was
/// created under. For a phone using privacy that is the random address the link
/// came up on, which is *not* the address the bond is filed under and not the
/// one the next connection will arrive with. `Device1.Address` on the same
/// object is — bluer's own docs for the property say it "represents … Identity
/// Address after pairing", and on the bench a mid-pairing claim from path
/// `79:C2:1B:08:F0:32` resolved to the `64:11:A4:B0:7B:D9` its bond was being
/// written to.
///
/// Recording the wrong one writes an administrator that can never authorize
/// again. So this runs before the first request of a session is dispatched, and
/// the answer is cached: it is one property read per session, not per RPC.
///
/// A failure falls back to the request address — the pre-#149 behaviour, which
/// still works for the common case where the two are the same — rather than
/// refusing to serve a peer because BlueZ was momentarily unhelpful.
async fn resolve_peer(
    adapter: &bluer::Adapter,
    addr: Address,
    state: &TransportState,
) -> PeerIdentity {
    if let Some(known) = state.identities.lock().await.get(&addr) {
        return known.clone();
    }
    let mut peer = PeerIdentity::unresolved(addr.to_string(), "public");
    match adapter.device(addr) {
        Ok(device) => {
            match device.remote_address().await {
                Ok(identity) => peer.resolved = Some(identity.to_string()),
                Err(e) => debug!(
                    peer = %addr,
                    error = %e,
                    "Could not read the peer's identity address; using the request address",
                ),
            }
            if let Ok(kind) = device.address_type().await {
                peer.address_type = kind.to_string();
            }
        }
        Err(e) => debug!(
            peer = %addr,
            error = %e,
            "No BlueZ device object for the peer; using the request address",
        ),
    }
    if peer
        .resolved
        .as_deref()
        .is_some_and(|r| !r.eq_ignore_ascii_case(&peer.address))
    {
        info!(
            peer = %peer.address,
            identity = %peer.identity(),
            "Peer connected under a different address than its bond identity",
        );
    }
    state.identities.lock().await.insert(addr, peer.clone());
    peer
}

async fn handle_write(
    peer: &PeerIdentity,
    chunk: Vec<u8>,
    claim: Arc<ClaimMachine>,
    svc: Arc<dyn ManagementService>,
    unbond_tx: &mpsc::Sender<Address>,
    state: &TransportState,
) -> bluer::gatt::local::ReqResult<()> {
    debug!(
        peer = %peer.address,
        chunk_len = chunk.len(),
        "BLE request chunk arrived"
    );

    // One administrator at a time (issue #149). A device can have several
    // bonded phones now, and everything below this point is single-session:
    // one `FrameReader`, one response outbox, one events outbox. Two phones
    // writing at once would interleave frames into the same reassembler and
    // each would read the other's replies out of the same queue — silently, in
    // both directions, because a truncated read is indistinguishable from an
    // idle one.
    //
    // So the newcomer is refused at the GATT layer rather than served badly.
    // `InProgress` reaches the phone as a write failure, which its stack
    // surfaces immediately; pushing an error frame into the response outbox
    // instead would have queued the refusal where the *other* phone reads.
    //
    // Taking the session also resets the reassembler when the owner changes,
    // so a stuck half-frame from the previous holder cannot pollute the next
    // one's first request.
    match state.take_session(peer).await {
        Ok(false) => {}
        Ok(true) => *state.reader.lock().await = FrameReader::new(MAX_FRAME_BYTES),
        Err(holder) => {
            warn!(
                peer = %peer.address,
                holder = %holder,
                idle_secs = SESSION_HANDOVER_IDLE.as_secs(),
                "Refusing a write: another administrator holds the session",
            );
            return Err(bluer::gatt::local::ReqError::InProgress);
        }
    }

    {
        let mut r = state.reader.lock().await;
        r.push(&chunk);
        loop {
            match r.pop_frame() {
                Ok(Some(frame)) => {
                    drop(r);
                    dispatch_frame(peer, &frame, &claim, &svc, unbond_tx, state).await;
                    r = state.reader.lock().await;
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

async fn dispatch_frame(
    peer: &PeerIdentity,
    frame: &[u8],
    claim: &Arc<ClaimMachine>,
    svc: &Arc<dyn ManagementService>,
    unbond_tx: &mpsc::Sender<Address>,
    state: &TransportState,
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
            push_response(&state.response_outbox, &resp).await;
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
        state.response_outbox.clear().await;
        state.events_outbox.clear().await;
    }

    info!(
        peer = %peer.address,
        id, method = %request.method,
        "BLE RPC received"
    );
    let response = match request.method.as_str() {
        // Ungated: this is how a phone *becomes* an administrator, so it
        // cannot require already being one.
        "claim" => handle_claim_rpc(id, request.params, peer, claim).await,
        // Gates itself, because it has to succeed on an unclaimed device too.
        "factory_reset" => handle_factory_reset_rpc(id, peer, claim, unbond_tx).await,
        // The roster methods (issue #149). Handled here rather than through
        // `ManagementService` because they act on the claim machine, which
        // lives in this crate — `shepherd-management` sits *below* it and
        // cannot name it without a dependency cycle.
        method @ ("list_admins"
        | "revoke_admin"
        | "list_enrolment_requests"
        | "approve_enrolment_request"
        | "deny_enrolment_request") => match claim.authorize(peer) {
            AuthDecision::Allow => {
                handle_admin_rpc(id, method, request.params, peer, claim, unbond_tx).await
            }
            AuthDecision::Deny { reason } => denied(id, claim, reason),
        },
        _ => match claim.authorize(peer) {
            AuthDecision::Allow => dispatch_management(svc.as_ref(), request).await,
            AuthDecision::Deny { reason } => denied(id, claim, reason),
        },
    };
    info!(
        peer = %peer.address,
        id,
        ok = response.error.is_none(),
        "BLE RPC response queued"
    );
    push_response(&state.response_outbox, &response).await;
}

/// The refusal an unauthorized caller gets, with the code that says which kind
/// of "no" it is: an unclaimed device has nobody to authorize against, while a
/// claimed one is telling this particular phone it is not an administrator.
fn denied(id: u32, claim: &Arc<ClaimMachine>, reason: String) -> RpcResponse {
    let code = if claim.is_claimed() {
        ErrorCode::PermissionDenied
    } else {
        ErrorCode::NotClaimed
    };
    RpcResponse::err(id, code, reason)
}

/// Map a claim-machine error onto the wire.
fn claim_error(id: u32, e: ClaimError) -> RpcResponse {
    let message = e.to_string();
    let code = match e {
        ClaimError::EnrolmentDenied => ErrorCode::EnrolmentDenied,
        ClaimError::NoSuchRequest | ClaimError::NoSuchAdmin => ErrorCode::NotFound,
        // Revoking the last administrator is refused because it is the wrong
        // operation, not because the caller lacks standing — `factory_reset`
        // is the one that unclaims a device, and it also drops the bond.
        ClaimError::LastAdmin => ErrorCode::Conflict,
        ClaimError::NotClaimed => ErrorCode::NotClaimed,
        ClaimError::PermissionDenied => ErrorCode::PermissionDenied,
        ClaimError::Store(_) => ErrorCode::Internal,
    };
    RpcResponse::err(id, code, message)
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
        Ok(outcome) => match serde_json::to_value(&outcome) {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => RpcResponse::err(id, ErrorCode::Internal, e.to_string()),
        },
        Err(e) => claim_error(id, e),
    }
}

/// The admin-roster RPCs (issue #149), all of which the caller has already
/// been authorized for.
async fn handle_admin_rpc(
    id: u32,
    method: &str,
    params: serde_json::Value,
    peer: &PeerIdentity,
    claim: &Arc<ClaimMachine>,
    unbond_tx: &mpsc::Sender<Address>,
) -> RpcResponse {
    #[derive(serde::Deserialize)]
    struct ById {
        id: String,
    }
    fn by_id(id: u32, params: serde_json::Value) -> Result<String, RpcResponse> {
        serde_json::from_value::<ById>(params)
            .map(|p| p.id)
            .map_err(|e| RpcResponse::err(id, ErrorCode::InvalidParams, e.to_string()))
    }
    fn ok(id: u32, value: impl serde::Serialize) -> RpcResponse {
        match serde_json::to_value(value) {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => RpcResponse::err(id, ErrorCode::Internal, e.to_string()),
        }
    }

    match method {
        "list_admins" => ok(id, claim.list_admins(peer)),
        "list_enrolment_requests" => ok(id, claim.list_requests()),
        "approve_enrolment_request" => match by_id(id, params) {
            Err(resp) => resp,
            Ok(request_id) => match claim.approve_request(&request_id) {
                Ok(summary) => ok(id, summary),
                Err(e) => claim_error(id, e),
            },
        },
        "deny_enrolment_request" => match by_id(id, params) {
            Err(resp) => resp,
            Ok(request_id) => match claim.deny_request(&request_id) {
                Ok(()) => RpcResponse::ok(id, serde_json::Value::Null),
                Err(e) => claim_error(id, e),
            },
        },
        "revoke_admin" => match by_id(id, params) {
            Err(resp) => resp,
            Ok(admin_id) => match claim.revoke_admin(&admin_id) {
                Ok(removed) => {
                    queue_unbond(&removed.identity_address, unbond_tx, "revoke_admin").await;
                    RpcResponse::ok(id, serde_json::Value::Null)
                }
                Err(e) => claim_error(id, e),
            },
        },
        // Unreachable: the caller matched this same list to get here. Answered
        // rather than panicked so a future edit to one list and not the other
        // is a bad response, not a dead daemon.
        other => RpcResponse::err(
            id,
            ErrorCode::MethodNotFound,
            format!("unknown admin method '{other}'"),
        ),
    }
}

/// Ask the adapter-owning task to forget `address`'s bond.
///
/// A revoked or reset admin that stays bonded reaches the link layer on every
/// reconnect and is then refused by `authorize`, which is a lockout re-pairing
/// cannot clear because the bond already exists. The task on the other end
/// records the debt durably before attempting the removal, so a failure here
/// is retried at the next startup.
async fn queue_unbond(address: &str, unbond_tx: &mpsc::Sender<Address>, why: &'static str) {
    match address.parse::<Address>() {
        Ok(addr) => {
            if unbond_tx.send(addr).await.is_err() {
                warn!(
                    peer = %address,
                    why,
                    "unbond channel closed; BlueZ bond not removed",
                );
            }
        }
        Err(e) => warn!(
            peer = %address,
            why,
            error = %e,
            "Could not parse admin identity address; BlueZ bond not removed",
        ),
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
                for record in &previous {
                    queue_unbond(&record.identity_address, unbond_tx, "factory_reset").await;
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
        PeerIdentity::unresolved(address.to_string(), "public".to_string())
    }

    /// A claimed machine that never touches disk: `authorize` reads only
    /// in-memory state, and none of these tests drive a claim/reset that
    /// would persist. Claimed (not Unclaimed) so non-claim RPCs are
    /// allowed through to the service rather than denied with
    /// `not_claimed`.
    fn claimed_machine() -> Arc<ClaimMachine> {
        machine_admin_for(&["AA:BB:CC:DD:EE:FF"])
    }

    /// The same, with a chosen roster — `authorize` compares addresses now
    /// (issue #149), so a test driving the write path has to be an admin at
    /// the address it writes from or every RPC comes back `permission_denied`.
    fn machine_admin_for(addresses: &[&str]) -> Arc<ClaimMachine> {
        let records = addresses
            .iter()
            .map(|a| AdminRecord::new((*a).to_string(), "public".into(), "tester".into()))
            .collect();
        let store = AdminStore::new(Arc::new(shepherd_util::LocalProtectedFiles::new(
            std::path::PathBuf::from("/nonexistent/shepherd-ble-test"),
        )));
        Arc::new(ClaimMachine::new(store, ClaimState::Claimed(records)))
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
        let state = TransportState::new();

        // Bytes the previous BLE session left behind, unread.
        state
            .response_outbox
            .push(encode_frame(b"stale-response"))
            .await;
        state.events_outbox.push(encode_frame(b"stale-event")).await;

        // The first RPC of a fresh session always carries id == 1.
        let body = serde_json::to_vec(&req(1, "health", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_sender(),
            &state,
        )
        .await;

        // Events outbox is wiped and nothing re-queues onto it.
        assert_eq!(state.events_outbox.pending_bytes().await, 0);

        // Response outbox holds exactly the fresh health response — the
        // stale frame was dropped, not stacked in front of it.
        let responses = drain_responses(&state.response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 1);
        assert!(responses[0].error.is_none());
        assert_eq!(*mock.health_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn non_first_rpc_leaves_pending_events_intact() {
        let claim = claimed_machine();
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let state = TransportState::new();

        state
            .events_outbox
            .push(encode_frame(b"pending-event"))
            .await;
        let before = state.events_outbox.pending_bytes().await;

        // id != 1: a mid-session RPC must not disturb events the companion
        // has not yet polled.
        let body = serde_json::to_vec(&req(2, "health", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_sender(),
            &state,
        )
        .await;

        assert_eq!(state.events_outbox.pending_bytes().await, before);
        let responses = drain_responses(&state.response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 2);
    }

    /// A disconnect releases the session, and the next administrator to write
    /// starts on a clean reassembler.
    ///
    /// The partial frame is the point: if A's leftover header bytes survived
    /// the handover they would be read as B's length prefix, and B's first
    /// request would come back as a garbage `id=0` parse error.
    #[tokio::test]
    async fn handover_resets_a_partial_frame() {
        let claim = machine_admin_for(&["AA:AA:AA:AA:AA:AA", "BB:BB:BB:BB:BB:BB"]);
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let state = TransportState::new();

        // Peer A writes only the head of a longer frame, then vanishes.
        let partial = request_frame(7, "health")[..3].to_vec();
        handle_write(
            &peer("AA:AA:AA:AA:AA:AA"),
            partial,
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            &state,
        )
        .await
        .unwrap();
        assert!(
            drain_responses(&state.response_outbox).await.is_empty(),
            "an incomplete frame must not produce a response"
        );

        // BlueZ reports A gone, which is what normally frees the session.
        state.reset_session().await;

        handle_write(
            &peer("BB:BB:BB:BB:BB:BB"),
            request_frame(8, "health"),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            &state,
        )
        .await
        .unwrap();

        let responses = drain_responses(&state.response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 8);
        assert!(responses[0].error.is_none());
    }

    /// Turn-taking (issue #149). Two administrators may both be bonded, but
    /// the transport below is single-session, so the second one to arrive is
    /// refused at the GATT layer rather than being allowed to interleave its
    /// frames into the first one's reassembler.
    #[tokio::test]
    async fn a_second_administrator_is_refused_while_the_first_holds_the_session() {
        let claim = machine_admin_for(&["AA:AA:AA:AA:AA:AA", "BB:BB:BB:BB:BB:BB"]);
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let state = TransportState::new();
        let a = peer("AA:AA:AA:AA:AA:AA");
        let b = peer("BB:BB:BB:BB:BB:BB");

        handle_write(
            &a,
            request_frame(1, "health"),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            &state,
        )
        .await
        .unwrap();

        // B is a perfectly good administrator, and still gets turned away —
        // as a write failure its own stack surfaces, not as an error frame
        // queued where A would read it.
        let refused = handle_write(
            &b,
            request_frame(1, "health"),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            &state,
        )
        .await;
        assert!(matches!(
            refused,
            Err(bluer::gatt::local::ReqError::InProgress)
        ));

        // A's reply is intact and unaccompanied: nothing of B's reached the
        // queue at all.
        let responses = drain_responses(&state.response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 1);
        assert!(responses[0].error.is_none());
    }

    /// The other half of the rule: while A owns the session, B's polls of the
    /// read characteristics come back empty, so it can never carry off a reply
    /// addressed to A.
    #[tokio::test]
    async fn only_the_session_owner_may_drain_the_outboxes() {
        let state = TransportState::new();
        let a: Address = "AA:AA:AA:AA:AA:AA".parse().unwrap();
        let b: Address = "BB:BB:BB:BB:BB:BB".parse().unwrap();

        // Before anyone has written, both may read: the companion drains both
        // outboxes inside `connect()`, before its first RPC.
        assert!(state.may_read(a).await);
        assert!(state.may_read(b).await);

        state
            .take_session(&peer("AA:AA:AA:AA:AA:AA"))
            .await
            .unwrap();
        assert!(state.may_read(a).await);
        assert!(!state.may_read(b).await);

        // A disconnect frees the queues for whoever comes next.
        state.reset_session().await;
        assert!(state.may_read(b).await);
    }

    /// A phone that walked out of range without a clean teardown must not
    /// hold the device against the other parent forever. BlueZ reporting the
    /// disconnect is the normal release; this is the backstop for when that
    /// report never comes.
    #[tokio::test]
    async fn an_idle_owner_loses_the_session() {
        let state = TransportState::new();
        let a = peer("AA:AA:AA:AA:AA:AA");
        let b = peer("BB:BB:BB:BB:BB:BB");

        state.take_session(&a).await.unwrap();
        assert_eq!(
            state.take_session(&b).await.unwrap_err(),
            a.address,
            "a live owner keeps the session, and is named in the refusal",
        );

        // Age the owner past the handover window rather than waiting it out.
        {
            let mut owner = state.last_peer.lock().await;
            let owner = owner.as_mut().expect("A holds the session");
            owner.last_write = owner.last_write - SESSION_HANDOVER_IDLE - Duration::from_secs(1);
        }
        assert!(
            state.take_session(&b).await.unwrap(),
            "the session changed hands, so the reassembler wants resetting",
        );
        assert!(!state.may_read("AA:AA:AA:AA:AA:AA".parse().unwrap()).await);
    }

    /// The owner writing again is not a handover: the reassembler must be
    /// left alone, or every multi-write frame would be truncated.
    #[tokio::test]
    async fn the_owner_keeps_its_reassembler_across_writes() {
        let state = TransportState::new();
        let a = peer("AA:AA:AA:AA:AA:AA");
        assert!(
            state.take_session(&a).await.unwrap(),
            "first write claims it"
        );
        assert!(
            !state.take_session(&a).await.unwrap(),
            "the same peer writing again is the same session",
        );
    }

    #[tokio::test]
    async fn framing_error_drops_state_and_recovers() {
        let claim = machine_admin_for(&["CC:CC:CC:CC:CC:CC"]);
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let state = TransportState::new();
        let p = peer("CC:CC:CC:CC:CC:CC");

        // A length prefix past MAX_FRAME_BYTES is an unrecoverable framing
        // error; the write path must drop the buffered state rather than
        // wedge on it forever.
        let bad = ((MAX_FRAME_BYTES + 1) as u16).to_le_bytes().to_vec();
        handle_write(
            &p,
            bad,
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            &state,
        )
        .await
        .unwrap();
        assert!(drain_responses(&state.response_outbox).await.is_empty());

        // A valid frame from the same peer afterwards still parses — the
        // reader was reset, not left holding the rejected prefix.
        handle_write(
            &p,
            request_frame(2, "health"),
            claim.clone(),
            svc.clone(),
            &unbond_sender(),
            &state,
        )
        .await
        .unwrap();

        let responses = drain_responses(&state.response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 2);
        assert!(responses[0].error.is_none());
    }

    /// With no peer mid-read, a reported disconnect still wipes every
    /// scrap of per-session transport state — both outboxes, the
    /// last-peer marker, and any half-assembled request frame — so a
    /// companion that resumes the same connection across a transient drop
    /// (no fresh `id == 1`) doesn't inherit a desynced byte stream.
    #[tokio::test]
    async fn reset_session_wipes_all_session_state() {
        let state = TransportState::new();
        state
            .take_session(&peer("AA:BB:CC:DD:EE:FF"))
            .await
            .unwrap();

        // Bytes a dropped session left behind: unread outbox frames plus
        // the head of a request frame whose tail never arrived.
        state
            .response_outbox
            .push(encode_frame(b"stale-response"))
            .await;
        state.events_outbox.push(encode_frame(b"stale-event")).await;
        state
            .reader
            .lock()
            .await
            .push(&request_frame(7, "health")[..3]);

        state.reset_session().await;

        assert_eq!(state.response_outbox.pending_bytes().await, 0);
        assert_eq!(state.events_outbox.pending_bytes().await, 0);
        assert!(state.last_peer.lock().await.is_none());

        // The reader kept no leftover prefix: a full frame pushed now
        // parses as itself rather than stitched onto the discarded head.
        let mut r = state.reader.lock().await;
        r.push(&request_frame(8, "health"));
        let frame = r
            .pop_frame()
            .expect("frame parses cleanly")
            .expect("a complete frame is present");
        let parsed: RpcRequest = serde_json::from_slice(&frame).unwrap();
        assert_eq!(parsed.id, 8);
    }

    /// `pin_peer_to_bredr` makes BlueZ report a fresh pairing's
    /// peer disconnected ~600 ms after it connects, while the companion is
    /// mid-drain of the `service_state` it just asked for. Wiping the
    /// outbox there took the rest of that response with it, and a
    /// truncated read looks exactly like an idle one — so the companion
    /// polled an empty characteristic until its RPC timeout and showed an
    /// empty device screen.
    ///
    /// A peer that holds a partial frame keeps its bytes.
    #[tokio::test]
    async fn a_reported_disconnect_spares_a_response_the_peer_is_still_reading() {
        let state = TransportState::new();
        let body = vec![b'x'; 4096];
        state.response_outbox.push(encode_frame(&body)).await;
        let queued = state.response_outbox.pending_bytes().await;

        // The companion has drained one ATT read's worth: the head is now
        // mid-delivery, which is the whole signal that it is still there.
        let first = state.response_outbox.read(512).await;
        assert_eq!(first.len(), 512);

        state.reset_session().await;

        assert_eq!(
            state.response_outbox.pending_bytes().await,
            queued - 512,
            "the rest of the response the peer is reading is still there",
        );

        // And it is still the *same* frame: draining the remainder yields
        // the body byte for byte, so the peer's reassembler stays aligned.
        let mut rest = Vec::new();
        loop {
            let chunk = state.response_outbox.read(512).await;
            if chunk.is_empty() {
                break;
            }
            rest.extend_from_slice(&chunk);
        }
        let whole = [first, rest].concat();
        assert_eq!(whole, encode_frame(&body), "the frame is delivered intact");
    }

    /// The other half of it: once the peer has finished the frame the
    /// outbox is aligned again, so a genuine disconnect clears it. This is
    /// what stops the guard from turning into a leak.
    #[tokio::test]
    async fn a_reported_disconnect_clears_an_outbox_no_one_is_mid_frame_on() {
        let state = TransportState::new();
        state.response_outbox.push(encode_frame(b"first")).await;
        state.response_outbox.push(encode_frame(b"second")).await;

        // Drain the head exactly, leaving the queue aligned on a frame
        // boundary with a whole message still behind it.
        let head = state.response_outbox.read(usize::MAX).await;
        assert_eq!(head, encode_frame(b"first"));

        state.reset_session().await;

        assert_eq!(
            state.response_outbox.pending_bytes().await,
            0,
            "nothing is mid-delivery, so the stale frame goes",
        );
    }

    /// The request side is reset either way, and deliberately so: an
    /// orphaned partial *request* cannot be resynced from this end, and
    /// the next session's writes would stitch onto it.
    #[tokio::test]
    async fn a_reported_disconnect_always_drops_a_half_written_request() {
        let state = TransportState::new();
        // Mid-delivery on the way out, to prove the two directions are
        // decided separately rather than by one shared condition.
        state
            .response_outbox
            .push(encode_frame(&vec![b'x'; 4096]))
            .await;
        let _ = state.response_outbox.read(512).await;
        state
            .reader
            .lock()
            .await
            .push(&request_frame(9, "health")[..3]);

        state.reset_session().await;

        assert!(
            state.response_outbox.pending_bytes().await > 0,
            "the outbound frame is spared",
        );
        let mut r = state.reader.lock().await;
        r.push(&request_frame(10, "health"));
        let frame = r
            .pop_frame()
            .expect("frame parses cleanly")
            .expect("a complete frame is present");
        let parsed: RpcRequest = serde_json::from_slice(&frame).unwrap();
        assert_eq!(parsed.id, 10, "no leftover prefix stitched onto it");
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
    /// A recording sink, to assert the pairing-agent condition is reported in
    /// both directions rather than only raised.
    #[derive(Default)]
    struct RecordingSink {
        events: std::sync::Mutex<Vec<(bool, DiagnosticCode)>>,
    }

    impl DiagnosticSink for RecordingSink {
        fn raise(&self, diagnostic: Diagnostic) {
            self.events.lock().unwrap().push((true, diagnostic.code));
        }
        fn clear(&self, code: DiagnosticCode, _subject: &DiagnosticSubject) {
            self.events.lock().unwrap().push((false, code));
        }
    }

    fn server_with(sink: Arc<RecordingSink>) -> BleServer {
        let dir = tempfile::tempdir().unwrap();
        let config = BleServerConfig {
            device_name: "test".into(),
            firmware_version: "0".into(),
            adapter: None,
            files: Arc::new(shepherd_util::LocalProtectedFiles::new(
                dir.path().to_path_buf(),
            )),
        };
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let display: Arc<dyn PairingDisplay> = Arc::new(NoopPairingDisplay);
        BleServer::new(config, svc, display)
            .unwrap()
            .with_diagnostics(sink as Arc<dyn DiagnosticSink>)
    }

    /// The agent is re-registered when the adapter powers back on, so a failure
    /// that resolves on its own has to clear on its own — otherwise the panel
    /// would show a pairing problem that fixed itself hours ago.
    #[test]
    fn the_pairing_agent_condition_is_reported_in_both_directions() {
        let sink = Arc::new(RecordingSink::default());
        let server = server_with(sink.clone());

        server.report_agent_status(false);
        server.report_agent_status(true);

        assert_eq!(
            *sink.events.lock().unwrap(),
            vec![
                (true, DiagnosticCode::BlePairingAgentUnavailable),
                (false, DiagnosticCode::BlePairingAgentUnavailable),
            ]
        );
    }

    /// Every other caller of `BleServer::new` has no registry, and a missing
    /// sink must not change what the server does.
    #[test]
    fn a_server_without_a_sink_reports_nothing_and_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let config = BleServerConfig {
            device_name: "test".into(),
            firmware_version: "0".into(),
            adapter: None,
            files: Arc::new(shepherd_util::LocalProtectedFiles::new(
                dir.path().to_path_buf(),
            )),
        };
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let display: Arc<dyn PairingDisplay> = Arc::new(NoopPairingDisplay);
        let server = BleServer::new(config, svc, display).unwrap();
        server.report_agent_status(false);
        server.report_agent_status(true);
    }

    #[test]
    fn only_whole_state_snapshots_coalesce() {
        let snapshot = shepherd_api::ServiceStateSnapshot {
            api_version: 1,
            policy_loaded: true,
            current_session: None,
            entry_count: 0,
            entries: vec![],
            internet_status: vec![],
            diagnostics: Default::default(),
            admin_mode: false,
            locked: false,
        };
        assert_eq!(
            coalesce_key_for(&EventPayload::StateChanged(snapshot)),
            Some(COALESCE_STATE_CHANGED),
        );
        // Diagnostics carry the whole set too, so they supersede their
        // predecessors — but under their own key, not the snapshot's: a
        // diagnostics update must not drop a queued state snapshot.
        assert_eq!(
            coalesce_key_for(&EventPayload::DiagnosticsChanged(Default::default())),
            Some(COALESCE_DIAGNOSTICS),
        );
        assert_ne!(COALESCE_DIAGNOSTICS, COALESCE_STATE_CHANGED);

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
        let state = TransportState::new();
        let (unbond_tx, mut unbond_rx) = mpsc::channel::<Address>(4);

        let body = serde_json::to_vec(&req(5, "factory_reset", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_tx,
            &state,
        )
        .await;

        assert!(!claim.is_claimed(), "device is Unclaimed after reset");
        let addr = unbond_rx.try_recv().expect("bond removal was requested");
        assert_eq!(addr.to_string(), "AA:BB:CC:DD:EE:FF");

        // The reset still returns a success response to the peer.
        let responses = drain_responses(&state.response_outbox).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 5);
        assert!(responses[0].error.is_none());
    }

    /// A `factory_reset` on an already-unclaimed device is a no-op success
    /// and must not queue a bond removal (there is no bond to forget).
    #[tokio::test]
    async fn factory_reset_when_unclaimed_requests_no_removal() {
        let store = AdminStore::new(Arc::new(shepherd_util::LocalProtectedFiles::new(
            std::path::PathBuf::from("/nonexistent/shepherd-ble-test"),
        )));
        let claim: Arc<ClaimMachine> = Arc::new(ClaimMachine::new(store, ClaimState::Unclaimed));
        let svc: Arc<dyn ManagementService> = Arc::new(MockSvc::new());
        let state = TransportState::new();
        let (unbond_tx, mut unbond_rx) = mpsc::channel::<Address>(4);

        let body = serde_json::to_vec(&req(1, "factory_reset", serde_json::Value::Null)).unwrap();
        dispatch_frame(
            &peer("AA:BB:CC:DD:EE:FF"),
            &body,
            &claim,
            &svc,
            &unbond_tx,
            &state,
        )
        .await;

        assert!(matches!(
            unbond_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let responses = drain_responses(&state.response_outbox).await;
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
        let files: Arc<dyn shepherd_util::ProtectedFiles> = Arc::new(
            shepherd_util::LocalProtectedFiles::new(dir.path().to_path_buf()),
        );
        let admin_path = dir
            .path()
            .join(shepherd_util::ProtectedFile::AdminRecord.file_name());
        let sentinel_path = dir
            .path()
            .join(shepherd_util::ProtectedFile::ResetSentinel.file_name());

        let store = AdminStore::new(Arc::clone(&files));
        store
            .save_all(&[AdminRecord::new(
                "AA:BB:CC:DD:EE:FF".into(),
                "public".into(),
                "phone".into(),
            )])
            .unwrap();
        std::fs::write(&sentinel_path, "").unwrap();

        let config = BleServerConfig {
            device_name: "shepherd".into(),
            firmware_version: "test".into(),
            adapter: None,
            files: Arc::clone(&files),
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
        let next_boot = PendingUnbondStore::new(Arc::clone(&files));
        assert_eq!(next_boot.list().unwrap(), queued);
    }
}
