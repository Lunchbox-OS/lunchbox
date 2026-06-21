//! Config reload handler

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use crate::error::ApiError;
use crate::state::AppState;

pub async fn reload_config(State(state): State<AppState>) -> impl IntoResponse {
    match state.svc.reload_config().await {
        Ok(entry_count) => (
            StatusCode::OK,
            Json(serde_json::json!({ "entry_count": entry_count })),
        )
            .into_response(),
        // The underlying load_config error is surfaced as Unprocessable; map
        // to the existing 422 + {"error": "config_error", ...} wire shape so
        // existing clients continue to recognise the failure code.
        Err(shepherd_management::ManagementError::Unprocessable(msg)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "config_error", "message": msg })),
        )
            .into_response(),
        Err(other) => ApiError::from(other).into_response(),
    }
}
