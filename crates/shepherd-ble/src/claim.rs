//! `ClaimMachine`: the `Unclaimed → Claimed` state machine and the
//! per-request authorization gate.
//!
//! Trust-on-First-Use single-admin model (v1). Multi-admin is deferred
//! but the per-bond record schema in [`crate::admin`] already
//! accommodates additional roles when it lands.

use crate::admin::{AdminRecord, AdminStore, AdminStoreError};
use shepherd_management::AdminAuthority;
use std::sync::{Arc, RwLock};
use thiserror::Error;
use tracing::{info, warn};

/// Stable peer identity as resolved by BlueZ post-pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub address: String,
    pub address_type: String,
}

impl PeerIdentity {
    pub fn matches(&self, record: &AdminRecord) -> bool {
        self.address.eq_ignore_ascii_case(&record.identity_address)
            && self.address_type == record.address_type
    }
}

#[derive(Debug, Clone)]
pub enum ClaimState {
    Unclaimed,
    Claimed(AdminRecord),
}

impl ClaimState {
    pub fn is_claimed(&self) -> bool {
        matches!(self, ClaimState::Claimed(_))
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
    #[error("device is already claimed by another admin")]
    AlreadyClaimed,
    #[error("device must be claimed before any other operation")]
    NotClaimed,
    #[error("peer is not the admin")]
    PermissionDenied,
    #[error("admin store error: {0}")]
    Store(#[from] AdminStoreError),
}

/// State machine uses [`std::sync::RwLock`] so that synchronous
/// consumers (notably the HTTP auth middleware, via
/// [`AdminAuthority`]) can check the current admin token without
/// blocking on an async lock. All operations hold the lock only for
/// brief in-memory updates plus an atomic file write on claim/reset.
pub struct ClaimMachine {
    state: RwLock<ClaimState>,
    store: AdminStore,
}

impl ClaimMachine {
    /// Construct a machine seeded with the persisted admin record (if
    /// any). The `store` is retained so subsequent claim / reset
    /// transitions persist immediately.
    pub fn new(store: AdminStore, initial: ClaimState) -> Self {
        Self {
            state: RwLock::new(initial),
            store,
        }
    }

    /// Convenience: load the admin record from disk and wrap it in a
    /// machine. Returns `Unclaimed` on a missing file.
    pub fn load(store: AdminStore) -> Result<Self, ClaimError> {
        let initial = match store.load()? {
            Some(record) => ClaimState::Claimed(record),
            None => ClaimState::Unclaimed,
        };
        Ok(Self::new(store, initial))
    }

    pub fn snapshot(&self) -> ClaimState {
        self.state
            .read()
            .expect("claim state lock poisoned")
            .clone()
    }

    pub fn is_claimed(&self) -> bool {
        self.state
            .read()
            .expect("claim state lock poisoned")
            .is_claimed()
    }

    /// Claim the device for `peer` under `device_name`. Idempotent for
    /// the same peer (returns the existing record); rejected otherwise.
    pub fn claim(
        &self,
        peer: PeerIdentity,
        device_name: String,
    ) -> Result<AdminRecord, ClaimError> {
        let mut state = self.state.write().expect("claim state lock poisoned");
        match &*state {
            ClaimState::Claimed(existing) if peer.matches(existing) => {
                // Same peer re-issuing claim — idempotent.
                Ok(existing.clone())
            }
            ClaimState::Claimed(_) => Err(ClaimError::AlreadyClaimed),
            ClaimState::Unclaimed => {
                let record = AdminRecord::new(peer.address, peer.address_type, device_name);
                self.store.save(&record)?;
                info!(
                    identity = %record.identity_address,
                    device = %record.device_name,
                    "Admin claim recorded",
                );
                *state = ClaimState::Claimed(record.clone());
                Ok(record)
            }
        }
    }

    /// Wipe the admin record and bond. Returns the previously-claimed
    /// record (if any) so callers can ask BlueZ to forget the bonded
    /// device.
    pub fn factory_reset(&self) -> Result<Option<AdminRecord>, ClaimError> {
        let mut state = self.state.write().expect("claim state lock poisoned");
        let previous = match std::mem::replace(&mut *state, ClaimState::Unclaimed) {
            ClaimState::Unclaimed => None,
            ClaimState::Claimed(r) => Some(r),
        };
        if let Err(e) = self.store.clear() {
            // Restore state so a transient I/O error doesn't leave us in
            // an inconsistent in-memory state.
            if let Some(ref r) = previous {
                *state = ClaimState::Claimed(r.clone());
            }
            return Err(e.into());
        }
        if previous.is_some() {
            info!("Admin claim cleared (factory reset)");
        }
        Ok(previous)
    }

    /// Gate every non-claim RPC. Called by the dispatcher before
    /// invoking the underlying `ManagementService` method.
    pub fn authorize(&self, peer: &PeerIdentity) -> AuthDecision {
        match &*self.state.read().expect("claim state lock poisoned") {
            ClaimState::Unclaimed => AuthDecision::Deny {
                reason: "device is not claimed".into(),
            },
            ClaimState::Claimed(record) if peer.matches(record) => AuthDecision::Allow,
            ClaimState::Claimed(_) => {
                warn!(peer = ?peer, "Rejecting RPC from non-admin peer");
                AuthDecision::Deny {
                    reason: "peer is not the admin".into(),
                }
            }
        }
    }
}

impl AdminAuthority for ClaimMachine {
    fn current_http_token(&self) -> Option<String> {
        match &*self.state.read().expect("claim state lock poisoned") {
            ClaimState::Unclaimed => None,
            ClaimState::Claimed(record) => Some(record.http_token.clone()),
        }
    }
}

/// Convenience for use with `Arc<ClaimMachine>`.
pub type SharedClaimMachine = Arc<ClaimMachine>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(dir: &TempDir) -> AdminStore {
        AdminStore::new(dir.path().join("admin.toml"))
    }

    fn peer_a() -> PeerIdentity {
        PeerIdentity {
            address: "AA:BB:CC:DD:EE:FF".into(),
            address_type: "public".into(),
        }
    }

    fn peer_b() -> PeerIdentity {
        PeerIdentity {
            address: "11:22:33:44:55:66".into(),
            address_type: "public".into(),
        }
    }

    #[test]
    fn unclaimed_loads_when_no_record() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        assert!(!m.is_claimed());
        assert!(m.current_http_token().is_none());
    }

    #[test]
    fn claim_persists_and_transitions() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        let record = m.claim(peer_a(), "iPhone".into()).unwrap();
        assert_eq!(record.device_name, "iPhone");
        assert!(m.is_claimed());
        assert_eq!(
            m.current_http_token().as_deref(),
            Some(record.http_token.as_str())
        );

        // Reload from disk: state is preserved.
        let again = ClaimMachine::load(store(&dir)).unwrap();
        assert!(again.is_claimed());
    }

    #[test]
    fn claim_is_idempotent_for_same_peer() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        let a = m.claim(peer_a(), "iPhone".into()).unwrap();
        let b = m.claim(peer_a(), "iPhone (renamed)".into()).unwrap();
        // Same record returned — re-claim does not overwrite device_name
        // for the established admin.
        assert_eq!(a.http_token, b.http_token);
        assert_eq!(b.device_name, "iPhone");
    }

    #[test]
    fn claim_rejects_different_peer() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        m.claim(peer_a(), "A".into()).unwrap();
        let err = m.claim(peer_b(), "B".into()).unwrap_err();
        assert!(matches!(err, ClaimError::AlreadyClaimed));
    }

    #[test]
    fn authorize_unclaimed_denies_everyone() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        assert!(matches!(m.authorize(&peer_a()), AuthDecision::Deny { .. }));
    }

    #[test]
    fn authorize_claimed_allows_admin_only() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        m.claim(peer_a(), "A".into()).unwrap();
        assert_eq!(m.authorize(&peer_a()), AuthDecision::Allow);
        assert!(matches!(m.authorize(&peer_b()), AuthDecision::Deny { .. }));
    }

    #[test]
    fn factory_reset_returns_previous_and_clears_store() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        m.claim(peer_a(), "A".into()).unwrap();
        let prev = m.factory_reset().unwrap();
        assert!(prev.is_some());
        assert!(!m.is_claimed());
        assert!(m.current_http_token().is_none());

        // Reload from disk confirms persistence.
        let again = ClaimMachine::load(store(&dir)).unwrap();
        assert!(!again.is_claimed());
    }

    #[test]
    fn factory_reset_on_unclaimed_is_noop() {
        let dir = TempDir::new().unwrap();
        let m = ClaimMachine::load(store(&dir)).unwrap();
        assert!(m.factory_reset().unwrap().is_none());
    }
}
