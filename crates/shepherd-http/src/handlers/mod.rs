//! HTTP route definitions.
//!
//! The management surface is a two-endpoint API:
//!
//! - `POST /api/v1/rpc` — JSON-RPC pass-through into
//!   `ManagementService::dispatch_json`. Every trait method is
//!   reachable here as `{ "method": "name", "params": {...} }`. See
//!   [`rpc`] for the wire semantics.
//! - `GET /api/v1/events` — Server-Sent Events stream carrying every
//!   `shepherd_api::Event` the daemon broadcasts. Push-shaped, so it
//!   stays on its own endpoint rather than folding into RPC.
//! - `/api/v1/auth/*` — signing in (issue #156). Five of these are the only
//!   routes under `/api/v1` reachable without a credential; see [`auth`].
//! - `GET`/`PUT /api/v1/config` — the policy file itself (issue #185), for the
//!   web config editor. Off the RPC endpoint on purpose; see [`config`].

pub mod auth;
pub mod config;
pub mod rpc;
pub mod sse;

use axum::{
    Router, middleware,
    routing::{delete, get, post},
};

use crate::auth::AuthSources;
use crate::state::AppState;

/// Build the full API router under `/api/v1`.
///
/// `auth_sources` carries the credential store, the static config token (if
/// any) and the optional admin authority that sources BLE-derived tokens at
/// request time. Build it with [`AuthSources::new`]; the storeless
/// [`AuthSources::without_credential_store`] leaves the API open when nothing
/// else authenticates it, which is the pre-#156 behaviour and never a device.
pub fn router(state: AppState, auth_sources: AuthSources) -> Router {
    // Everything that needs a credential. The auth middleware sits on this
    // router alone, so adding a route here is automatically gated and adding
    // one to `open` below is a deliberate, visible act.
    let guarded = Router::new()
        .route("/rpc", post(rpc::dispatch))
        .route("/events", get(sse::sse_handler))
        .route("/config", get(config::read).put(config::write))
        .route("/auth/session", get(auth::current_session))
        .route("/auth/signout", post(auth::signout))
        .route("/auth/sessions", get(auth::list_sessions))
        .route("/auth/sessions/{id}", delete(auth::revoke_session))
        .with_state(state.clone())
        .layer(middleware::from_fn(
            move |req: axum::extract::Request, next: middleware::Next| async move {
                crate::auth::require_auth(req, next).await
            },
        ));

    // The five pre-auth routes. Each one either says something a login page
    // cannot render without (`status`) or is itself a way of authenticating,
    // and each is throttled inside `WebAuth` rather than out here, because the
    // throttle has to count a wrong password and a spurious approval request
    // against the same budget.
    let open = Router::new()
        .route("/auth/status", get(auth::status))
        .route("/auth/setup", post(auth::setup))
        .route("/auth/login", post(auth::login))
        .route("/auth/request", post(auth::request_login))
        .route("/auth/poll", post(auth::poll_login))
        .with_state(state);

    let api = Router::new().merge(guarded).merge(open).layer(
        // Both halves need the sources: the guarded one to authenticate, the
        // open one to mint cookies with the right `Secure` attribute.
        middleware::from_fn(
            move |mut req: axum::extract::Request, next: middleware::Next| {
                let sources = auth_sources.clone();
                async move {
                    req.extensions_mut().insert(sources);
                    next.run(req).await
                }
            },
        ),
    );

    Router::new()
        .nest("/api/v1", api)
        .fallback(crate::web_assets::static_handler)
}
