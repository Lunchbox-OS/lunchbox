//! Cross-transport admin authorization, and the administrator roster.
//!
//! [`AdminAuthority`] is the trait HTTP (and any future transport) uses
//! to check a presented bearer token against the ones the BLE claim flow
//! minted for this device's administrators. [`AdminRoster`] is the same idea
//! for *managing* those administrators — listing them, approving a new phone,
//! revoking an old one. Both live here rather than in `shepherd-ble` so the
//! HTTP layer and `ManagementService` can reach them without taking a
//! dependency on the BLE crate.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// Verifier for the per-admin HTTP bearer tokens the BLE claim flow mints.
///
/// **Ask, do not fetch.** This used to be `current_http_token() -> Option<String>`
/// and the caller did the comparison. A device can have several administrators
/// (issue #149), each with their own token, so the singular answer no longer
/// exists — and the plural one, a `Vec<String>` of live credentials, would
/// push the constant-time discipline out to every caller and copy every secret
/// on every request to do it. The comparison belongs where the tokens are.
///
/// Tokens are minted per admin, dropped when that admin is revoked, and all
/// dropped on factory reset, so callers must ask on every request rather than
/// caching an answer.
///
/// Implementations must support being called from synchronous code
/// (e.g. the HTTP auth middleware that runs per request) — they should
/// not block on async locks or perform I/O.
pub trait AdminAuthority: Send + Sync {
    /// Whether `presented` is the live token of any current administrator.
    /// Implementations must compare in constant time.
    fn verify_http_token(&self, presented: &str) -> bool;

    /// Whether the device has any administrator at all. Distinguishes an
    /// unclaimed device — which has no companion to approve anything, and no
    /// bearer credential to check against — from a claimed one.
    fn has_admin(&self) -> bool;
}

// ---------------------------------------------------------------------------
// The administrator roster (issue #149)
// ---------------------------------------------------------------------------

/// One administrator, as anyone with standing to see the list sees them.
///
/// Carries no credential. The record behind this holds a minted HTTP bearer
/// token, which is the whole reason the summary exists: listing administrators
/// hands the list to a phone or a browser, and a client that can read every
/// other client's token does not need to be revoked to keep using the device
/// after it has been.
///
/// Here rather than in `shepherd-ble` because both transports return it and
/// `shepherd-ble` sits *above* this crate — the same reason
/// [`crate::webauth::WebSessionInfo`] lives here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AdminSummary {
    /// Stable public handle — what `revoke_admin` takes. Not a credential.
    pub id: String,
    pub device_name: String,
    /// Shown so a parent can tell two phones with the same name apart, and so
    /// a client can recognise its own row without the device having to guess
    /// which caller it is talking to.
    pub identity_address: String,
    pub bonded_at: DateTime<Local>,
    pub role: String,
}

/// A phone waiting to be let in, as an administrator sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EnrolmentRequestInfo {
    /// Public handle — what `approve_enrolment_request` takes. Safe to list.
    pub id: String,
    /// Six digits the requesting phone is displaying. Whoever approves
    /// compares them against that phone's screen.
    ///
    /// Same ritual as BLE pairing's Numeric Comparison and #156's login code,
    /// and for the same reason: a racing attacker's request carries different
    /// digits, so comparing is what picks the right row out of a list.
    pub code: String,
    /// What the requesting phone calls itself.
    pub device_name: String,
    /// Its address, so two phones with the same name are still distinguishable.
    pub peer: String,
    pub requested_at: DateTime<Local>,
    pub expires_at: DateTime<Local>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdminRosterError {
    #[error("an administrator turned this request down")]
    EnrolmentDenied,
    #[error("no such enrolment request, or it expired")]
    NoSuchRequest,
    #[error("no such administrator")]
    NoSuchAdmin,
    #[error("this is the only administrator; factory-reset the device instead")]
    LastAdmin,
    #[error("admin store error: {0}")]
    Store(String),
}

/// The device's administrator roster, as a transport-neutral surface.
///
/// Implemented by `shepherd-ble`'s `ClaimMachine`, which owns the records, the
/// bonds and the enrolment handshake. It is a trait here for the same reason
/// [`AdminAuthority`] is: the roster has to be reachable from
/// [`crate::ManagementService`], and this crate sits below `shepherd-ble` and
/// cannot name it.
///
/// Making it reachable from the trait is what lets a browser approve a second
/// phone (issue #149) — the alternative was routing these in the BLE server
/// alone, which left the web management UI unable to do the one thing a parent
/// sitting at a laptop most plausibly wants to do with it.
///
/// Every method is synchronous: the implementation holds an in-memory roster
/// behind a `std::sync::RwLock` plus, on the paths that change something, one
/// atomic file write.
pub trait AdminRoster: Send + Sync {
    fn list_admins(&self) -> Vec<AdminSummary>;

    /// Remove one administrator and forget its Bluetooth bond.
    ///
    /// Refuses the last one: that would leave a device nobody administers with
    /// a phone still bonded to it, which is a lockout re-pairing cannot clear.
    /// `factory_reset` is the operation that unclaims a device.
    fn revoke_admin(&self, id: &str) -> Result<AdminSummary, AdminRosterError>;

    fn list_enrolment_requests(&self) -> Vec<EnrolmentRequestInfo>;

    /// Let a waiting phone in, minting its own record and token. The token is
    /// not returned — the requester collects it over its own bond.
    fn approve_enrolment_request(&self, id: &str) -> Result<AdminSummary, AdminRosterError>;

    fn deny_enrolment_request(&self, id: &str) -> Result<(), AdminRosterError>;
}
