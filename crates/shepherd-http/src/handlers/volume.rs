//! Volume control handlers

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use shepherd_api::VolumeInfo;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub async fn get_volume(State(state): State<AppState>) -> ApiResult<Json<VolumeInfo>> {
    Ok(Json(state.svc.get_volume().await?))
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum VolumeRequest {
    Percent { percent: u8 },
    Mute { muted: bool },
}

pub async fn set_volume(
    State(state): State<AppState>,
    Json(body): Json<VolumeRequest>,
) -> impl IntoResponse {
    let result = match body {
        VolumeRequest::Percent { percent } => state.svc.set_volume(percent).await,
        VolumeRequest::Mute { muted } => state.svc.set_mute(muted).await,
    };
    match result {
        Ok(info) => (StatusCode::OK, Json(info)).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}
