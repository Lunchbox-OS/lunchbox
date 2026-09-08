//! Cross-transport admin authorization.
//!
//! [`AdminAuthority`] is the trait HTTP (and any future transport) uses
//! to check a presented bearer token against the ones the BLE claim flow
//! minted for this device's administrators. It lives here rather than in
//! `shepherd-ble` so the HTTP layer can consult it without taking a
//! dependency on the BLE crate.

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
