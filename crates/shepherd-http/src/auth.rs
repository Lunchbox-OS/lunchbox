//! Bearer token authentication middleware

use axum::{
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Middleware that checks Bearer token auth if a token is injected as an extension.
/// The token is inserted by the router via `layer(middleware::from_fn(...))`.
pub async fn require_auth(req: Request, next: Next) -> Response {
    // Token stored in extensions by the router layer
    let expected: Option<String> = req.extensions().get::<Option<String>>().cloned().flatten();

    let Some(expected) = expected else {
        // No token configured — allow all requests
        return next.run(req).await;
    };

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_owned);

    if provided.as_deref() != Some(expected.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": "unauthorized",
                "message": "Valid Bearer token required"
            })),
        )
            .into_response();
    }

    next.run(req).await
}
