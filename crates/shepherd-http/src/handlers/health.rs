//! Health and state snapshot handlers

use axum::{Json, extract::State};
use shepherd_api::HealthStatus;

use crate::error::ApiResult;
use crate::state::AppState;

pub async fn get_health(State(state): State<AppState>) -> ApiResult<Json<HealthStatus>> {
    let _eng = state.engine.lock().await;
    Ok(Json(HealthStatus {
        live: true,
        ready: true,
        policy_loaded: true,
        host_adapter_ok: state.host.is_healthy(),
        store_ok: state.store.is_healthy(),
    }))
}

pub async fn get_state(
    State(state): State<AppState>,
) -> ApiResult<Json<shepherd_api::ServiceStateSnapshot>> {
    let eng = state.engine.lock().await;
    Ok(Json(eng.get_state()))
}
