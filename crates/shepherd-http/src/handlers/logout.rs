//! Logout handler — terminates the current user's desktop session

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tracing::info;

use crate::state::AppState;

pub async fn logout(State(state): State<AppState>) -> impl IntoResponse {
    info!("Logout requested via HTTP");
    state.svc.logout().await;
    StatusCode::NO_CONTENT
}
