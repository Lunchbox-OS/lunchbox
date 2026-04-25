//! Screen-time usage analytics handlers

use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::NaiveDate;
use serde::Deserialize;
use shepherd_api::UsageStat;
use shepherd_util::EntryId;

use crate::error::{ApiError, ApiResult};
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

    if from > to {
        return Err(ApiError::BadRequest("`from` must not be after `to`".into()));
    }

    // Build a label map from the current policy for display
    let label_map: std::collections::HashMap<String, String> = {
        let eng = state.engine.lock().await;
        eng.policy()
            .entries
            .iter()
            .map(|e| (e.id.as_str().to_owned(), e.label.clone()))
            .collect()
    };

    let mut stats = Vec::new();
    for entry in state.engine.lock().await.policy().entries.iter() {
        let entry_id = entry.id.clone();
        let label = label_map
            .get(entry_id.as_str())
            .cloned()
            .unwrap_or_else(|| entry_id.as_str().to_owned());

        let rows = state
            .store
            .get_usage_range(&entry_id, from, to)
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        for (date, duration) in rows {
            stats.push(UsageStat {
                entry_id: entry_id.clone(),
                label: label.clone(),
                date,
                duration_seconds: duration.as_secs(),
            });
        }
    }

    // Sort by date then entry
    stats.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.entry_id.as_str().cmp(b.entry_id.as_str()))
    });
    Ok(Json(stats))
}

pub async fn get_usage_entry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DateRangeQuery>,
) -> ApiResult<Json<Vec<UsageStat>>> {
    let today = shepherd_util::now().date_naive();
    let from = q.from.unwrap_or(today);
    let to = q.to.unwrap_or(today);

    if from > to {
        return Err(ApiError::BadRequest("`from` must not be after `to`".into()));
    }

    let entry_id = EntryId::new(id);

    let label = {
        let eng = state.engine.lock().await;
        eng.policy()
            .get_entry(&entry_id)
            .map(|e| e.label.clone())
            .ok_or_else(|| ApiError::NotFound(format!("No entry with id '{entry_id}'")))?
    };

    let rows = state
        .store
        .get_usage_range(&entry_id, from, to)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let stats = rows
        .into_iter()
        .map(|(date, duration)| UsageStat {
            entry_id: entry_id.clone(),
            label: label.clone(),
            date,
            duration_seconds: duration.as_secs(),
        })
        .collect();

    Ok(Json(stats))
}
