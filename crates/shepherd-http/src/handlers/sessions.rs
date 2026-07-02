//! Session control handlers (launch, stop, extend)

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use shepherd_api::{ReasonCode, SessionInfo, StopMode};
use shepherd_management::{LaunchOutcome, ManagementError};
use shepherd_util::EntryId;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub async fn get_current(State(state): State<AppState>) -> ApiResult<Json<Option<SessionInfo>>> {
    Ok(Json(state.svc.current_session().await))
}

#[derive(Deserialize)]
pub struct LaunchRequest {
    pub entry_id: String,
}

#[derive(Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum LaunchResponse {
    Approved {
        session_id: String,
        deadline: Option<DateTime<chrono::Local>>,
    },
    Denied {
        reasons: Vec<ReasonCode>,
    },
}

pub async fn launch(
    State(state): State<AppState>,
    Json(body): Json<LaunchRequest>,
) -> impl IntoResponse {
    let entry_id = EntryId::new(body.entry_id);
    match state.svc.launch(entry_id).await {
        Ok(LaunchOutcome::Approved {
            session_id,
            deadline,
        }) => (
            StatusCode::OK,
            Json(LaunchResponse::Approved {
                session_id,
                deadline,
            }),
        )
            .into_response(),
        Ok(LaunchOutcome::Denied { reasons }) => {
            (StatusCode::OK, Json(LaunchResponse::Denied { reasons })).into_response()
        }
        // Preserve the existing wire shape for these specific launch failures:
        // a LaunchResponse::Denied body with a Disabled reason, but a non-200
        // status. New transports (BLE) can read the ManagementError directly.
        // The browser-spec + confirm_on_close wiring that main added inline
        // lives in `ManagementService::launch` now, so it applies uniformly
        // to every transport (HTTP, IPC, BLE) instead of only the HTTP path.
        Err(ManagementError::NotFound(msg)) => (
            StatusCode::NOT_FOUND,
            Json(LaunchResponse::Denied {
                reasons: vec![ReasonCode::Disabled { reason: Some(msg) }],
            }),
        )
            .into_response(),
        Err(ManagementError::Internal(msg)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(LaunchResponse::Denied {
                reasons: vec![ReasonCode::Disabled { reason: Some(msg) }],
            }),
        )
            .into_response(),
        Err(other) => ApiError::from(other).into_response(),
    }
}

#[derive(Deserialize)]
pub struct StopRequest {
    #[serde(default = "default_graceful")]
    pub mode: StopMode,
}

fn default_graceful() -> StopMode {
    StopMode::Graceful
}

pub async fn stop_current(
    State(state): State<AppState>,
    body: Option<Json<StopRequest>>,
) -> impl IntoResponse {
    let mode = body.map(|b| b.0.mode).unwrap_or(StopMode::Graceful);
    match state.svc.stop_current(mode).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(ManagementError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no_active_session" })),
        )
            .into_response(),
        Err(other) => ApiError::from(other).into_response(),
    }
}

#[derive(Deserialize)]
pub struct ExtendRequest {
    pub seconds: i64,
}

#[derive(Serialize)]
pub struct ExtendResponse {
    pub new_deadline: Option<DateTime<chrono::Local>>,
}

pub async fn extend_current(
    State(state): State<AppState>,
    Json(body): Json<ExtendRequest>,
) -> ApiResult<Json<ExtendResponse>> {
    let new_deadline = state.svc.extend_current(body.seconds).await?;
    Ok(Json(ExtendResponse { new_deadline }))
}
