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

pub mod rpc;
pub mod sse;

use axum::{Router, middleware, routing::get};

use crate::auth::AuthSources;
use crate::state::AppState;

/// Build the full API router under `/api/v1`.
///
/// `auth_sources` carries the static config token (if any) and the
/// optional admin authority that sources BLE-derived tokens at request
/// time. Pass `AuthSources::default()` to leave the API open (legacy
/// behaviour when neither auth source is configured).
pub fn router(state: AppState, auth_sources: AuthSources) -> Router {
    let api = Router::new()
        .route("/rpc", axum::routing::post(rpc::dispatch))
        .route("/events", get(sse::sse_handler))
        .with_state(state)
        .layer(middleware::from_fn(
            move |mut req: axum::extract::Request, next: middleware::Next| {
                let sources = auth_sources.clone();
                async move {
                    req.extensions_mut().insert(sources);
                    crate::auth::require_auth(req, next).await
                }
            },
        ));

    Router::new()
        .nest("/api/v1", api)
        .fallback(crate::web_assets::static_handler)
}
