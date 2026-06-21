//! Screen-time usage analytics handlers

use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::NaiveDate;
use serde::Deserialize;
use shepherd_api::UsageStat;
use shepherd_util::EntryId;

use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct DateRangeQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
}

pub async fn get_usage_all(
    State(state): State<AppState>,
    Query(q): Query<DateRangeQuery>,
) -> ApiResult<Json<Vec<UsageStat>>> {
    let today = shepherd_util::now().date_naive();
    let from = q.from.unwrap_or(today);
    let to = q.to.unwrap_or(today);
    Ok(Json(state.svc.usage_all(from, to).await?))
}

pub async fn get_usage_entry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DateRangeQuery>,
) -> ApiResult<Json<Vec<UsageStat>>> {
    let today = shepherd_util::now().date_naive();
    let from = q.from.unwrap_or(today);
    let to = q.to.unwrap_or(today);
    let entry_id = EntryId::new(id);
    Ok(Json(state.svc.usage_entry(&entry_id, from, to).await?))
}
