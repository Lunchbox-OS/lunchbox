//! Volume control handlers

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use shepherd_api::{Event, EventPayload, VolumeInfo, VolumeRestrictions};
use shepherd_config::VolumePolicy;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub async fn get_volume(State(state): State<AppState>) -> ApiResult<Json<VolumeInfo>> {
    let restrictions = current_restrictions(&state).await;
    match state.volume.get_status().await {
        Ok(s) => Ok(Json(VolumeInfo {
            percent: s.percent,
            muted: s.muted,
            available: state.volume.capabilities().available,
            backend: state.volume.capabilities().backend.clone(),
            restrictions,
        })),
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
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
    let restrictions = current_restrictions(&state).await;

    match body {
        VolumeRequest::Percent { percent } => {
            if !restrictions.allow_change {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "error": "forbidden", "message": "Volume changes are not allowed" })),
                )
                    .into_response();
            }
            let clamped = restrictions.clamp_volume(percent);
            match state.volume.set_volume(clamped).await {
                Ok(()) => broadcast_volume_change(&state).await,
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::json!({ "error": "internal_error", "message": e.to_string() }),
                    ),
                )
                    .into_response(),
            }
        }
        VolumeRequest::Mute { muted } => {
            if !restrictions.allow_mute {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "error": "forbidden", "message": "Mute toggle is not allowed" })),
                )
                    .into_response();
            }
            match state.volume.set_mute(muted).await {
                Ok(()) => broadcast_volume_change(&state).await,
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::json!({ "error": "internal_error", "message": e.to_string() }),
                    ),
                )
                    .into_response(),
            }
        }
    }
}

async fn broadcast_volume_change(state: &AppState) -> axum::response::Response {
    match state.volume.get_status().await {
        Ok(s) => {
            let _ = state.event_tx.send(Event::new(EventPayload::VolumeChanged {
                percent: s.percent,
                muted: s.muted,
            }));
            let info = VolumeInfo {
                percent: s.percent,
                muted: s.muted,
                available: state.volume.capabilities().available,
                backend: state.volume.capabilities().backend.clone(),
                restrictions: current_restrictions(state).await,
            };
            (StatusCode::OK, Json(info)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "internal_error", "message": e.to_string() })),
        )
            .into_response(),
    }
}

async fn current_restrictions(state: &AppState) -> VolumeRestrictions {
    let eng = state.engine.lock().await;
    let policy = if let Some(session) = eng.current_session()
        && let Some(entry) = eng.policy().get_entry(&session.plan.entry_id)
        && let Some(ref vol) = entry.volume
    {
        vol.clone()
    } else {
        eng.policy().volume.clone()
    };
    convert_volume_policy(&policy)
}

fn convert_volume_policy(p: &VolumePolicy) -> VolumeRestrictions {
    VolumeRestrictions {
        max_volume: p.max_volume,
        min_volume: p.min_volume,
        allow_mute: p.allow_mute,
        allow_change: p.allow_change,
    }
}
