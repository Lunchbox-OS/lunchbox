//! `ClaimMachine`: the admin roster, the enrolment handshake a second phone
//! goes through, and the per-request authorization gate.
//!
//! The first phone to reach an unclaimed device becomes its admin on the spot
//! — Trust On First Use, because there is nobody yet to ask. Every phone after
//! that has to be let in by one that is already an admin (issue #149): it
//! bonds, calls `claim`, and gets back a six-digit code and a pending request
//! the existing admin sees and approves.
//!
//! **Why not TOFU for everyone.** The anchor for a BLE pairing is physical
//! presence — the digits are on the TV, so whoever pairs is standing in front
//! of it. That is a fine anchor for the first phone, which arrives before the
//! child has met the device, and a useless one afterwards: the child is
//! standing in front of the TV more than anyone. So the second bond is gated
//! on somebody who is already trusted, which is the same shape #156 built for
//! letting a browser in, for the same reason.

use crate::admin::{AdminRecord, AdminStore, AdminStoreError};
use bluer::Address;
use rand::RngCore;
use lunchbox_management::{
    AdminAuthority, AdminRoster, AdminRosterError, AdminSummary, EnrolmentRequestInfo,
};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use chrono::{DateTime, Duration as ChronoDuration, Local};
use serde::{Deserialize, Serialize};

/// How long a pending enrolment stays approvable.
///
/// Longer than #156's two minutes for a browser, because the ritual is
/// different: there, one person holds the phone and the laptop. Here the
/// second phone's owner has to find the first phone's owner, who may be in
/// another room or another house. Five minutes is a walk, not a wait.
pub const ENROLMENT_REQUEST_TTL: Duration = Duration::from_secs(300);

/// Who a GATT request came from, by both of the addresses BlueZ has for them.
///
/// **These are genuinely two different addresses, and the difference decides
/// whether an administrator can reconnect.** A GATT request reports the peer by
/// the address its D-Bus object path was created under, which for a phone using
/// privacy is the random address it happened to advertise with when the link
/// came up. `Device1.Address` on that same object reports the *identity*
/// address once pairing has completed — the one BlueZ files the bond under and
/// resolves later random addresses to.
///
/// Measured on the bench (2026-09-07): a phone claiming a device mid-pairing
/// arrived at path `79:C2:1B:08:F0:32` while its bond was already being written
/// to `64:11:A4:B0:7B:D9`. Recording the path address would have written a
/// record that phone could never match again, because its next connection
/// arrives under the identity. This is the drift the old single-admin
/// `authorize` chose to ignore rather than resolve; ignoring it stops being an
/// option once the gate actually compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The address the request arrived under — a D-Bus object path, so
    /// possibly a random address that will not be seen again.
    pub address: String,
    /// `Device1.Address`, when it could be read. The stable one.
    pub resolved: Option<String>,
    /// `"public"` or `"random"`.
    pub address_type: String,
}

impl PeerIdentity {
    /// Construct from a request address alone, with no resolution — what the
    /// server falls back to when BlueZ cannot be asked.
    pub fn unresolved(address: impl Into<String>, address_type: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            resolved: None,
            address_type: address_type.into(),
        }
    }

    /// The address to *record*: the stable one when we have it.
    pub fn identity(&self) -> &str {
        self.resolved.as_deref().unwrap_or(&self.address)
    }

    /// Whether this peer is the one `record` names.
    ///
    /// Either address counts. A record written by this build holds the
    /// resolved identity, but one migrated from before it holds whatever
    /// address the claim arrived under — and for a phone whose object path
    /// already *was* its identity (a dual-mode phone reached over its LE
    /// identity address, which is the common case) those are different
    /// strings for the same phone. Accepting both is what lets an
    /// administrator that predates this change keep working without re-pairing.
    ///
    /// It does not widen anything: both addresses come from BlueZ, about the
    /// same bonded peer, and neither is anything the peer chooses.
    pub fn matches(&self, record: &AdminRecord) -> bool {
        self.address.eq_ignore_ascii_case(&record.identity_address)
            || self
                .resolved
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(&record.identity_address))
    }

    /// Whether two sightings are the same peer, by the stable address.
    fn is(&self, other: &PeerIdentity) -> bool {
        self.identity().eq_ignore_ascii_case(other.identity())
    }
}

/// What `claim` did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClaimOutcome {
    /// The caller is an admin: freshly enrolled, or already one and asking
    /// again. Carries the record, token and all — this is the one moment the
    /// token crosses the wire.
    Claimed { admin: AdminRecord },
    /// The device already has admins and this phone is not one. It has to be
    /// approved from a phone that is; poll `claim` again to find out.
    Pending { request: EnrolmentRequestInfo },
}

#[derive(Debug, Clone)]
pub enum ClaimState {
    Unclaimed,
    Claimed(Vec<AdminRecord>),
}

impl ClaimState {
    pub fn is_claimed(&self) -> bool {
        matches!(self, ClaimState::Claimed(admins) if !admins.is_empty())
    }
}

/// Result of the authorization check applied to every RPC except the
/// claim-flow methods. RPC dispatchers should call
/// [`ClaimMachine::authorize`] before invoking `ManagementService`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthDecision {
    Allow,
    Deny { reason: String },
}

#[derive(Debug, Error)]
pub enum ClaimError {
    #[error("device must be claimed before any other operation")]
    NotClaimed,
    #[error("peer is not the admin")]
    PermissionDenied,
    #[error("an administrator turned this request down")]
    EnrolmentDenied,
    #[error("no such enrolment request, or it expired")]
    NoSuchRequest,
    #[error("no such administrator")]
    NoSuchAdmin,
    #[error("this is the only administrator; factory-reset the device instead")]
    LastAdmin,
    #[error("admin store error: {0}")]
    Store(#[from] AdminStoreError),
}

/// A pending enrolment. In memory only, like #156's login requests and for the
/// same reason: it is a thing a human is looking at right now, and one that
/// outlived a daemon restart would be a live approval nobody is watching.
struct PendingEnrolment {
    id: String,
    code: String,
    device_name: String,
    peer: PeerIdentity,
    requested_at: DateTime<Local>,
    expires: Instant,
    /// Set by `deny`, cleared by the requester's next `claim` — which is how
    /// the phone learns it was turned down instead of waiting out the TTL.
    denied: bool,
}

struct Inner {
    admins: Vec<AdminRecord>,
    pending: Vec<PendingEnrolment>,
}

/// State machine uses [`std::sync::RwLock`] so that synchronous
/// consumers (notably the HTTP auth middleware, via
/// [`AdminAuthority`]) can check tokens without blocking on an async
/// lock. All operations hold the lock only for brief in-memory updates
/// plus an atomic file write on the paths that change the roster.
pub struct ClaimMachine {
    state: RwLock<Inner>,
    store: AdminStore,
    /// Where to report a bond this device no longer wants.
    ///
    /// The machine that drops an administrator's record is the one that owes
    /// the bond removal, so it holds the channel rather than making every
    /// caller remember. Unbounded and therefore *synchronously* sendable,
    /// which is what lets [`AdminRoster`] — reached from the HTTP middleware
    /// as well as from BLE — stay a synchronous trait.
    ///
    /// `None` until the server has an adapter to remove bonds with, and in
    /// every test and tool that has none. A revocation still succeeds then;
    /// the debt simply goes unrecorded, which is why the *server* also writes
    /// it to [`crate::admin::PendingUnbondStore`] before attempting it.
    unbond: RwLock<Option<mpsc::UnboundedSender<Address>>>,
}

impl ClaimMachine {
    /// Construct a machine seeded with `initial`. The `store` is retained so
    /// subsequent roster changes persist immediately.
    pub fn new(store: AdminStore, initial: ClaimState) -> Self {
        let admins = match initial {
            ClaimState::Unclaimed => Vec::new(),
            ClaimState::Claimed(admins) => admins,
        };
        Self {
            state: RwLock::new(Inner {
                admins,
                pending: Vec::new(),
            }),
            store,
            unbond: RwLock::new(None),
        }
    }

    /// Load the roster from disk and wrap it in a machine.
    ///
    /// Migrates a v1 single-`[admin]` file in place: the rewrite happens here,
    /// at startup, rather than being left to whichever later operation happens
    /// to save first — a device that is upgraded and then never re-claimed
    /// would otherwise keep the old shape indefinitely, and every load would
    /// pay for the guesswork again.
    pub fn load(store: AdminStore) -> Result<Self, ClaimError> {
        let stored = store.load()?;
        if stored.needs_migration {
            info!(
                admins = stored.admins.len(),
                "Migrating the admin record from the single-admin format",
            );
            if let Err(e) = store.save_all(&stored.admins) {
                // Not fatal: the in-memory roster is right either way, and the
                // next roster change writes the new shape. Losing the device's
                // only admin over a failed rewrite would be far worse.
                warn!(error = %e, "Could not rewrite the admin record; it stays in the old format");
            }
        }
        Ok(Self::new(store, ClaimState::Claimed(stored.admins)))
    }

    /// Tell the machine where to send bonds that need forgetting.
    ///
    /// Set by the BLE server once it has an adapter. Separate from
    /// construction because the claim machine exists before the channel does —
    /// the HTTP layer is handed this same machine as an [`AdminRoster`] at
    /// startup, and it must work whether or not BLE ever came up.
    pub fn set_unbond_sender(&self, tx: mpsc::UnboundedSender<Address>) {
        *self.unbond.write().expect("unbond lock poisoned") = Some(tx);
    }

    /// Ask for `address`'s bond to be forgotten, if anyone is listening.
    fn request_unbond(&self, address: &str) {
        let Some(tx) = self.unbond.read().expect("unbond lock poisoned").clone() else {
            debug!(
                peer = %address,
                "No unbond channel; the BlueZ bond stays until the next startup drain",
            );
            return;
        };
        match address.parse::<Address>() {
            Ok(addr) => {
                if tx.send(addr).is_err() {
                    warn!(peer = %address, "Unbond channel closed; BlueZ bond not removed");
                }
            }
            Err(e) => warn!(
                peer = %address,
                error = %e,
                "Could not parse an admin identity address; BlueZ bond not removed",
            ),
        }
    }

    pub fn snapshot(&self) -> ClaimState {
        let inner = self.state.read().expect("claim state lock poisoned");
        if inner.admins.is_empty() {
            ClaimState::Unclaimed
        } else {
            ClaimState::Claimed(inner.admins.clone())
        }
    }

    pub fn is_claimed(&self) -> bool {
        !self
            .state
            .read()
            .expect("claim state lock poisoned")
            .admins
            .is_empty()
    }

    /// Ask to administer this device.
    ///
    /// Three things happen here depending on who is asking:
    ///
    /// - **Nobody is an admin yet** — the caller becomes one. TOFU, because
    ///   there is nobody to ask.
    /// - **The caller is already an admin** — idempotent; the existing record
    ///   comes back, token and all, so a phone that lost its local copy can
    ///   recover it over a link only its own bond can open. The device name is
    ///   *not* overwritten, matching the single-admin behaviour.
    /// - **Anyone else** — a pending request, which an existing admin has to
    ///   approve. Calling again returns the same request rather than making a
    ///   second one, so the phone polls this to find out what happened.
    pub fn claim(
        &self,
        peer: PeerIdentity,
        device_name: String,
    ) -> Result<ClaimOutcome, ClaimError> {
        let mut inner = self.state.write().expect("claim state lock poisoned");
        sweep_pending(&mut inner);

        if let Some(existing) = inner.admins.iter().find(|a| peer.matches(a)) {
            return Ok(ClaimOutcome::Claimed {
                admin: existing.clone(),
            });
        }

        if inner.admins.is_empty() {
            let record = AdminRecord::new(
                peer.identity().to_string(),
                peer.address_type.clone(),
                device_name,
            );
            inner.admins.push(record.clone());
            let admins = inner.admins.clone();
            self.store.save_all(&admins)?;
            info!(
                identity = %record.identity_address,
                device = %record.device_name,
                "First admin claimed the device",
            );
            return Ok(ClaimOutcome::Claimed { admin: record });
        }

        // A request already in flight for this peer. Denial is delivered here
        // and consumed, so the phone gets told once and a retry starts over
        // rather than reporting the old refusal forever.
        if let Some(idx) = inner.pending.iter().position(|r| r.peer.is(&peer)) {
            if inner.pending[idx].denied {
                inner.pending.remove(idx);
                return Err(ClaimError::EnrolmentDenied);
            }
            return Ok(ClaimOutcome::Pending {
                request: info_for(&inner.pending[idx]),
            });
        }

        let requested_at = lunchbox_util::now();
        let request = PendingEnrolment {
            id: crate::admin::new_admin_id(),
            code: numeric_code(),
            device_name,
            peer,
            requested_at,
            expires: Instant::now() + ENROLMENT_REQUEST_TTL,
            denied: false,
        };
        let info = info_for(&request);
        info!(
            peer = %request.peer.identity(),
            device = %request.device_name,
            "A second phone asked to administer this device; awaiting approval",
        );
        inner.pending.push(request);
        Ok(ClaimOutcome::Pending { request: info })
    }

    /// Every phone waiting on a tap, for whoever can provide one.
    fn requests(&self) -> Vec<EnrolmentRequestInfo> {
        let mut inner = self.state.write().expect("claim state lock poisoned");
        sweep_pending(&mut inner);
        inner
            .pending
            .iter()
            .filter(|r| !r.denied)
            .map(info_for)
            .collect()
    }

    /// Let a waiting phone in, minting its own record and token.
    ///
    /// The token is *not* returned here. The approving phone has no use for
    /// another phone's credential, and the requester collects its own by
    /// calling `claim` again over its own bond.
    fn approve(&self, id: &str) -> Result<AdminSummary, ClaimError> {
        let mut inner = self.state.write().expect("claim state lock poisoned");
        sweep_pending(&mut inner);
        let Some(idx) = inner.pending.iter().position(|r| r.id == id && !r.denied) else {
            return Err(ClaimError::NoSuchRequest);
        };
        let request = inner.pending.remove(idx);
        let record = AdminRecord::new(
            request.peer.identity().to_string(),
            request.peer.address_type.clone(),
            request.device_name.clone(),
        );
        inner.admins.push(record.clone());
        let admins = inner.admins.clone();
        self.store.save_all(&admins)?;
        info!(
            identity = %record.identity_address,
            device = %record.device_name,
            "Enrolment approved; the phone is now an administrator",
        );
        Ok(record.summary())
    }

    /// Turn a waiting phone away. The refusal is held rather than dropped so
    /// the requester's next poll is told, instead of silently timing out.
    fn deny(&self, id: &str) -> Result<(), ClaimError> {
        let mut inner = self.state.write().expect("claim state lock poisoned");
        let Some(request) = inner.pending.iter_mut().find(|r| r.id == id && !r.denied) else {
            return Err(ClaimError::NoSuchRequest);
        };
        request.denied = true;
        warn!(peer = %request.peer.identity(), "Enrolment denied");
        Ok(())
    }

    /// The roster, credential-free.
    fn roster(&self) -> Vec<AdminSummary> {
        self.state
            .read()
            .expect("claim state lock poisoned")
            .admins
            .iter()
            .map(AdminRecord::summary)
            .collect()
    }

    /// Remove one admin, returning the record so the caller can ask BlueZ to
    /// forget its bond.
    ///
    /// Refuses to remove the last one. That is not the same operation: it
    /// would leave a device with no administrator and a phone still bonded to
    /// it, which is the asymmetric-bond lockout the unbond queue exists to
    /// prevent. `factory_reset` is the way to unclaim a device, and it clears
    /// both halves.
    fn revoke(&self, id: &str) -> Result<AdminRecord, ClaimError> {
        let mut inner = self.state.write().expect("claim state lock poisoned");
        let Some(idx) = inner.admins.iter().position(|a| a.id == id) else {
            return Err(ClaimError::NoSuchAdmin);
        };
        if inner.admins.len() == 1 {
            return Err(ClaimError::LastAdmin);
        }
        let removed = inner.admins.remove(idx);
        let admins = inner.admins.clone();
        if let Err(e) = self.store.save_all(&admins) {
            // Put it back: a failed write means the file still lists them, and
            // an in-memory roster that disagrees would let the revocation
            // un-happen at the next restart with nobody the wiser.
            inner.admins.insert(idx, removed);
            return Err(e.into());
        }
        // Only once it is durable. Any request from that phone goes with it, so
        // a revoked admin cannot be re-approved from a card that was already on
        // screen — but dropping requests for a revocation that then failed to
        // persist would discard a decision nobody made.
        inner.pending.retain(|r| !r.peer.matches(&removed));
        info!(
            identity = %removed.identity_address,
            device = %removed.device_name,
            "Administrator revoked",
        );
        Ok(removed)
    }

    /// Wipe every admin record. Returns the previously-claimed records so
    /// callers can ask BlueZ to forget each bonded device.
    pub fn factory_reset(&self) -> Result<Vec<AdminRecord>, ClaimError> {
        let mut inner = self.state.write().expect("claim state lock poisoned");
        let previous = std::mem::take(&mut inner.admins);
        inner.pending.clear();
        if let Err(e) = self.store.clear() {
            // Restore state so a transient I/O error doesn't leave us in
            // an inconsistent in-memory state.
            inner.admins = previous;
            return Err(e.into());
        }
        if !previous.is_empty() {
            info!(
                admins = previous.len(),
                "Admin roster cleared (factory reset)"
            );
        }
        drop(inner);
        for record in &previous {
            self.request_unbond(&record.identity_address);
        }
        Ok(previous)
    }

    /// Gate every non-claim RPC. Called by the dispatcher before invoking the
    /// underlying `ManagementService` method.
    ///
    /// **This compares addresses, and that is the point.** It used to allow
    /// any peer that reached it, on the reasoning that an encrypt-authenticated
    /// GATT write proves a MITM-protected bond and v1 only ever had one bond.
    /// The first half still holds; the second no longer does. With more than
    /// one bond possible, "reached us over a bond" and "is an administrator"
    /// came apart — and they were already apart before this change, because
    /// nothing stopped a second phone from bonding and simply *not* calling
    /// `claim`: the device stays pairable while claimed and the pairing agent
    /// accepts everyone. A child's phone, in the room, in front of the TV, had
    /// the whole management surface.
    ///
    /// The address is a sound key for this despite the drift the old comment
    /// warned about. That drift is a *claim-time* phenomenon: while bonding is
    /// still in flight the peer is known by the random address it advertised
    /// under. By the time the companion sends `claim` it has completed
    /// bonding, and on every later reconnect BlueZ hands us the identity
    /// address from the D-Bus object path — verified on hardware on
    /// 2026-09-07, where a reconnecting Pixel's request writes arrived from
    /// exactly the address its record had held since it was claimed. (Note
    /// that is the *path* address; `Device1.Address` on the same object
    /// reports the classic BD_ADDR for a merged dual-mode device, and the two
    /// differ. The path is what a write reports and what the record holds.)
    ///
    /// A peer whose address matches nothing is denied and told so, and its way
    /// back in is the enrolment handshake: call `claim`, get a code, have an
    /// existing admin approve it.
    pub fn authorize(&self, peer: &PeerIdentity) -> AuthDecision {
        let inner = self.state.read().expect("claim state lock poisoned");
        if inner.admins.is_empty() {
            return AuthDecision::Deny {
                reason: "device is not claimed".into(),
            };
        }
        if inner.admins.iter().any(|a| peer.matches(a)) {
            return AuthDecision::Allow;
        }
        warn!(
            peer = %peer.address,
            identity = peer.resolved.as_deref().unwrap_or("<unresolved>"),
            admins = inner.admins.len(),
            "Denying an RPC from a bonded peer that is not an administrator",
        );
        AuthDecision::Deny {
            reason: "this phone is not an administrator of this device".into(),
        }
    }
}

/// The transport-neutral half, reached from `ManagementService` — and so from
/// a browser as well as a phone (issue #149).
///
/// Everything here goes through the same gate on the way in: over BLE
/// [`ClaimMachine::authorize`] has already established the caller is an
/// administrator, and over HTTP the auth middleware has established a live
/// session or a machine token. Neither is a weaker door than the other, which
/// is why approving a phone is offered on both.
impl AdminRoster for ClaimMachine {
    fn list_admins(&self) -> Vec<AdminSummary> {
        self.roster()
    }

    fn revoke_admin(&self, id: &str) -> Result<AdminSummary, AdminRosterError> {
        let removed = self.revoke(id)?;
        // The record is gone; the bond has to follow, or the phone stays
        // bonded to a device that refuses it — accepted at the link layer and
        // rejected at every RPC, which re-pairing cannot clear.
        self.request_unbond(&removed.identity_address);
        Ok(removed.summary())
    }

    fn list_enrolment_requests(&self) -> Vec<EnrolmentRequestInfo> {
        self.requests()
    }

    fn approve_enrolment_request(&self, id: &str) -> Result<AdminSummary, AdminRosterError> {
        Ok(self.approve(id)?)
    }

    fn deny_enrolment_request(&self, id: &str) -> Result<(), AdminRosterError> {
        Ok(self.deny(id)?)
    }
}

impl From<ClaimError> for AdminRosterError {
    fn from(e: ClaimError) -> Self {
        match e {
            ClaimError::EnrolmentDenied => AdminRosterError::EnrolmentDenied,
            ClaimError::NoSuchRequest => AdminRosterError::NoSuchRequest,
            ClaimError::NoSuchAdmin => AdminRosterError::NoSuchAdmin,
            ClaimError::LastAdmin => AdminRosterError::LastAdmin,
            ClaimError::Store(e) => AdminRosterError::Store(e.to_string()),
            // Neither can arise from a roster call: they are the claim flow's
            // answers, and the roster's callers have already been authorized.
            other => AdminRosterError::Store(other.to_string()),
        }
    }
}

impl AdminAuthority for ClaimMachine {
    fn verify_http_token(&self, presented: &str) -> bool {
        let inner = self.state.read().expect("claim state lock poisoned");
        // Every admin is checked, and the loop does not stop early on a match:
        // with a handful of records the cost is nothing, and short-circuiting
        // would make the reply time depend on which admin's token was
        // presented — a distinction between "wrong token" and "the third
        // admin's token" that nobody needs to be able to measure.
        let mut found = false;
        for admin in &inner.admins {
            found |= constant_time_eq(presented, &admin.http_token);
        }
        found
    }

    fn has_admin(&self) -> bool {
        !self
            .state
            .read()
            .expect("claim state lock poisoned")
            .admins
            .is_empty()
    }
}

/// Convenience for use with `Arc<ClaimMachine>`.
pub type SharedClaimMachine = Arc<ClaimMachine>;

fn info_for(request: &PendingEnrolment) -> EnrolmentRequestInfo {
    EnrolmentRequestInfo {
        id: request.id.clone(),
        code: request.code.clone(),
        device_name: request.device_name.clone(),
        peer: request.peer.identity().to_string(),
        requested_at: request.requested_at,
        expires_at: request.requested_at
            + ChronoDuration::from_std(ENROLMENT_REQUEST_TTL)
                .unwrap_or_else(|_| ChronoDuration::minutes(5)),
    }
}

/// Drop expired requests.
///
/// Called at the top of every operation that can *observe* a request —
/// `claim`, `list_requests`, `approve_request` — rather than on a timer, which
/// is enough to make expiry real: an expired request is never listed, never
/// approvable, and never returned to the phone that made it. There is nothing
/// a timer would additionally prevent, and the memory it would reclaim is one
/// small struct per phone that has ever completed Numeric Comparison against
/// this device and then not been approved.
fn sweep_pending(inner: &mut Inner) {
    let now = Instant::now();
    inner.pending.retain(|r| r.expires > now);
}

/// Six digits, uniformly, for a human to compare across two phones.
fn numeric_code() -> String {
    // Rejection sampling rather than `% 1_000_000`, which would make the low
    // codes fractionally likelier — the same reasoning as #156's login code.
    loop {
        let mut bytes = [0u8; 4];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let n = u32::from_le_bytes(bytes);
        if n < 4_294_000_000 {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(dir: &TempDir) -> AdminStore {
        AdminStore::new(Arc::new(lunchbox_util::LocalProtectedFiles::new(
            dir.path().to_path_buf(),
        )))
    }

    fn peer_a() -> PeerIdentity {
        PeerIdentity::unresolved("AA:BB:CC:DD:EE:FF", "public")
    }

    fn peer_b() -> PeerIdentity {
        PeerIdentity::unresolved("11:22:33:44:55:66", "public")
    }

    fn peer_c() -> PeerIdentity {
        PeerIdentity::unresolved("99:88:77:66:55:44", "public")
    }

    /// The record behind a `Claimed` outcome, or a panic naming what came back.
    fn admin_of(outcome: ClaimOutcome) -> AdminRecord {
        match outcome {
            ClaimOutcome::Claimed { admin } => admin,
            ClaimOutcome::Pending { request } => {
                panic!("expected a claim, got a pending request {}", request.id)
            }
        }
    }

    fn request_of(outcome: ClaimOutcome) -> EnrolmentRequestInfo {
        match outcome {
            ClaimOutcome::Pending { request } => request,
            ClaimOutcome::Claimed { admin } => {
                panic!("expected a pending request, got a claim by {}", admin.id)
            }
        }
    }

    /// A machine whose only admin is `peer_a`.
    fn claimed(dir: &TempDir) -> ClaimMachine {
        let m = ClaimMachine::load(store(dir)).unwrap();
        m.claim(peer_a(), "A".into()).unwrap();
        m
    }

    #[test]
    fn unclaimed_loads_when_no_record() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        assert!(!m.is_claimed());
        assert!(!m.has_admin());
    }

    #[test]
    fn first_claim_persists_and_transitions() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        let record = admin_of(m.claim(peer_a(), "iPhone".into()).unwrap());
        assert_eq!(record.device_name, "iPhone");
        assert!(m.is_claimed());
        assert!(m.verify_http_token(&record.http_token));

        // Reload from disk: state is preserved.
        let again = ClaimMachine::load(store(&dir)).unwrap();
        assert!(again.is_claimed());
        assert!(again.verify_http_token(&record.http_token));
    }

    #[test]
    fn claim_is_idempotent_for_an_existing_admin() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        let a = admin_of(m.claim(peer_a(), "iPhone".into()).unwrap());
        let b = admin_of(m.claim(peer_a(), "iPhone (renamed)".into()).unwrap());
        // Same record returned — re-claim does not overwrite device_name
        // for an established admin, and does not mint a second token.
        assert_eq!(a.http_token, b.http_token);
        assert_eq!(a.id, b.id);
        assert_eq!(b.device_name, "iPhone");
        assert_eq!(m.list_admins().len(), 1);
    }

    /// The heart of #149: a second phone is not refused, it is queued.
    #[test]
    fn second_phone_gets_a_pending_request() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);

        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());
        assert_eq!(request.device_name, "B");
        assert_eq!(request.peer, peer_b().address);
        assert_eq!(request.code.len(), 6);
        assert!(request.code.chars().all(|c| c.is_ascii_digit()));

        // Still one admin, and B cannot do anything yet.
        assert_eq!(m.list_admins().len(), 1);
        assert!(matches!(m.authorize(&peer_b()), AuthDecision::Deny { .. }));

        // Polling returns the same request rather than making another.
        let again = request_of(m.claim(peer_b(), "B".into()).unwrap());
        assert_eq!(again.id, request.id);
        assert_eq!(again.code, request.code);
        assert_eq!(m.list_enrolment_requests().len(), 1);
    }

    #[test]
    fn approval_makes_the_second_phone_an_admin() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());

        let summary = m.approve_enrolment_request(&request.id).unwrap();
        assert_eq!(summary.device_name, "B");
        assert_eq!(summary.identity_address, peer_b().address);

        // B is now authorized, and collects its own token by asking again.
        assert_eq!(m.authorize(&peer_b()), AuthDecision::Allow);
        let record = admin_of(m.claim(peer_b(), "B".into()).unwrap());
        assert!(m.verify_http_token(&record.http_token));

        // Two admins, each with their own token, both accepted.
        let admins = m.list_admins();
        assert_eq!(admins.len(), 2);
        // No "is this me?" flag on the wire — a client recognises its own row
        // by the identity address it already knows.
        assert!(
            admins
                .iter()
                .any(|a| a.device_name == "B" && a.identity_address == peer_b().address)
        );
        assert!(
            admins
                .iter()
                .any(|a| a.device_name == "A" && a.identity_address == peer_a().address)
        );
        // And never a token.
        assert!(m.verify_http_token(&record.http_token));

        // The request is gone, and approving it twice is not a second admin.
        assert!(m.list_enrolment_requests().is_empty());
        assert!(matches!(
            m.approve_enrolment_request(&request.id),
            Err(AdminRosterError::NoSuchRequest)
        ));

        // Survives a reload.
        let again = ClaimMachine::load(store(&dir)).unwrap();
        assert_eq!(again.list_admins().len(), 2);
        assert!(again.verify_http_token(&record.http_token));
    }

    /// Each admin's token is accepted, and only theirs.
    #[test]
    fn tokens_are_per_admin() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let a = admin_of(m.claim(peer_a(), "A".into()).unwrap());
        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());
        m.approve_enrolment_request(&request.id).unwrap();
        let b = admin_of(m.claim(peer_b(), "B".into()).unwrap());

        assert_ne!(a.http_token, b.http_token);
        assert!(m.verify_http_token(&a.http_token));
        assert!(m.verify_http_token(&b.http_token));
        assert!(!m.verify_http_token("not-a-token"));
        assert!(!m.verify_http_token(""));
    }

    #[test]
    fn denial_is_delivered_once_and_then_forgotten() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());

        m.deny_enrolment_request(&request.id).unwrap();
        // A denied request is no longer offered for approval.
        assert!(m.list_enrolment_requests().is_empty());
        assert!(matches!(
            m.approve_enrolment_request(&request.id),
            Err(AdminRosterError::NoSuchRequest)
        ));

        // The requester learns why, exactly once...
        assert!(matches!(
            m.claim(peer_b(), "B".into()),
            Err(ClaimError::EnrolmentDenied)
        ));
        // ...and asking again starts a fresh request rather than replaying the
        // refusal forever.
        let fresh = request_of(m.claim(peer_b(), "B".into()).unwrap());
        assert_ne!(fresh.id, request.id);
    }

    #[test]
    fn expired_requests_are_swept() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        m.claim(peer_b(), "B".into()).unwrap();
        assert_eq!(m.list_enrolment_requests().len(), 1);

        // Reach in and expire it: the TTL is minutes, and a test that waits
        // them out is a test nobody runs.
        {
            let mut inner = m.state.write().unwrap();
            inner.pending[0].expires = Instant::now() - Duration::from_secs(1);
        }
        assert!(m.list_enrolment_requests().is_empty());
        // And a poll after expiry makes a new request rather than resurrecting
        // one nobody is looking at any more.
        let fresh = request_of(m.claim(peer_b(), "B".into()).unwrap());
        assert_eq!(m.list_enrolment_requests().len(), 1);
        assert_eq!(m.list_enrolment_requests()[0].id, fresh.id);
    }

    #[test]
    fn separate_phones_get_separate_requests_and_codes() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let b = request_of(m.claim(peer_b(), "B".into()).unwrap());
        let c = request_of(m.claim(peer_c(), "C".into()).unwrap());

        // Distinct handles, so approving one cannot let the other in — which
        // is the whole reason the parent compares digits before tapping.
        assert_ne!(b.id, c.id);
        assert_eq!(m.list_enrolment_requests().len(), 2);

        m.approve_enrolment_request(&b.id).unwrap();
        assert_eq!(m.authorize(&peer_b()), AuthDecision::Allow);
        assert!(matches!(m.authorize(&peer_c()), AuthDecision::Deny { .. }));
    }

    #[test]
    fn authorize_unclaimed_denies_everyone() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        assert!(matches!(m.authorize(&peer_a()), AuthDecision::Deny { .. }));
    }

    /// The hole #149 closes: a bonded phone that never claimed used to be
    /// allowed through, because the gate only asked whether *anyone* had
    /// claimed the device.
    #[test]
    fn authorize_denies_a_bonded_peer_that_is_not_an_admin() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        assert_eq!(m.authorize(&peer_a()), AuthDecision::Allow);
        assert!(matches!(m.authorize(&peer_b()), AuthDecision::Deny { .. }));
    }

    /// BlueZ has been seen to report an address in either case.
    #[test]
    fn authorize_matches_addresses_case_insensitively() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let lowercased = PeerIdentity::unresolved(peer_a().address.to_lowercase(), "public");
        assert_eq!(m.authorize(&lowercased), AuthDecision::Allow);
    }

    /// A phone that claims mid-pairing is known by the random address its link
    /// came up on, while its bond is filed under its identity address. The
    /// record must hold the identity, or that phone can never authorize again
    /// — the failure this cost a bench pairing to find.
    #[test]
    fn a_claim_records_the_resolved_identity_not_the_link_address() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        let mid_pairing = PeerIdentity {
            address: "79:C2:1B:08:F0:32".into(),
            resolved: Some("64:11:A4:B0:7B:D9".into()),
            address_type: "public".into(),
        };
        let record = admin_of(m.claim(mid_pairing.clone(), "moto".into()).unwrap());
        assert_eq!(record.identity_address, "64:11:A4:B0:7B:D9");

        // The reconnect, which arrives under the identity and nothing else.
        let reconnect = PeerIdentity::unresolved("64:11:A4:B0:7B:D9", "public");
        assert_eq!(m.authorize(&reconnect), AuthDecision::Allow);
        // And the pairing-time sighting still matches, so a session that spans
        // the resolution is not cut off half way through.
        assert_eq!(m.authorize(&mid_pairing), AuthDecision::Allow);
    }

    /// The same, one step later: approving an enrolment records the identity.
    #[test]
    fn an_approval_records_the_resolved_identity() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let mid_pairing = PeerIdentity {
            address: "79:C2:1B:08:F0:32".into(),
            resolved: Some("64:11:A4:B0:7B:D9".into()),
            address_type: "public".into(),
        };
        let request = request_of(m.claim(mid_pairing, "moto".into()).unwrap());
        // The row the approving parent reads names the stable address, not the
        // one that is about to stop existing.
        assert_eq!(request.peer, "64:11:A4:B0:7B:D9");

        let summary = m.approve_enrolment_request(&request.id).unwrap();
        assert_eq!(summary.identity_address, "64:11:A4:B0:7B:D9");
        assert_eq!(
            m.authorize(&PeerIdentity::unresolved("64:11:A4:B0:7B:D9", "public")),
            AuthDecision::Allow
        );
    }

    /// A record written before #149 holds whichever address the claim arrived
    /// under, which for a phone reached over its LE identity is *not* the
    /// `Device1.Address` this build now resolves. Both have to match, or
    /// upgrading the daemon would lock out the phone that already administers
    /// the device.
    #[test]
    fn a_legacy_record_still_matches_after_resolution_starts() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        // Claimed the old way: the link address is all there was.
        m.claim(
            PeerIdentity::unresolved("78:61:DF:9B:2C:8E", "public"),
            "Pixel 10a".into(),
        )
        .unwrap();

        // Now the same phone arrives with resolution switched on, and BlueZ
        // reports its merged dual-mode identity — a different string.
        let resolved = PeerIdentity {
            address: "78:61:DF:9B:2C:8E".into(),
            resolved: Some("B8:F4:A4:E5:20:F1".into()),
            address_type: "public".into(),
        };
        assert_eq!(m.authorize(&resolved), AuthDecision::Allow);

        // A phone matching *neither* address is still refused.
        let stranger = PeerIdentity {
            address: "01:02:03:04:05:06".into(),
            resolved: Some("0A:0B:0C:0D:0E:0F".into()),
            address_type: "public".into(),
        };
        assert!(matches!(m.authorize(&stranger), AuthDecision::Deny { .. }));
    }

    #[test]
    fn revoke_removes_one_admin_and_its_token() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());
        m.approve_enrolment_request(&request.id).unwrap();
        let b = admin_of(m.claim(peer_b(), "B".into()).unwrap());

        let removed = m.revoke_admin(&b.id).unwrap();
        assert_eq!(removed.identity_address, peer_b().address);
        assert_eq!(m.list_admins().len(), 1);
        assert!(matches!(m.authorize(&peer_b()), AuthDecision::Deny { .. }));
        // The HTTP door closes with the BLE one.
        assert!(!m.verify_http_token(&b.http_token));
        // A is untouched.
        assert_eq!(m.authorize(&peer_a()), AuthDecision::Allow);

        let again = ClaimMachine::load(store(&dir)).unwrap();
        assert_eq!(again.list_admins().len(), 1);
        assert!(!again.verify_http_token(&b.http_token));
    }

    /// Revoking the last admin would leave a device nobody administers with a
    /// phone still bonded to it — the asymmetric-bond lockout. `factory_reset`
    /// is the operation that unclaims a device, because it drops both halves.
    #[test]
    fn revoke_refuses_the_last_admin() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let a = admin_of(m.claim(peer_a(), "A".into()).unwrap());
        assert!(matches!(
            m.revoke_admin(&a.id),
            Err(AdminRosterError::LastAdmin)
        ));
        assert!(m.is_claimed());
        assert_eq!(m.authorize(&peer_a()), AuthDecision::Allow);
    }

    #[test]
    fn revoke_rejects_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        assert!(matches!(
            m.revoke_admin("nope"),
            Err(AdminRosterError::NoSuchAdmin)
        ));
    }

    /// A revoked phone must not be let back in by a card the approving parent
    /// still had on screen.
    #[test]
    fn revoke_drops_that_phones_pending_request() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());
        m.approve_enrolment_request(&request.id).unwrap();
        let b = admin_of(m.claim(peer_b(), "B".into()).unwrap());

        // B is revoked, then asks again, then is revoked... the second ask
        // creates a request; revoking B again is a no-op, but the request from
        // a *revoked* phone must not survive the revocation that removed it.
        m.claim(peer_c(), "C".into()).unwrap();
        assert_eq!(m.list_enrolment_requests().len(), 1);
        m.revoke_admin(&b.id).unwrap();
        // C's request is untouched — only the revoked phone's would go.
        assert_eq!(m.list_enrolment_requests().len(), 1);
    }

    #[test]
    fn factory_reset_returns_every_admin_and_clears_the_store() {
        let dir = TempDir::new().unwrap();
        let m = claimed(&dir);
        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());
        m.approve_enrolment_request(&request.id).unwrap();
        m.claim(peer_c(), "C".into()).unwrap(); // leaves a pending request

        let previous = m.factory_reset().unwrap();
        // Both bonds come back, so both get queued for removal.
        assert_eq!(previous.len(), 2);
        assert!(!m.is_claimed());
        assert!(!m.has_admin());
        assert!(!m.verify_http_token(&previous[0].http_token));
        // Pending requests go too: there is nobody left to approve them.
        assert!(m.list_enrolment_requests().is_empty());

        let again = ClaimMachine::load(store(&dir)).unwrap();
        assert!(!again.is_claimed());
    }

    #[test]
    fn factory_reset_on_unclaimed_is_noop() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        assert!(m.factory_reset().unwrap().is_empty());
    }

    /// A device claimed before #149 keeps its admin and its token, and the
    /// file is rewritten in the current shape on load.
    #[test]
    fn a_v1_record_still_administers_the_device() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("admin.toml"),
            r#"
[admin]
identity_address = "AA:BB:CC:DD:EE:FF"
address_type = "public"
device_name = "Pixel 10a"
bonded_at = "2026-09-07T15:11:40.684313362-04:00"
http_token = "5ee6bfbb85bb23212c32763c0067a212b160b1dff6d895f6711d944d7f788989"
role = "admin"
"#,
        )
        .unwrap();

        let m = ClaimMachine::load(store(&dir)).unwrap();
        assert!(m.is_claimed());
        assert_eq!(m.authorize(&peer_a()), AuthDecision::Allow);
        assert!(
            m.verify_http_token("5ee6bfbb85bb23212c32763c0067a212b160b1dff6d895f6711d944d7f788989")
        );
        // And it is a normal admin from here on: a second phone can be
        // enrolled alongside it.
        let request = request_of(m.claim(peer_b(), "B".into()).unwrap());
        m.approve_enrolment_request(&request.id).unwrap();
        assert_eq!(m.list_admins().len(), 2);
    }
}
