//! Brightness control handlers

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use shepherd_api::BrightnessInfo;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub async fn get_brightness(State(state): State<AppState>) -> ApiResult<Json<BrightnessInfo>> {
    Ok(Json(state.svc.get_brightness().await?))
}

#[derive(Deserialize)]
pub struct BrightnessRequest {
    pub percent: u8,
}

pub async fn set_brightness(
    State(state): State<AppState>,
    Json(body): Json<BrightnessRequest>,
) -> impl IntoResponse {
    match state.svc.set_brightness(body.percent).await {
        Ok(info) => (StatusCode::OK, Json(info)).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}
