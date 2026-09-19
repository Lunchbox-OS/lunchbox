//! JSON-RPC pass-through: one HTTP endpoint that dispatches into the
//! trait via `lunchbox_management::dispatch_json`.
//!
//! Wire shape:
//!
//! ```text
//! POST /api/v1/rpc
//! { "method": "<method-name>", "params": <object|null> }
//! ```
//!
//! Success returns 2xx with the trait method's return value as the
//! response body (encoded exactly as `dispatch_json` produces).
//! Failures return 4xx/5xx with `{ "error": <code>, "message": <str> }`.
//! Client transports (the web UI's `callRpc<T>`) throw on non-2xx and
//! deserialise the 2xx body straight into the expected type.
//!
//! The REST routes on the same router keep working — this endpoint
//! exists so consumers that would rather hit a single URL by method
//! name (i.e. every new trait method landing without a matching
//! `.route(...)` line) have somewhere to go.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use lunchbox_management::{ManagementError, RpcDispatchError, dispatch_json};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct RpcRequest {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

pub async fn dispatch(State(state): State<AppState>, Json(body): Json<RpcRequest>) -> Response {
    match dispatch_json(state.svc.as_ref(), &body.method, body.params).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(RpcDispatchError::MethodNotFound(m)) => rpc_error(
            StatusCode::NOT_FOUND,
            "method_not_found",
            format!("unknown method '{m}'"),
        ),
        Err(RpcDispatchError::InvalidParams(msg)) => {
            rpc_error(StatusCode::BAD_REQUEST, "invalid_params", msg)
        }
        Err(RpcDispatchError::Serialization(msg)) => {
            rpc_error(StatusCode::INTERNAL_SERVER_ERROR, "internal", msg)
        }
        Err(RpcDispatchError::Management(e)) => {
            let (status, code, msg) = management_error_to_http(e);
            rpc_error(status, code, msg)
        }
    }
}

fn rpc_error(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "error": code, "message": message.into() })),
    )
        .into_response()
}

fn management_error_to_http(e: ManagementError) -> (StatusCode, &'static str, String) {
    match e {
        ManagementError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
        ManagementError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
        ManagementError::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m),
        ManagementError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
        ManagementError::Unprocessable(m) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable", m),
        ManagementError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", m),
    }
}
