//! HTTP route definitions

pub mod brightness;
pub mod config;
pub mod debug;
pub mod entries;
pub mod health;
pub mod logout;
pub mod overrides;
pub mod sessions;
pub mod sse;
pub mod usage;
pub mod volume;

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
        .route("/volume", get(volume::get_volume).put(volume::set_volume))
        .route(
            "/brightness",
            get(brightness::get_brightness).put(brightness::set_brightness),
        )
        .route("/config/reload", axum::routing::post(config::reload_config))
        .route("/user/logout", axum::routing::post(logout::logout))
        .route("/debug/windows", get(debug::list_windows))
        .route(
            "/debug/windows/{id}/close",
            axum::routing::post(debug::close_window),
        )
        .route(
            "/debug/windows/{id}/hide",
            axum::routing::post(debug::hide_window),
        )
        .route(
            "/debug/windows/{id}/show",
            axum::routing::post(debug::show_window),
        )
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
