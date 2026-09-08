//! Embedded web UI static assets.
//!
//! Files are embedded from `shepherd-webui/dist/` at compile time.
//! If the web UI has not been built yet (`npm run build` in `shepherd-webui/`),
//! the binary embeds nothing and all UI routes return 404.

use axum::{
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../shepherd-webui/dist"]
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
            .body(Body::from(file.data))
            .unwrap(),
    )
}

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
