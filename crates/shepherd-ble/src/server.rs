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
//! polling simply accumulate (bounded by the outbox capacity); the
//! companion re-syncs current state by calling `service_state` on
//! every reconnect.
//!
//! v1 still assumes a single bonded admin peer at a time (TOFU
//! single-admin model from the design doc).

use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::agent::AgentHandle;
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteMethod, Service,
};
use futures_util::FutureExt;
use shepherd_management::ManagementService;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tracing::{debug, error, info, warn};

use crate::admin::{AdminStore, check_reset_sentinel};
use crate::agent::{PairingDisplay, build_agent};
use crate::claim::{AuthDecision, ClaimMachine, PeerIdentity};
use crate::framing::{FrameReader, encode_frame};
use crate::outbox::Outbox;
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

/// Cap on the Events outbox. Sized to comfortably hold ~10 full
/// `StateChanged` snapshots (each up to ~20 KiB once a few entries are
/// configured) plus the smaller session/policy events that fire around
/// them. Stale events that overrun this when no companion is polling
/// are dropped from the front; the companion re-fetches current state
/// via `service_state` on every reconnect.
const EVENTS_OUTBOX_BYTES: usize = 256 * 1024;

/// Bluetooth Core spec ceiling on a single GATT attribute value.
/// Android's stack enforces this strictly: even with an ATT MTU of
/// 517 (allowing 516-byte read responses on the wire), a single
/// `BluetoothGatt.readCharacteristic` call delivers at most 512 bytes
/// to the application, silently truncating the rest. We cap the
/// outbox read chunk here so each ATT response fits inside what the
/// client will actually receive.
const GATT_MAX_ATTR_VALUE: usize = 512;

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
    /// 6. Spawn the events forwarder task.
    /// 7. Wait for shutdown.
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

        let response_outbox = Arc::new(Outbox::new("response", RESPONSE_OUTBOX_BYTES));
        let events_outbox = Arc::new(Outbox::new("events", EVENTS_OUTBOX_BYTES));

        let application = build_application(
            self.config.clone(),
            self.svc.clone(),
            self.claim.clone(),
            response_outbox.clone(),
            events_outbox.clone(),
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

        let events_task = tokio::spawn(events_forwarder(self.svc.clone(), events_outbox.clone()));

        let _ = shutdown_rx.wait_for(|v| *v).await;
        info!("BLE management server shutting down");
        events_task.abort();
        let _ = events_task.await;
        Ok(())
    }
}

/// Forward every event from [`ManagementService::subscribe_events`]
/// into the events outbox for the entire server lifetime. Lagged
/// events are logged and skipped — the companion re-syncs current
/// state via `service_state` on reconnect, so a missed event is
/// recoverable.
async fn events_forwarder(svc: Arc<dyn ManagementService>, outbox: Arc<Outbox>) {
    // The outer `run` aborts this task on shutdown via JoinHandle::abort,
    // so we don't need an explicit shutdown signal here — keeping the
    // loop free of `watch::Ref` (which isn't Send) keeps tokio::spawn
    // happy.
    let mut rx = svc.subscribe_events();
    loop {
        match rx.recv().await {
            Ok(event) => match serde_json::to_vec(&event) {
                Ok(payload) => outbox.push(encode_frame(&payload)).await,
                Err(e) => warn!(error = %e, "Failed to serialize Event"),
            },
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
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
) -> Application {
    Application {
        services: vec![Service {
            uuid: SHEPHERD_MANAGEMENT_SERVICE_UUID,
            primary: true,
            characteristics: vec![
                device_info_characteristic(config, claim.clone()),
                request_characteristic(svc, claim, response_outbox.clone(), events_outbox.clone()),
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
fn request_characteristic(
    svc: Arc<dyn ManagementService>,
    claim: Arc<ClaimMachine>,
    response_outbox: Arc<Outbox>,
    events_outbox: Arc<Outbox>,
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
                let response_outbox = response_outbox.clone();
                let events_outbox = events_outbox.clone();
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
                    dispatch_frame(peer, &frame, &claim, &svc, &response_outbox, &events_outbox)
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

async fn dispatch_frame(
    peer: &PeerIdentity,
    frame: &[u8],
    claim: &Arc<ClaimMachine>,
    svc: &Arc<dyn ManagementService>,
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
