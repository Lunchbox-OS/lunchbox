//! Entry list and detail handlers

use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::DateTime;
use serde::Deserialize;
use shepherd_api::EntryView;
use shepherd_util::EntryId;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct AtTimeQuery {
    at: Option<DateTime<chrono::Local>>,
}

pub async fn list_entries(
    State(state): State<AppState>,
    Query(q): Query<AtTimeQuery>,
) -> ApiResult<Json<Vec<EntryView>>> {
    let now = q.at.unwrap_or_else(shepherd_util::now);
    let eng = state.engine.lock().await;
    Ok(Json(eng.list_entries(now)))
}

pub async fn get_entry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AtTimeQuery>,
) -> ApiResult<Json<EntryView>> {
    let now = q.at.unwrap_or_else(shepherd_util::now);
    let entry_id = EntryId::new(id);
    let eng = state.engine.lock().await;

    eng.list_entries(now)
        .into_iter()
        .find(|e| e.entry_id == entry_id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("No entry with id '{entry_id}'")))
}
