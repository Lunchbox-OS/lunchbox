//! Session control handlers (launch, stop, extend)

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use shepherd_api::{Event, EventPayload, ReasonCode, SessionEndReason, SessionInfo, StopMode};
use shepherd_core::{LaunchDecision, StopDecision};
use shepherd_host_api::SpawnOptions;
use shepherd_util::{EntryId, MonotonicInstant};
use std::time::Duration;
use tracing::warn;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub async fn get_current(State(state): State<AppState>) -> ApiResult<Json<Option<SessionInfo>>> {
    let eng = state.engine.lock().await;
    Ok(Json(
        eng.current_session()
            .map(|s| s.to_session_info(MonotonicInstant::now())),
    ))
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
    let now = shepherd_util::now();
    let now_mono = MonotonicInstant::now();

    let decision = {
        let eng = state.engine.lock().await;
        eng.request_launch(&entry_id, now)
    };

    match decision {
        LaunchDecision::Denied { reasons } => {
            (StatusCode::OK, Json(LaunchResponse::Denied { reasons })).into_response()
        }

        LaunchDecision::Approved(plan) => {
            let session_id = plan.session_id.clone();
            let plan_label = plan.label.clone();

            // Register session in engine
            {
                let mut eng = state.engine.lock().await;
                eng.start_session(plan, now, now_mono);
            }

            // Determine spawn options
            let (entry_kind, spawn_opts, needs_hidpi) = {
                let eng = state.engine.lock().await;
                let entry = eng.policy().get_entry(&entry_id);
                let kind = entry.map(|e| e.kind.clone());
                let input_compat = entry.map(|e| e.input_compat.clone()).unwrap_or_default();
                let input_compat_options =
                    entry.map(|e| e.input_compat_options).unwrap_or_default();
                let needs_hidpi = entry.is_some_and(|e| e.xwayland_native_resolution);
                let opts = if eng.policy().service.capture_child_output {
                    let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
                    let filename = format!(
                        "{}_{}.log",
                        entry_id.as_str().replace(['/', '\\', ' '], "_"),
                        timestamp
                    );
                    SpawnOptions {
                        capture_stdout: true,
                        capture_stderr: true,
                        log_path: Some(eng.policy().service.child_log_dir.join(filename)),
                        input_compat,
                        input_compat_options,
                        ..Default::default()
                    }
                } else {
                    SpawnOptions {
                        input_compat,
                        input_compat_options,
                        ..Default::default()
                    }
                };
                (kind, opts, needs_hidpi)
            };

            let Some(kind) = entry_kind else {
                let mut eng = state.engine.lock().await;
                eng.notify_session_exited(Some(-1), now_mono, now);
                return (
                    StatusCode::NOT_FOUND,
                    Json(LaunchResponse::Denied {
                        reasons: vec![ReasonCode::Disabled {
                            reason: Some("Entry not found".into()),
                        }],
                    }),
                )
                    .into_response();
            };

            // Apply the XWayland HiDPI workaround before spawning so the
            // client sees the native scale on first map (mirror of the IPC
            // launch path in shepherdd::main).
            if needs_hidpi {
                state.hidpi.apply().await;
            }

            match state
                .host
                .spawn(session_id.clone(), &kind, spawn_opts)
                .await
            {
                Ok(handle) => {
                    let deadline = {
                        let mut eng = state.engine.lock().await;
                        eng.attach_host_handle(handle);
                        eng.current_session().and_then(|s| s.deadline)
                    };

                    (state.broadcast_fn)(Event::new(EventPayload::SessionStarted {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        label: plan_label,
                        deadline,
                    }));

                    (
                        StatusCode::OK,
                        Json(LaunchResponse::Approved {
                            session_id: session_id.to_string(),
                            deadline,
                        }),
                    )
                        .into_response()
                }
                Err(e) => {
                    warn!(error = %e, "Spawn failed from HTTP launch");
                    // Roll back the scale change so the launcher reappears
                    // with a correctly-sized HUD.
                    state.hidpi.restore().await;
                    let mut eng = state.engine.lock().await;
                    eng.notify_session_exited(Some(-1), now_mono, now);
                    let snap = eng.get_state();
                    drop(eng);
                    (state.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(LaunchResponse::Denied {
                            reasons: vec![ReasonCode::Disabled {
                                reason: Some(format!("Spawn failed: {e}")),
                            }],
                        }),
                    )
                        .into_response()
                }
            }
        }
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
    let now = shepherd_util::now();
    let now_mono = MonotonicInstant::now();

    let (handle, decision) = {
        let mut eng = state.engine.lock().await;
        let handle = eng.current_session().and_then(|s| s.host_handle.clone());
        let reason = match mode {
            StopMode::Graceful => SessionEndReason::UserStop,
            StopMode::Force => SessionEndReason::AdminStop,
        };
        let decision = eng.stop_current(reason, now_mono, now);
        (handle, decision)
    };

    match decision {
        StopDecision::NoActiveSession => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no_active_session" })),
        )
            .into_response(),
        StopDecision::Stopped(result) => {
            (state.broadcast_fn)(Event::new(EventPayload::SessionEnded {
                session_id: result.session_id,
                entry_id: result.entry_id,
                reason: result.reason,
                duration: result.duration,
            }));
            let snap = state.engine.lock().await.get_state();
            (state.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));

            // Restore output scale / HUD factor before tearing down the
            // process so the launcher reappears at its normal size; idempotent
            // when no workaround was active.
            state.hidpi.restore().await;

            if let Some(h) = handle {
                let host_mode = match mode {
                    StopMode::Graceful => shepherd_host_api::StopMode::Graceful {
                        timeout: Duration::from_secs(5),
                    },
                    StopMode::Force => shepherd_host_api::StopMode::Force,
                };
                let _ = state.host.stop(&h, host_mode).await;
            }

            StatusCode::NO_CONTENT.into_response()
        }
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
    let now = shepherd_util::now();
    let now_mono = MonotonicInstant::now();

    let new_deadline = {
        let mut eng = state.engine.lock().await;

        if !eng.has_active_session() {
            return Err(ApiError::NotFound("No active session".into()));
        }

        if body.seconds >= 0 {
            eng.extend_current(Duration::from_secs(body.seconds as u64), now_mono, now)
        } else {
            eng.reduce_current(
                Duration::from_secs(body.seconds.unsigned_abs()),
                now_mono,
                now,
            )
        }
    };

    let snap = state.engine.lock().await.get_state();
    (state.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));

    Ok(Json(ExtendResponse { new_deadline }))
}
