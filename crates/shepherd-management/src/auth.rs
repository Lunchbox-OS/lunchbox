//! Cross-transport admin authorization.
//!
//! [`AdminAuthority`] is the trait HTTP (and any future transport) uses
//! to discover the bearer token that the BLE claim flow minted for the
//! current admin. It lives here rather than in `shepherd-ble` so the
//! HTTP layer can consult it without taking a dependency on the BLE
//! crate.

/// Source of the per-admin HTTP bearer token. Returns `None` while the
/// device is unclaimed.
///
/// The token is rotated on factory reset and on new claims, so callers
/// must compare against the *current* value on every request rather
/// than caching across requests.
///
/// Implementations must support being called from synchronous code
/// (e.g. the HTTP auth middleware that runs per request) — they should
/// not block on async locks or perform I/O.
pub trait AdminAuthority: Send + Sync {
    fn current_http_token(&self) -> Option<String>;
}
