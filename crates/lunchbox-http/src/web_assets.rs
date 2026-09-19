//! Embedded web UI static assets.
//!
//! Files are embedded from `lunchbox-webui/dist/` at compile time.
//! If the web UI has not been built yet (`npm run build` in `lunchbox-webui/`),
//! the binary embeds nothing and all UI routes return 404.

use axum::{
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../lunchbox-webui/dist"]
#[exclude = ".*"] // skip .gitkeep and other dotfiles
struct Assets;

/// Axum fallback handler — serves embedded static files.
/// Returns `index.html` for any path that doesn't match a file (SPA routing).
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(resp) = serve(path) {
        return resp;
    }

    // Unknown path: return index.html so the SPA router can handle it.
    // If the web UI hasn't been built, this also returns 404 (no index.html).
    serve("index.html").unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;

    let mime = mime_type(path);
    // Hashed asset filenames (e.g. `index.4e3776db.js`) are immutable;
    // index.html must be re-fetched so the browser picks up new hashes.
    let cache = if path.ends_with(".html") {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };

    Some(
        Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, cache)
            .header(header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY)
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
            .body(Body::from(file.data))
            .unwrap(),
    )
}

/// What the management UI's own origin is allowed to load and run.
///
/// It matters more since issue #195 put a file manager on this origin: a file
/// a parent uploads is served back from here, and script running as this
/// origin can call `/api/v1/rpc` with the administrator's cookie attached.
/// Downloads defend themselves — `Content-Disposition: attachment`, `nosniff`,
/// a fixed `application/octet-stream` — and this is the second lock on the
/// same door.
///
/// Three of these are load-bearing and not to be tidied away:
///
/// - **`'wasm-unsafe-eval'`** — the config editor's validator is a ~950 kB
///   wasm module, and without this it does not instantiate at all.
/// - **`'unsafe-inline'` for styles** — MUI's emotion injects `<style>` tags
///   at runtime. Nonces would be better and would mean threading one through
///   a `rust_embed` fallback that serves a fixed byte string.
/// - **`connect-src *`** — `ConnectionSettings` lets this SPA be pointed at a
///   *different* device's `apiBase`, which is how `npm run dev` and a browser
///   managing two devices both work. Narrowing it to `'self'` would break
///   that, and the exfiltration channel it would close is not the one that
///   matters here: `script-src 'self'` is what stops an uploaded file running
///   in the first place.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
     script-src 'self' 'wasm-unsafe-eval'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; \
     font-src 'self' data:; \
     connect-src *; \
     object-src 'none'; \
     base-uri 'none'; \
     frame-ancestors 'none'";

fn mime_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        // The config editor's validator (issue #185). Without this the module
        // is served as `application/octet-stream`, `instantiateStreaming`
        // refuses it, and wasm-bindgen's glue falls back to buffering the
        // whole megabyte — with a console warning naming this line as the bug.
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
