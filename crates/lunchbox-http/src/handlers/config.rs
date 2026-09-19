//! The policy file, over HTTP (issue #185).
//!
//! Two routes, and they are deliberately **not** `ManagementService` RPCs:
//!
//! ```text
//! GET /api/v1/config    -> 200 text/plain  + ETag
//! PUT /api/v1/config    <- text/plain, If-Match required
//! ```
//!
//! `#[management_rpc]` turns every async trait method into a `dispatch_json`
//! arm, which BLE serves too — and a policy is tens of kilobytes against BLE's
//! 16 KiB frame cap, so a config on that surface would be a method that exists
//! and cannot work. #156 kept the login exchange off the trait for the mirror
//! reason (a transport that has already authenticated its peer has no use for
//! a login), and this is the same move. What the trait does carry is
//! [`ManagementService::read_policy`] and
//! [`ManagementService::write_policy`], which are synchronous and therefore
//! skipped by the macro.
//!
//! ## Why `ETag` and `If-Match`
//!
//! A device's policy has three writers: `sudoedit`, `shepherd install policy`,
//! and now this. An editor open in a browser holds a copy that any of the
//! other two can invalidate, and without a precondition the browser's save
//! would silently discard their work. `If-Match` is required rather than
//! optional so that forgetting it is a 428 rather than a clobber; a caller
//! that genuinely means "overwrite whatever is there" says `If-Match: *`.
//!
//! ## What this does not do
//!
//! It does not reload. The write lands through a rename, which the state
//! custodian's watch — or lunchboxd's own, on a device without one — turns
//! into a reload within a second, exactly as it does for the other two
//! writers. See [`ManagementService::write_policy`].

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use lunchbox_management::{ManagementError, ManagementService};
use std::sync::Arc;

use crate::state::AppState;

/// Hand over the policy file's exact bytes.
///
/// `text/plain` rather than `application/toml`: the body is what an editor
/// puts in a text buffer, and no browser does anything useful with the latter.
/// `no-store`, because a cached policy is a policy someone edits a stale copy
/// of.
pub async fn read(State(state): State<AppState>) -> Response {
    match blocking(state.svc.clone(), |svc| svc.read_policy()).await {
        Ok(doc) => (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    "text/plain; charset=utf-8".to_string(),
                ),
                (header::ETAG, quote(&doc.version)),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            doc.text,
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

/// Replace the policy file.
///
/// The body is TOML. Validation, the precondition check and the write all
/// happen inside [`ManagementService::write_policy`], which is where they can
/// be tested without a socket.
pub async fn write(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    let Some(if_match) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()) else {
        return (
            StatusCode::PRECONDITION_REQUIRED,
            Json(json!({
                "error": "precondition_required",
                "message": "Send If-Match with the ETag from GET /api/v1/config, \
                            or If-Match: * to overwrite whatever is there",
            })),
        )
            .into_response();
    };
    // `*` is HTTP's "any current version will do", which here means an
    // administrator who has decided to overwrite. Anything else is compared to
    // the file, after unwrapping the quotes an ETag is spelled with; a weak
    // validator (`W/"…"`) is not something we ever emit, so it is not
    // something we accept.
    let expected = (if_match.trim() != "*").then(|| unquote(if_match).to_string());

    let text = body;
    match blocking(state.svc.clone(), move |svc| {
        svc.write_policy(&text, expected.as_deref())
    })
    .await
    {
        Ok(doc) => (
            StatusCode::OK,
            [(header::ETAG, quote(&doc.version))],
            Json(json!({ "version": doc.version })),
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

/// Run one of the two synchronous policy calls off the runtime's worker.
///
/// `ProtectedFiles` is a blocking interface — on a device each call is a round
/// trip to the state custodian's socket — and blocking an axum worker on it
/// would stall every other request the runtime is serving.
async fn blocking<T: Send + 'static>(
    svc: Arc<dyn ManagementService>,
    f: impl FnOnce(&dyn ManagementService) -> Result<T, ManagementError> + Send + 'static,
) -> Result<T, ManagementError> {
    match tokio::task::spawn_blocking(move || f(svc.as_ref())).await {
        Ok(result) => result,
        Err(e) => Err(ManagementError::Internal(format!(
            "The policy operation panicked: {e}"
        ))),
    }
}

/// `"abc"`, the way an `ETag` is spelled.
fn quote(version: &str) -> String {
    format!("\"{version}\"")
}

/// The inverse, tolerant of a caller that sent the tag bare.
fn unquote(value: &str) -> &str {
    value.trim().trim_matches('"')
}

fn error_response(e: ManagementError) -> Response {
    let (status, code, message) = match e {
        // Not 409: this one is always an `If-Match` that did not match, and
        // 412 is the answer HTTP has for that. A client seeing it should
        // re-read the file rather than retry the same body.
        ManagementError::Conflict(m) => (StatusCode::PRECONDITION_FAILED, "precondition_failed", m),
        ManagementError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
        ManagementError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
        ManagementError::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m),
        ManagementError::Unprocessable(m) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable", m),
        ManagementError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", m),
    };
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}
