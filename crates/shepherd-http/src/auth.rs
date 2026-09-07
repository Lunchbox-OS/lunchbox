//! Authentication for the management API (issue #156).
//!
//! Three credentials reach this middleware, and they are not equals:
//!
//! - **A session cookie.** What a browser gets after a password login or an
//!   approval on the paired companion. `HttpOnly`, so a script in the page
//!   cannot read it; `SameSite=Strict`, so it is not sent on a cross-site
//!   request at all; `Secure` wherever the listener is TLS.
//! - **A session token as a bearer.** The same session, presented by something
//!   that is not a browser. The web UI's cross-origin "API Server URL" mode
//!   lands here, since a cookie set by one origin is not sent to another.
//! - **A machine token as a bearer.** The static `auth_token` from the config
//!   file, or the token the BLE claim minted. These authenticate a *request*
//!   and cannot open a session: they are for `curl`, the e2e harness and the
//!   companion, none of which have a browser to keep a cookie in. That
//!   demotion is the resolution of the question the June 2026 BLE design left
//!   open — the static token stays, but stops being a way for a person to log
//!   in.
//!
//! ## What happened to open mode
//!
//! It used to be that a device with no static token and no BLE claim served
//! the entire management surface to anyone who could reach the port. That is
//! gone from any device with a credential store: `WebAuth` always exists when
//! shepherdd runs the API, and an unconfigured store answers the setup
//! endpoints and nothing else.
//!
//! [`AuthSources::is_open`] survives for embeddings that construct a router
//! with no store at all — the tests, and anything else that wants the trait
//! over HTTP without the credential machinery. Reaching that shape is
//! deliberately awkward: the fields are private, [`AuthSources::new`] demands
//! a store, and the only way to get one without is
//! [`AuthSources::without_credential_store`], which says so in its name at
//! every call site. There is no `Default`, because a defaulted `AuthSources`
//! would be the fail-open shape and `unwrap_or_default()` is exactly the kind
//! of line that gets written without noticing.

use axum::{
    extract::{ConnectInfo, Request},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use shepherd_management::{AdminAuthority, WebAuth};
use std::net::SocketAddr;
use std::sync::Arc;

/// Name of the session cookie. Prefixed so it cannot be confused with anything
/// an activity's browser profile might set on the same host.
pub const SESSION_COOKIE: &str = "shepherd_session";

/// Who the request turned out to be.
///
/// Put in the request's extensions by [`require_auth`], so a handler can tell
/// a browser session from a machine token — the sessions list marks "this
/// one", and sign-out needs to know which session to end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    /// A live web session; the string is its public id.
    Session { id: String, token: String },
    /// A machine credential: the config's `auth_token`, or the BLE admin's.
    Machine,
    /// No credential was required, because this router has no credential
    /// store. Never reachable on a device.
    Open,
}

/// The credentials this router will accept.
///
/// Fields are private and there is no `Default`: the shape with no credential
/// store is the fail-open one, and it should be impossible to reach by
/// forgetting something. Build one with [`Self::new`], or say
/// [`Self::without_credential_store`] out loud.
#[derive(Clone)]
pub struct AuthSources {
    /// Static config token from `[service.management_api].auth_token`.
    static_token: Option<String>,
    /// The BLE claim's minted token, read fresh on every request because a
    /// factory reset rotates it.
    admin: Option<Arc<dyn AdminAuthority>>,
    /// The web credential store. `Some` on any device; `None` only where a
    /// router was built without one.
    web: Option<Arc<WebAuth>>,
    /// Whether the listener is TLS, which decides the `Secure` attribute on
    /// the session cookie. A `Secure` cookie on a plaintext origin is simply
    /// dropped by the browser, so this has to follow the listener rather than
    /// being hardcoded to the safer-sounding value.
    secure_cookies: bool,
}

impl AuthSources {
    /// The shape every device has: a credential store, so an unconfigured
    /// device is closed rather than open.
    pub fn new(web: Arc<WebAuth>) -> Self {
        Self {
            static_token: None,
            admin: None,
            web: Some(web),
            secure_cookies: false,
        }
    }

    /// A router with **no** credential store, which is open when nothing else
    /// authenticates it. The pre-#156 shape, kept for the `shepherd-http`
    /// tests and for an embedding that wants the management trait over HTTP
    /// without the login machinery.
    ///
    /// Never correct on a device. `shepherdd` cannot reach it:
    /// [`crate::HttpServer::with_web_auth`] takes a store rather than an
    /// `Option`, and [`crate::HttpServer::run`] refuses to serve without one.
    pub fn without_credential_store() -> Self {
        Self {
            static_token: None,
            admin: None,
            web: None,
            secure_cookies: false,
        }
    }

    /// The machine credential from `[service.management_api].auth_token`.
    pub fn with_static_token(mut self, token: Option<String>) -> Self {
        self.static_token = token;
        self
    }

    /// The BLE claim's authority, whose token is read fresh per request.
    pub fn with_admin(mut self, admin: Option<Arc<dyn AdminAuthority>>) -> Self {
        self.admin = admin;
        self
    }

    /// Whether to mark the session cookie `Secure`. Follows the listener: a
    /// `Secure` cookie on a plaintext origin is dropped by the browser.
    pub fn with_secure_cookies(mut self, secure: bool) -> Self {
        self.secure_cookies = secure;
        self
    }

    /// The credential store, for the handlers that mint and revoke sessions.
    pub fn web(&self) -> Option<&Arc<WebAuth>> {
        self.web.as_ref()
    }

    /// Whether a minted cookie should carry `Secure`.
    pub fn secure_cookies(&self) -> bool {
        self.secure_cookies
    }

    /// True when this router has nothing to authenticate against.
    ///
    /// Only possible without a credential store: with one, an unconfigured
    /// device is *closed* (the setup endpoints are the way in), not open.
    fn is_open(&self) -> bool {
        self.web.is_none()
            && self.static_token.is_none()
            && self
                .admin
                .as_ref()
                .and_then(|a| a.current_http_token())
                .is_none()
    }

    /// Match a bearer token against the machine credentials. Session tokens
    /// are resolved separately, because resolving one has the side effect of
    /// bumping its `last_seen`.
    fn matches_machine(&self, presented: &str) -> bool {
        if let Some(t) = &self.static_token
            && constant_time_eq(presented, t)
        {
            return true;
        }
        if let Some(a) = &self.admin
            && let Some(t) = a.current_http_token()
            && constant_time_eq(presented, &t)
        {
            return true;
        }
        false
    }

    /// Resolve whatever the request presented.
    pub fn authenticate(&self, req: &Request) -> Option<Identity> {
        // Cookie first: a browser that has one is the common case, and a
        // browser never sends a bearer header of its own accord.
        if let Some(web) = &self.web
            && let Some(token) = session_cookie(req)
            && let Some(id) = web.resolve(&token)
        {
            return Some(Identity::Session { id, token });
        }

        if let Some(presented) = bearer(req) {
            if let Some(web) = &self.web
                && let Some(id) = web.resolve(&presented)
            {
                return Some(Identity::Session {
                    id,
                    token: presented,
                });
            }
            if self.matches_machine(&presented) {
                return Some(Identity::Machine);
            }
        }

        if self.is_open() {
            return Some(Identity::Open);
        }
        None
    }
}

/// Read the session cookie out of the `Cookie` header.
fn session_cookie(req: &Request) -> Option<String> {
    let header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    header.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == SESSION_COOKIE).then(|| value.trim().to_string())
    })
}

fn bearer(req: &Request) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_owned)
}

/// The address the request came from, for the login throttle and the sessions
/// list. `"unknown"` where the router was built without connect info — the
/// throttle then counts every such caller as one peer, which is the safe
/// direction to be wrong in.
pub fn peer_of(extensions: &axum::http::Extensions) -> String {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Reject a cross-origin state-changing request that authenticated by cookie.
///
/// `SameSite=Strict` already means a browser will not send the cookie
/// cross-site, so this is the belt to that pair of braces — it costs two
/// header reads and covers the browser that gets `SameSite` wrong. Bearer
/// callers are exempt: nothing attaches a bearer header automatically, so a
/// cross-site page cannot forge one.
fn origin_is_foreign(req: &Request) -> bool {
    if matches!(
        req.method(),
        &Method::GET | &Method::HEAD | &Method::OPTIONS
    ) {
        return false;
    }
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        // No `Origin` at all: a non-browser client, or a same-origin request
        // from a browser old enough not to send one on same-origin POSTs.
        return false;
    };
    let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    // Compare authorities, not schemes: the listener may be behind a TLS
    // terminator that rewrote the scheme, and `Host` never carries one.
    let origin_authority = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    origin_authority != host
}

pub async fn require_auth(req: Request, next: Next) -> Response {
    // No sources in the extensions means the layer that inserts them is not
    // on this route. That used to `unwrap_or_default()` into the open shape;
    // a router misassembled that way now refuses every request instead of
    // serving the management surface to anyone who can reach the port.
    let Some(sources) = req.extensions().get::<AuthSources>().cloned() else {
        return unauthorized();
    };

    let Some(identity) = sources.authenticate(&req) else {
        return unauthorized();
    };

    if matches!(identity, Identity::Session { .. }) && origin_is_foreign(&req) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "forbidden",
                "message": "Cross-origin request refused"
            })),
        )
            .into_response();
    }

    let mut req = req;
    req.extensions_mut().insert(identity);
    next.run(req).await
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({
            "error": "unauthorized",
            "message": "Sign in to the management UI, or present a machine token"
        })),
    )
        .into_response()
}

/// Compare two secrets without a length-dependent early exit.
fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Render the `Set-Cookie` value that carries a session.
///
/// `SameSite=Strict` rather than `Lax`: there is no cross-site flow into this
/// UI that needs to arrive authenticated, and `Strict` is the attribute that
/// makes CSRF a non-question rather than a mitigated one.
pub fn session_cookie_header(token: &str, max_age: std::time::Duration, secure: bool) -> String {
    let mut cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        max_age.as_secs()
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The `Set-Cookie` that clears the session — sign-out, and any response that
/// discovers the presented cookie is dead.
pub fn clear_cookie_header(secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}
