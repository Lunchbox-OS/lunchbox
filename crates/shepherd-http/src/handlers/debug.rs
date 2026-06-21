//! Debug endpoints for the host compositor.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use shepherd_api::{WindowAction, WindowInfo};

use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct WindowsResponse {
    pub windows: Vec<WindowInfo>,
}

pub async fn list_windows(State(state): State<AppState>) -> ApiResult<Json<WindowsResponse>> {
    let windows = state.svc.list_windows().await?;
    Ok(Json(WindowsResponse { windows }))
}

pub async fn close_window(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<StatusCode> {
    state.svc.act_on_window(id, WindowAction::Close).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn hide_window(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<StatusCode> {
    state.svc.act_on_window(id, WindowAction::Hide).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn show_window(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<StatusCode> {
    state.svc.act_on_window(id, WindowAction::Show).await?;
    Ok(StatusCode::NO_CONTENT)
}
