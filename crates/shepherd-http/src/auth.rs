//! Bearer token authentication middleware.
//!
//! Accepts either:
//!
//! - the static config token (`[service.management_api].auth_token`), or
//! - the current admin's HTTP token from the BLE claim flow, surfaced
//!   through [`AdminAuthority`].
//!
//! Open mode: if no static token is configured **and** no admin has
//! claimed via BLE yet, the middleware passes every request through
//! unchanged. This preserves the documented "omit auth_token to allow
//! unauthenticated access" behaviour even when BLE is enabled — a
//! BLE-configured-but-unclaimed device still has no token to enforce
//! and would otherwise lock itself out of HTTP. As soon as either a
//! static token is set or an admin claims via BLE, the gate engages
//! and unauthenticated requests are rejected with 401.

use axum::{
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use shepherd_management::AdminAuthority;
use std::sync::Arc;

/// Pair of auth sources injected into request extensions by the router
/// layer; cloned per request so the middleware can compare against
/// whichever combination is currently configured.
#[derive(Clone, Default)]
pub struct AuthSources {
    /// Static config token from `[service.management_api].auth_token`.
    pub static_token: Option<String>,
    /// Dynamic source of the admin's HTTP token (minted by the BLE
    /// claim flow). `None` means "BLE-derived auth is not configured";
    /// `Some(authority)` with `authority.current_http_token() == None`
    /// means BLE is configured but no admin has claimed yet.
    pub admin: Option<Arc<dyn AdminAuthority>>,
}

impl AuthSources {
    /// True when there is no token to enforce — neither a static
    /// config token nor a claimed BLE admin. A BLE authority that's
    /// plugged in but not yet claimed still counts as open, since
    /// otherwise a fresh install with BLE enabled would lock itself
    /// out of HTTP before any admin has been set up.
    fn is_open(&self) -> bool {
        self.static_token.is_none()
            && self
                .admin
                .as_ref()
                .and_then(|a| a.current_http_token())
                .is_none()
    }

    fn matches(&self, presented: &str) -> bool {
        if let Some(t) = &self.static_token
            && presented == t
        {
            return true;
        }
        if let Some(a) = &self.admin
            && let Some(t) = a.current_http_token()
            && presented == t
        {
            return true;
        }
        false
    }
}

pub async fn require_auth(req: Request, next: Next) -> Response {
    let sources: AuthSources = req
        .extensions()
        .get::<AuthSources>()
        .cloned()
        .unwrap_or_default();

    if sources.is_open() {
        return next.run(req).await;
    }

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_owned);

    let ok = provided.as_deref().is_some_and(|t| sources.matches(t));
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": "unauthorized",
                "message": "Valid Bearer token required"
            })),
        )
            .into_response();
    }

    next.run(req).await
}
