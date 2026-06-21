//! Entry list and detail handlers

use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::DateTime;
use serde::Deserialize;
use shepherd_api::EntryView;
use shepherd_util::EntryId;

use crate::error::ApiResult;
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
    Ok(Json(state.svc.list_entries(now).await))
}

pub async fn get_entry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AtTimeQuery>,
) -> ApiResult<Json<EntryView>> {
    let now = q.at.unwrap_or_else(shepherd_util::now);
    let entry_id = EntryId::new(id);
    Ok(Json(state.svc.get_entry(&entry_id, now).await?))
}
