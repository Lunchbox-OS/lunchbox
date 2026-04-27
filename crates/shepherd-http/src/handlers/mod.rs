//! HTTP route definitions

pub mod config;
pub mod entries;
pub mod health;
pub mod maintenance;
pub mod overrides;
pub mod sessions;
pub mod sse;
pub mod usage;
pub mod volume;

use axum::{Router, middleware, routing::get};

use crate::state::AppState;

/// Build the full API router under `/api/v1`
pub fn router(state: AppState, auth_token: Option<String>) -> Router {
    let api = Router::new()
        .route("/health", get(health::get_health))
        .route("/state", get(health::get_state))
        .route("/entries", get(entries::list_entries))
        .route("/entries/{id}", get(entries::get_entry))
        .route(
            "/sessions/current",
            get(sessions::get_current).delete(sessions::stop_current),
        )
        .route("/sessions", axum::routing::post(sessions::launch))
        .route(
            "/sessions/current/extend",
            axum::routing::post(sessions::extend_current),
        )
        .route("/overrides", get(overrides::list_overrides))
        .route(
            "/overrides/{entry_id}",
            get(overrides::get_override)
                .put(overrides::upsert_override)
                .delete(overrides::delete_override),
        )
        .route("/usage", get(usage::get_usage_all))
        .route("/usage/{entry_id}", get(usage::get_usage_entry))
        .route(
            "/maintenance",
            get(maintenance::get_maintenance)
                .post(maintenance::enter_maintenance)
                .delete(maintenance::exit_maintenance),
        )
        .route("/volume", get(volume::get_volume).put(volume::set_volume))
        .route("/config/reload", axum::routing::post(config::reload_config))
        .route("/events", get(sse::sse_handler))
        .with_state(state)
        .layer(middleware::from_fn(
            move |mut req: axum::extract::Request, next: middleware::Next| {
                let token = auth_token.clone();
                async move {
                    req.extensions_mut().insert(token);
                    crate::auth::require_auth(req, next).await
                }
            },
        ));

    Router::new()
        .nest("/api/v1", api)
        .fallback(crate::web_assets::static_handler)
}
