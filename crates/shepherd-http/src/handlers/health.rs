//! Health and state snapshot handlers

use axum::{Json, extract::State};
use shepherd_api::HealthStatus;

use crate::error::ApiResult;
use crate::state::AppState;

pub async fn get_health(State(state): State<AppState>) -> ApiResult<Json<HealthStatus>> {
    Ok(Json(state.svc.health().await))
}

pub async fn get_state(
    State(state): State<AppState>,
) -> ApiResult<Json<shepherd_api::ServiceStateSnapshot>> {
    Ok(Json(state.svc.service_state().await))
}
