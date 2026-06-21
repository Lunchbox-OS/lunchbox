//! Daily override CRUD handlers

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::NaiveDate;
use serde::Deserialize;
use shepherd_api::DailyOverride;
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
    Ok(Json(state.svc.list_overrides(date).await?))
}

pub async fn get_override(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
    Query(q): Query<DateQuery>,
) -> ApiResult<Json<Option<DailyOverride>>> {
    let date = q.date.unwrap_or_else(|| shepherd_util::now().date_naive());
    let id = EntryId::new(entry_id);
    Ok(Json(state.svc.get_override(&id, date).await?))
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
    Ok(Json(
        state
            .svc
            .upsert_override(&id, date, body.availability, body.quota_delta_seconds)
            .await?,
    ))
}

pub async fn delete_override(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
    Query(q): Query<DateQuery>,
) -> impl IntoResponse {
    let date = q.date.unwrap_or_else(|| shepherd_util::now().date_naive());
    let id = EntryId::new(entry_id);

    match state.svc.delete_override(&id, date).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not_found", "message": "No override found for this entry and date" })),
        )
            .into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}
