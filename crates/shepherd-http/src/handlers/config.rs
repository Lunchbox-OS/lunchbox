//! Config reload handler

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use shepherd_api::{Event, EventPayload};
use shepherd_config::load_config;
use tracing::warn;

use crate::state::AppState;

pub async fn reload_config(State(state): State<AppState>) -> impl IntoResponse {
    match load_config(&state.config_path) {
        Ok(policy) => {
            let entry_count = policy.entries.len();
            {
                let mut eng = state.engine.lock().await;
                eng.reload_policy(policy);
            }
            let snap = state.engine.lock().await.get_state();
            (state.broadcast_fn)(Event::new(EventPayload::PolicyReloaded { entry_count }));
            (state.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
            (
                StatusCode::OK,
                Json(serde_json::json!({ "entry_count": entry_count })),
            )
                .into_response()
        }
        Err(e) => {
            warn!(error = %e, "Config reload failed via HTTP API");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": "config_error", "message": e.to_string() })),
            )
                .into_response()
        }
    }
}
