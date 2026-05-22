//! Brightness control handlers

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use shepherd_api::{BrightnessInfo, BrightnessRestrictions, Event, EventPayload};
use shepherd_config::BrightnessPolicy;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub async fn get_brightness(State(state): State<AppState>) -> ApiResult<Json<BrightnessInfo>> {
    let restrictions = current_restrictions(&state).await;
    match state.brightness.get_status().await {
        Ok(s) => Ok(Json(BrightnessInfo {
            percent: s.percent,
            available: state.brightness.capabilities().available,
            backend: state.brightness.capabilities().backend.clone(),
            device: state.brightness.capabilities().device.clone(),
            restrictions,
        })),
        Err(e) => {
            // No backlight detected (or read failed) → return an "unavailable"
            // info instead of a 500 so UIs can hide the slider without
            // treating the call as an error.
            if !state.brightness.capabilities().available {
                Ok(Json(BrightnessInfo {
                    percent: 0,
                    available: false,
                    backend: state.brightness.capabilities().backend.clone(),
                    device: state.brightness.capabilities().device.clone(),
                    restrictions,
                }))
            } else {
                Err(ApiError::Internal(e.to_string()))
            }
        }
    }
}

#[derive(Deserialize)]
pub struct BrightnessRequest {
    pub percent: u8,
}

pub async fn set_brightness(
    State(state): State<AppState>,
    Json(body): Json<BrightnessRequest>,
) -> impl IntoResponse {
    let restrictions = current_restrictions(&state).await;

    if !restrictions.allow_change {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "forbidden",
                "message": "Brightness changes are not allowed",
            })),
        )
            .into_response();
    }

    let clamped = restrictions.clamp_brightness(body.percent);
    match state.brightness.set_brightness(clamped).await {
        Ok(()) => broadcast_brightness_change(&state).await,
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "internal_error",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

async fn broadcast_brightness_change(state: &AppState) -> axum::response::Response {
    match state.brightness.get_status().await {
        Ok(s) => {
            (state.broadcast_fn)(Event::new(EventPayload::BrightnessChanged {
                percent: s.percent,
            }));
            let info = BrightnessInfo {
                percent: s.percent,
                available: state.brightness.capabilities().available,
                backend: state.brightness.capabilities().backend.clone(),
                device: state.brightness.capabilities().device.clone(),
                restrictions: current_restrictions(state).await,
            };
            (StatusCode::OK, Json(info)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "internal_error",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

async fn current_restrictions(state: &AppState) -> BrightnessRestrictions {
    let eng = state.engine.lock().await;
    let policy = if let Some(session) = eng.current_session()
        && let Some(entry) = eng.policy().get_entry(&session.plan.entry_id)
        && let Some(ref br) = entry.brightness
    {
        br.clone()
    } else {
        eng.policy().brightness.clone()
    };
    convert_brightness_policy(&policy)
}

fn convert_brightness_policy(p: &BrightnessPolicy) -> BrightnessRestrictions {
    BrightnessRestrictions {
        max_brightness: p.max_brightness,
        min_brightness: p.min_brightness,
        allow_change: p.allow_change,
    }
}
