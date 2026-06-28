//! `BleServer`: lifecycle for the GATT application, advertising, and
//! the pairing agent.
//!
//! Notification channels:
//!
//! - **Response** uses a broadcast channel held centrally;
//!   `dispatch_frame` writes the response there, and every notify
//!   subscriber takes its own [`broadcast::Receiver`] via
//!   `.subscribe()`. This means a reconnecting companion app always
//!   gets a working channel — the older mpsc-with-take-once design
//!   silently broke after the first disconnect.
//! - **Events** subscribes per-callback directly to
//!   [`ManagementService::subscribe_events`], so there's no central
//!   pump. Each new subscriber receives a fresh
//!   [`shepherd_api::Event::StateChanged`] snapshot before entering
//!   the live stream, so a freshly-reopened companion doesn't have
//!   to send a separate `service_state` RPC just to populate its UI.
//!
//! v1 still assumes a single bonded admin peer at a time (TOFU
//! single-admin model from the design doc), but the broadcast-based
//! channels mean multiple concurrent subscribers would work too if
//! we ever lift that assumption.

use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::agent::AgentHandle;
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod,
    Service,
};
use futures_util::FutureExt;
use shepherd_api::{Event, EventPayload};
use shepherd_management::ManagementService;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, watch};
use tracing::{debug, error, info, warn};

use crate::admin::{AdminStore, check_reset_sentinel};
use crate::agent::{PairingDisplay, build_agent};
use crate::claim::{AuthDecision, ClaimMachine, PeerIdentity};
use crate::framing::{FrameReader, chunk_payload};
use crate::protocol::{
    ClaimStateTag, DeviceInfo, ErrorCode, MAX_FRAME_BYTES, PROTOCOL_VERSION, RpcRequest,
    RpcResponse, SHEPHERD_DEVICE_INFO_CHAR_UUID, SHEPHERD_EVENTS_CHAR_UUID,
    SHEPHERD_MANAGEMENT_SERVICE_UUID, SHEPHERD_REQUEST_CHAR_UUID, SHEPHERD_RESPONSE_CHAR_UUID,
};
use crate::rpc::dispatch_management;

/// Depth of the Response broadcast channel. Small because per-subscriber
/// queues are independent (each holds its own backlog); a depth this
/// shallow only matters if the phone is dramatically slower than the
/// daemon for a sustained burst.
const NOTIFY_QUEUE_DEPTH: usize = 32;

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
}

pub struct BleServer {
    config: BleServerConfig,
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    display: Arc<dyn PairingDisplay>,
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

        if check_reset_sentinel(&config.reset_sentinel_path) {
            warn!(
                sentinel = %config.reset_sentinel_path.display(),
                "Factory-reset sentinel present at startup; clearing admin record",
            );
            store.clear()?;
        }

        let claim = Arc::new(ClaimMachine::load(store)?);
        Ok(Self {
            config,
            svc,
            claim,
            display,
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
    /// 6. Wait for shutdown.
    pub async fn run(self, mut shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<()> {
        let session = bluer::Session::new().await.map_err(|e| {
            anyhow::anyhow!(
                "BlueZ D-Bus session unavailable ({e}); is bluetoothd running and reachable on the system bus?"
            )
        })?;
        let adapter = session.default_adapter().await.map_err(|e| {
            anyhow::anyhow!(
                "No default Bluetooth adapter ({e}); check `bluetoothctl show` and that an HCI controller is attached"
            )
        })?;
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

        // Best-effort bond removal after a sentinel-triggered reset.
        // We tolerate failures: the next restart will retry, and the
        // admin record itself is already gone.
        if let Some(prev) = persisted_admin_for_unbond(&self.claim).await
            && let Err(e) = adapter.remove_device(prev.identity_address.parse()?).await
        {
            warn!(error = %e, "BlueZ bond removal after reset failed (best-effort)");
        }

        let _agent_handle = register_agent(&session, self.display.clone())
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to register BlueZ pairing agent ({e}); without this, pairing falls back to a PIN entry on the phone and no Numeric Comparison overlay appears on the TV. Likely cause: the daemon user lacks the `bluetooth` group."
                )
            })?;
        // Broadcast channel for RPC responses so re-subscribers (the
        // phone reopening the companion app) always get a fresh
        // receiver rather than the dead one from the previous session.
        let (response_tx, _) = broadcast::channel::<Vec<u8>>(NOTIFY_QUEUE_DEPTH);

        let application = build_application(
            self.config.clone(),
            self.svc.clone(),
            self.claim.clone(),
            response_tx.clone(),
        );

        let _app_handle: ApplicationHandle = adapter.serve_gatt_application(application).await?;

        let _adv_handle: AdvertisementHandle = adapter
            .advertise(Advertisement {
                advertisement_type: bluer::adv::Type::Peripheral,
                service_uuids: [SHEPHERD_MANAGEMENT_SERVICE_UUID].into_iter().collect(),
                local_name: Some(self.config.device_name.clone()),
                discoverable: Some(true),
                ..Default::default()
            })
            .await?;
        info!(
            device = %self.config.device_name,
            service = %SHEPHERD_MANAGEMENT_SERVICE_UUID,
            "BLE management advertising started",
        );

        let _ = shutdown_rx.wait_for(|v| *v).await;
        info!("BLE management server shutting down");
        Ok(())
    }
}

/// Returns the admin record we just cleared (and therefore want BlueZ
/// to forget). Currently always `None` because `check_reset_sentinel`
/// runs in `new` and we don't thread the previous record through —
/// reserved for when we do.
async fn persisted_admin_for_unbond(
    _claim: &Arc<ClaimMachine>,
) -> Option<crate::admin::AdminRecord> {
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
    config: BleServerConfig,
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    response_tx: broadcast::Sender<Vec<u8>>,
) -> Application {
    Application {
        services: vec![Service {
            uuid: SHEPHERD_MANAGEMENT_SERVICE_UUID,
            primary: true,
            characteristics: vec![
                device_info_characteristic(config, claim.clone()),
                request_characteristic(svc.clone(), claim, response_tx.clone()),
                response_characteristic(response_tx),
                events_characteristic(svc),
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
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    response_tx: broadcast::Sender<Vec<u8>>,
) -> Characteristic {
    // Per-device frame reassembly buffer. v1 holds a single reader
    // because we only expect one admin connection at a time; if a
    // second device writes here we wipe the buffer and start fresh.
    let reader: Arc<Mutex<FrameReader>> = Arc::new(Mutex::new(FrameReader::new(MAX_FRAME_BYTES)));
    let last_peer: Arc<Mutex<Option<PeerIdentity>>> = Arc::new(Mutex::new(None));

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
                let response_tx = response_tx.clone();
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
                    handle_write(&peer, chunk, reader, last_peer, claim, svc, response_tx).await
                }
                .boxed()
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn handle_write(
    peer: &PeerIdentity,
    chunk: Vec<u8>,
    reader: Arc<Mutex<FrameReader>>,
    last_peer: Arc<Mutex<Option<PeerIdentity>>>,
    claim: Arc<ClaimMachine>,
    svc: Arc<dyn ManagementService>,
    response_tx: broadcast::Sender<Vec<u8>>,
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
                    dispatch_frame(peer, &frame, &claim, &svc, &response_tx).await;
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

async fn dispatch_frame(
    peer: &PeerIdentity,
    frame: &[u8],
    claim: &Arc<ClaimMachine>,
    svc: &Arc<dyn ManagementService>,
    response_tx: &broadcast::Sender<Vec<u8>>,
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
            push_response(response_tx, &resp).await;
            return;
        }
    };

    let id = request.id;
    info!(
        peer = %peer.address,
        id, method = %request.method,
        "BLE RPC received"
    );
    let response = match request.method.as_str() {
        "claim" => handle_claim_rpc(id, request.params, peer, claim).await,
        "factory_reset" => handle_factory_reset_rpc(id, peer, claim).await,
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
    push_response(response_tx, &response).await;
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
) -> RpcResponse {
    // factory_reset is admin-gated: only the current admin (or no admin,
    // in which case it's a no-op) may invoke it.
    match claim.authorize(peer) {
        AuthDecision::Allow => match claim.factory_reset() {
            Ok(_) => RpcResponse::ok(id, serde_json::Value::Null),
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

async fn push_response(tx: &broadcast::Sender<Vec<u8>>, resp: &RpcResponse) {
    let bytes = match serde_json::to_vec(resp) {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "Failed to serialize RpcResponse");
            return;
        }
    };
    // broadcast::send returns Err when there are no active subscribers.
    // That's an expected condition if a request slipped in before the
    // companion subscribed to Response — we log and move on rather
    // than treat it as a fatal channel close.
    if let Err(e) = tx.send(bytes) {
        debug!(error = %e, "No Response subscribers; response dropped");
    }
}

/// `Response`: server notifies length-prefixed JSON-RPC responses.
///
/// Each subscribe takes its own [`broadcast::Receiver`] off the
/// shared `response_tx`, so a companion that disconnects and
/// reconnects always lands on a fresh receiver — the prior design's
/// one-shot mpsc receiver-take is gone.
fn response_characteristic(response_tx: broadcast::Sender<Vec<u8>>) -> Characteristic {
    Characteristic {
        uuid: SHEPHERD_RESPONSE_CHAR_UUID,
        notify: Some(CharacteristicNotify {
            notify: true,
            method: CharacteristicNotifyMethod::Fun(Box::new(move |mut notifier| {
                let response_tx = response_tx.clone();
                async move {
                    info!("BLE Response notify subscribed");
                    let mut rx = response_tx.subscribe();
                    let chunk_size = negotiated_chunk_size(&notifier);
                    loop {
                        // Race rx.recv() against the notifier's stopped
                        // future so we exit the loop the moment the
                        // peer drops its subscription (link disconnect,
                        // CCCD-disable). Without this, the loop keeps
                        // consuming responses and sending them into a
                        // dead notifier — every later reconnect from
                        // the same peer ends up with no working
                        // Response path.
                        tokio::select! {
                            _ = notifier.stopped() => {
                                info!("BLE Response notify session ended");
                                return;
                            }
                            recv = rx.recv() => match recv {
                                Ok(payload) => {
                                    info!(
                                        payload_len = payload.len(),
                                        chunk_size, "BLE Response notify sending payload"
                                    );
                                    if !send_chunks(&mut notifier, &payload, chunk_size).await {
                                        return;
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    warn!(
                                        skipped = n,
                                        "BLE Response subscriber lagged; some responses dropped"
                                    );
                                }
                                Err(broadcast::error::RecvError::Closed) => {
                                    debug!("Response broadcast closed");
                                    return;
                                }
                            }
                        }
                    }
                }
                .boxed()
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `Events`: server notifies serialized `shepherd_api::Event` JSON.
///
/// Each subscribe gets its own [`broadcast::Receiver`] from
/// [`ManagementService::subscribe_events`] and also receives an
/// initial `StateChanged` snapshot before entering the live stream.
/// This makes a freshly-reopened companion's UI populate without
/// needing a separate `service_state` RPC.
fn events_characteristic(svc: Arc<dyn ManagementService>) -> Characteristic {
    Characteristic {
        uuid: SHEPHERD_EVENTS_CHAR_UUID,
        notify: Some(CharacteristicNotify {
            notify: true,
            method: CharacteristicNotifyMethod::Fun(Box::new(move |mut notifier| {
                let svc = svc.clone();
                async move {
                    info!("BLE Events notify subscribed");
                    let mut rx = svc.subscribe_events();
                    let chunk_size = negotiated_chunk_size(&notifier);

                    // Initial-state push: synthesise a StateChanged event
                    // from the current snapshot so a freshly-connected
                    // companion can populate its UI immediately.
                    let snap = svc.service_state().await;
                    let initial = Event::new(EventPayload::StateChanged(snap));
                    match serde_json::to_vec(&initial) {
                        Ok(payload) => {
                            if !send_chunks(&mut notifier, &payload, chunk_size).await {
                                return;
                            }
                        }
                        Err(e) => warn!(error = %e, "Failed to serialize initial StateChanged"),
                    }

                    loop {
                        // Same stopped()-race rationale as response_characteristic:
                        // exit promptly on peer disconnect so we don't pin a
                        // broadcast subscriber across BLE sessions.
                        tokio::select! {
                            _ = notifier.stopped() => {
                                info!("BLE Events notify session ended");
                                return;
                            }
                            recv = rx.recv() => match recv {
                                Ok(event) => match serde_json::to_vec(&event) {
                                    Ok(payload) => {
                                        if !send_chunks(&mut notifier, &payload, chunk_size).await {
                                            return;
                                        }
                                    }
                                    Err(e) => warn!(error = %e, "Failed to serialize Event"),
                                },
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    warn!(skipped = n, "BLE Events subscriber lagged");
                                }
                                Err(broadcast::error::RecvError::Closed) => {
                                    debug!("Event broadcast channel closed");
                                    return;
                                }
                            }
                        }
                    }
                }
                .boxed()
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Fragment a length-prefixed payload across notifications. Returns
/// `false` if the notifier rejected a fragment (link dropped) so the
/// caller can exit its loop.
async fn send_chunks(
    notifier: &mut bluer::gatt::local::CharacteristicNotifier,
    payload: &[u8],
    chunk_size: usize,
) -> bool {
    for fragment in chunk_payload(payload, chunk_size) {
        if let Err(e) = notifier.notify(fragment).await {
            debug!(error = %e, "BLE notifier ended");
            return false;
        }
    }
    true
}

/// Best-guess fragment size for outbound notifications. bluer's
/// `CharacteristicNotifier` doesn't expose the negotiated ATT MTU on
/// the local end; we use a conservative default that matches the BLE
/// 4.0 23-byte MTU minus the 3-byte ATT header. Real installs
/// negotiate higher MTUs (often 247 or 517) — the worst case here is
/// extra fragmentation overhead, not a correctness issue.
fn negotiated_chunk_size(_notifier: &bluer::gatt::local::CharacteristicNotifier) -> usize {
    20
}
