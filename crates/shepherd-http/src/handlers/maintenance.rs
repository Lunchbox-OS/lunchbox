//! Maintenance / debug mode handlers

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use shepherd_api::MaintenanceState;

use crate::error::ApiResult;
use crate::state::AppState;

pub async fn get_maintenance(State(state): State<AppState>) -> ApiResult<Json<MaintenanceState>> {
    Ok(Json(state.maintenance.lock().await.clone()))
}

pub async fn enter_maintenance(State(state): State<AppState>) -> impl IntoResponse {
    let mut ms = state.maintenance.lock().await;
    if ms.active {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "conflict", "message": "Maintenance mode already active" })),
        )
            .into_response();
    }
    ms.active = true;
    ms.activated_at = Some(shepherd_util::now());
    let current = ms.clone();
    drop(ms);

    // Ask host to relax restrictions
    let _ = state.host.set_maintenance_mode(true).await;

    (StatusCode::OK, Json(current)).into_response()
}

pub async fn exit_maintenance(State(state): State<AppState>) -> impl IntoResponse {
    let mut ms = state.maintenance.lock().await;
    if !ms.active {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not_found", "message": "Maintenance mode is not active" })),
        )
            .into_response();
    }
    ms.active = false;
    ms.activated_at = None;
    drop(ms);

    let _ = state.host.set_maintenance_mode(false).await;

    StatusCode::NO_CONTENT.into_response()
}
