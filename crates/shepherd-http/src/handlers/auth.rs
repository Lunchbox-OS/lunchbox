//! The login endpoints (issue #156).
//!
//! These are the only routes under `/api/v1` that a request may reach without
//! a credential, so the split between the two halves of this file is
//! load-bearing:
//!
//! - **Pre-auth**: `status`, `setup`, `login`, `request`, `poll`. Everything
//!   here is rate-limited by the store's own throttle, and everything here is
//!   careful to say as little as possible — "incorrect password" and
//!   "incorrect setup code" are the same 403 to a caller who is guessing.
//! - **Post-auth**: `session`, `signout`, `sessions`, `revoke`. Ordinary
//!   authenticated handlers; they live here rather than in the RPC dispatch
//!   because they are about the *connection*, and the RPC trait has no idea
//!   which session is asking.
//!
//! Deliberately not RPC methods. The BLE transport has no use for a login —
//! by the time a GATT write lands, the peer is bonded — and exposing a login
//! endpoint there would be a second door into the same room. What the
//! companion *does* get is the approval half, which is on the trait.

use axum::{
    Json,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use shepherd_management::{
    LoginPoll, MintedSession, WebAuthError, WebAuthStatus, WebSessionInfo, label_from_user_agent,
};

use crate::auth::{AuthSources, Identity, clear_cookie_header, peer_of, session_cookie_header};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SetupBody {
    /// The six digits on the device's own screen.
    pub code: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub password: String,
}

#[derive(Deserialize)]
pub struct PollBody {
    /// The capability handed back by `request`. Not the six digits — those are
    /// for a human to compare, and are not a secret.
    pub poll_token: String,
}

#[derive(Serialize)]
pub struct RequestedLogin {
    pub poll_token: String,
    /// The digits to put in front of the person holding the phone.
    pub code: String,
    pub expires_at: chrono::DateTime<chrono::Local>,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PollAnswer {
    Pending,
    Approved { session: WebSessionInfo },
    Denied,
    Expired,
}

// ---------------------------------------------------------------------------
// Pre-auth
// ---------------------------------------------------------------------------

/// What a browser may know before it authenticates: whether there is a
/// password yet, and whether the phone can approve a login.
///
/// Reachable by anyone who can reach the port, deliberately — the login page
/// has to render something, and this is the minimum it needs to know which
/// form to show. It says nothing about the device, the child, or the policy.
pub async fn status(State(state): State<AppState>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let Some(web) = web(&parts) else {
        // No credential store: this router is open by construction, so tell
        // the client there is nothing to log into rather than 500ing.
        return Json(WebAuthStatus {
            configured: true,
            companion_available: false,
        })
        .into_response();
    };
    let _ = state;
    Json(web.status()).into_response()
}

/// First-run enrolment: the code from the TV buys the right to set a password,
/// and sets a session in the same exchange so the browser is not immediately
/// asked to log in with the password it just chose.
pub async fn setup(req: Request) -> Response {
    let (parts, body) = match split::<SetupBody>(req).await {
        Ok(pair) => pair,
        Err(e) => return e.into_response(),
    };
    let (Some(web), sources) = (web(&parts), sources(&parts)) else {
        return not_configured();
    };
    let label = label_from_user_agent(user_agent(&parts).as_deref());
    match web.complete_enrolment(
        &body.code,
        &body.password,
        &label,
        &peer_of(&parts.extensions),
    ) {
        Ok(minted) => session_response(minted, &sources, StatusCode::OK),
        Err(e) => auth_error(e),
    }
}

/// Password login.
pub async fn login(req: Request) -> Response {
    let (parts, body) = match split::<LoginBody>(req).await {
        Ok(pair) => pair,
        Err(e) => return e.into_response(),
    };
    let (Some(web), sources) = (web(&parts), sources(&parts)) else {
        return not_configured();
    };
    let label = label_from_user_agent(user_agent(&parts).as_deref());
    match web.login(&body.password, &label, &peer_of(&parts.extensions)) {
        Ok(minted) => session_response(minted, &sources, StatusCode::OK),
        Err(e) => auth_error(e),
    }
}

/// Ask the paired companion to approve this browser.
pub async fn request_login(req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let Some(web) = web(&parts) else {
        return not_configured();
    };
    let label = label_from_user_agent(user_agent(&parts).as_deref());
    match web.request_login(&label, &peer_of(&parts.extensions)) {
        Ok((poll_token, info)) => Json(RequestedLogin {
            poll_token,
            code: info.code,
            expires_at: info.expires_at,
        })
        .into_response(),
        Err(e) => auth_error(e),
    }
}

/// Collect the outcome of an approval request.
///
/// Polling rather than a stream: the browser is waiting on a human with a
/// phone, the wait is bounded at two minutes, and a `GET` every couple of
/// seconds needs no connection kept open through whatever is between the two.
pub async fn poll_login(req: Request) -> Response {
    let (parts, body) = match split::<PollBody>(req).await {
        Ok(pair) => pair,
        Err(e) => return e.into_response(),
    };
    let (Some(web), sources) = (web(&parts), sources(&parts)) else {
        return not_configured();
    };
    match web.poll_login(&body.poll_token) {
        LoginPoll::Pending => Json(PollAnswer::Pending).into_response(),
        LoginPoll::Denied => Json(PollAnswer::Denied).into_response(),
        LoginPoll::Expired => Json(PollAnswer::Expired).into_response(),
        LoginPoll::Approved(minted) => {
            let mut response = Json(PollAnswer::Approved {
                session: minted.info.clone(),
            })
            .into_response();
            set_cookie(&mut response, &minted.token, &sources);
            response
        }
    }
}

// ---------------------------------------------------------------------------
// Post-auth
// ---------------------------------------------------------------------------

/// Who am I? Used by the SPA on load to decide between the app and the login
/// page without racing a first data fetch.
pub async fn current_session(req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let Some(web) = web(&parts) else {
        return not_configured();
    };
    let Some(Identity::Session { id, .. }) = parts.extensions.get::<Identity>().cloned() else {
        // A machine token is authenticated but is not a session, and saying so
        // plainly keeps the UI from rendering a "sign out" button that would
        // do nothing.
        return Json(json!({ "session": Value::Null, "machine": true })).into_response();
    };
    let session = web
        .list_sessions(Some(&id))
        .into_iter()
        .find(|s| s.id == id);
    Json(json!({ "session": session, "machine": false })).into_response()
}

pub async fn signout(req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let (Some(web), sources) = (web(&parts), sources(&parts)) else {
        return not_configured();
    };
    if let Some(Identity::Session { token, .. }) = parts.extensions.get::<Identity>() {
        let _ = web.sign_out(token);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&clear_cookie_header(sources.secure_cookies))
            .expect("cookie header is ASCII"),
    );
    response
}

/// Every live session, with the caller's own marked — the list an
/// administrator revokes from.
pub async fn list_sessions(req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let Some(web) = web(&parts) else {
        return not_configured();
    };
    let current = match parts.extensions.get::<Identity>() {
        Some(Identity::Session { id, .. }) => Some(id.clone()),
        _ => None,
    };
    Json(web.list_sessions(current.as_deref())).into_response()
}

pub async fn revoke_session(Path(id): Path<String>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let (Some(web), sources) = (web(&parts), sources(&parts)) else {
        return not_configured();
    };
    let revoking_self = matches!(
        parts.extensions.get::<Identity>(),
        Some(Identity::Session { id: current, .. }) if *current == id
    );
    match web.revoke_session(&id) {
        Ok(()) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            if revoking_self {
                response.headers_mut().insert(
                    header::SET_COOKIE,
                    HeaderValue::from_str(&clear_cookie_header(sources.secure_cookies))
                        .expect("cookie header is ASCII"),
                );
            }
            response
        }
        Err(e) => auth_error(e),
    }
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

use serde_json::Value;

/// Take the request apart so a handler can read its headers *and* its body.
///
/// Axum's `Request` extractor must be last and consumes everything, so a
/// handler cannot ask for `Json<T>` alongside it. These handlers need both —
/// the body carries the credential, the headers carry the User-Agent that
/// becomes the session's label and the peer that the throttle counts — so they
/// take the whole request and split it here.
async fn split<T: DeserializeOwned>(req: Request) -> Result<(Parts, T), BadBody> {
    let (parts, body) = req.into_parts();
    // A login body is a password and maybe six digits. The cap is three orders
    // of magnitude above that and exists so an unauthenticated endpoint cannot
    // be asked to buffer something large.
    let bytes = match axum::body::to_bytes(body, 16 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return Err(BadBody::TooLarge),
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(value) => Ok((parts, value)),
        Err(e) => Err(BadBody::Malformed(e)),
    }
}

/// Why a body could not be read.
///
/// A description rather than the 400 itself: a `Response` is 128 bytes, and an
/// `Err` variant that large is what `clippy::result_large_err` exists to catch
/// — every caller of `split` pays for it on the success path too. The caller
/// turns this into the same response it used to receive.
enum BadBody {
    TooLarge,
    Malformed(serde_json::Error),
}

impl IntoResponse for BadBody {
    fn into_response(self) -> Response {
        match self {
            BadBody::TooLarge => bad_request("Request body too large"),
            BadBody::Malformed(e) => bad_request(format!("Malformed request body: {e}")),
        }
    }
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "bad_request", "message": message.into() })),
    )
        .into_response()
}

fn sources(parts: &Parts) -> AuthSources {
    parts
        .extensions
        .get::<AuthSources>()
        .cloned()
        .unwrap_or_default()
}

fn web(parts: &Parts) -> Option<std::sync::Arc<shepherd_management::WebAuth>> {
    parts.extensions.get::<AuthSources>()?.web.clone()
}

fn user_agent(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn session_response(minted: MintedSession, sources: &AuthSources, status: StatusCode) -> Response {
    let mut response = (status, Json(json!({ "session": minted.info }))).into_response();
    set_cookie(&mut response, &minted.token, sources);
    response
}

/// Attach the session cookie, sized to the session's own remaining life so the
/// browser forgets it at the same moment the device does.
fn set_cookie(response: &mut Response, token: &str, sources: &AuthSources) {
    let max_age = std::time::Duration::from_secs(30 * 24 * 3600);
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&session_cookie_header(
            token,
            max_age,
            sources.secure_cookies,
        ))
        .expect("cookie header is ASCII"),
    );
}

fn not_configured() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "conflict",
            "message": "This server has no web credential store"
        })),
    )
        .into_response()
}

/// Map a store failure onto a status.
///
/// A wrong password and a wrong setup code are the same 403 with the same
/// shape, because the difference is only interesting to somebody who is
/// guessing. A lockout is a 429 with `Retry-After`, because the difference
/// *is* interesting to the parent who mistyped twice and would otherwise keep
/// trying.
fn auth_error(e: WebAuthError) -> Response {
    let (status, code) = match &e {
        WebAuthError::NotConfigured | WebAuthError::AlreadyConfigured => {
            (StatusCode::CONFLICT, "conflict")
        }
        WebAuthError::BadPassword | WebAuthError::BadEnrolmentCode => {
            (StatusCode::FORBIDDEN, "forbidden")
        }
        WebAuthError::LockedOut(_) => (StatusCode::TOO_MANY_REQUESTS, "locked_out"),
        WebAuthError::NoSuchRequest | WebAuthError::NoSuchSession => {
            (StatusCode::NOT_FOUND, "not_found")
        }
        WebAuthError::PasswordTooShort(_) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable"),
        WebAuthError::Store(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    let mut response = (
        status,
        Json(json!({ "error": code, "message": e.to_string() })),
    )
        .into_response();
    if let WebAuthError::LockedOut(remaining) = e {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&remaining.as_secs().max(1).to_string())
                .expect("a number is ASCII"),
        );
    }
    response
}
