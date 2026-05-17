//! Debug endpoints for the host compositor.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use shepherd_api::{WindowAction, WindowInfo};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct WindowsResponse {
    pub windows: Vec<WindowInfo>,
}

pub async fn list_windows(State(state): State<AppState>) -> ApiResult<Json<WindowsResponse>> {
    let windows = state
        .host
        .list_windows()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(WindowsResponse { windows }))
}

async fn act(state: AppState, window_id: u64, action: WindowAction) -> ApiResult<StatusCode> {
    state
        .host
        .act_on_window(window_id, action)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn close_window(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<StatusCode> {
    act(state, id, WindowAction::Close).await
}

pub async fn hide_window(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<StatusCode> {
    act(state, id, WindowAction::Hide).await
}

pub async fn show_window(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<StatusCode> {
    act(state, id, WindowAction::Show).await
}
