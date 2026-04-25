//! Daily override CRUD handlers

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::NaiveDate;
use serde::Deserialize;
use shepherd_api::{DailyOverride, Event, EventPayload};
use shepherd_util::EntryId;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct DateQuery {
    date: Option<NaiveDate>,
}

pub async fn list_overrides(
    State(state): State<AppState>,
    Query(q): Query<DateQuery>,
) -> ApiResult<Json<Vec<DailyOverride>>> {
    let date = q.date.unwrap_or_else(|| shepherd_util::now().date_naive());
    let overrides = state
        .store
        .list_daily_overrides(date)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(overrides))
}

pub async fn get_override(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
    Query(q): Query<DateQuery>,
) -> ApiResult<Json<Option<DailyOverride>>> {
    let date = q.date.unwrap_or_else(|| shepherd_util::now().date_naive());
    let id = EntryId::new(entry_id);
    let ov = state
        .store
        .get_daily_override(&id, date)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ov))
}

#[derive(Deserialize)]
pub struct UpsertOverrideBody {
    pub date: Option<NaiveDate>,
    pub availability: Option<bool>,
    pub quota_delta_seconds: Option<i64>,
}

pub async fn upsert_override(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
    Json(body): Json<UpsertOverrideBody>,
) -> ApiResult<Json<DailyOverride>> {
    let date = body
        .date
        .unwrap_or_else(|| shepherd_util::now().date_naive());
    let id = EntryId::new(entry_id);

    // Validate that at least one field is set
    if body.availability.is_none() && body.quota_delta_seconds.is_none() {
        return Err(ApiError::BadRequest(
            "At least one of 'availability' or 'quota_delta_seconds' must be provided".into(),
        ));
    }

    let ov = state
        .store
        .upsert_daily_override(&id, date, body.availability, body.quota_delta_seconds)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Broadcast state change so UIs update immediately
    let snap = state.engine.lock().await.get_state();
    let _ = state
        .event_tx
        .send(Event::new(EventPayload::StateChanged(snap)));

    Ok(Json(ov))
}

pub async fn delete_override(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
    Query(q): Query<DateQuery>,
) -> impl IntoResponse {
    let date = q.date.unwrap_or_else(|| shepherd_util::now().date_naive());
    let id = EntryId::new(entry_id);

    match state.store.clear_daily_override(&id, date) {
        Ok(true) => {
            let snap = state.engine.lock().await.get_state();
            let _ = state.event_tx.send(Event::new(EventPayload::StateChanged(snap)));
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not_found", "message": "No override found for this entry and date" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "internal_error", "message": e.to_string() })),
        )
            .into_response(),
    }
}
