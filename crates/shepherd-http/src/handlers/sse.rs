//! Server-Sent Events (SSE) handler for real-time event streaming

use axum::{
    extract::State,
    response::sse::{Event as SseEvent, KeepAlive, Sse},
};
use std::convert::Infallible;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::state::AppState;

pub async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => serde_json::to_string(&event)
            .ok()
            .map(|data| Ok(SseEvent::default().data(data))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
