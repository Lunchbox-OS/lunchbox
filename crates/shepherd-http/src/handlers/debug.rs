//! Read-only debug endpoints for the host compositor.

use axum::{Json, extract::State};
use serde::Serialize;
use shepherd_api::WindowInfo;

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
