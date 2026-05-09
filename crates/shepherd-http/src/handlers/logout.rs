//! Logout handler — terminates the current user's desktop session

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tracing::warn;

use crate::state::AppState;

pub async fn logout(State(state): State<AppState>) -> impl IntoResponse {
    if let Err(e) = state.host.logout().await {
        warn!(error = %e, "Logout failed");
    }
    StatusCode::NO_CONTENT
}
