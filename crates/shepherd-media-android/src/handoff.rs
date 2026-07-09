//! Phone hand-off: a tiny LAN HTTP server so a phone — which has a real
//! keyboard — can hand a library URL to the TV, which does not.
//!
//! [`start`] binds an ephemeral port, spawns a background thread running an
//! `axum` server (the same stack the management API in `shepherd-http` uses),
//! and returns the `http://<lan-ip>:<port>/` URL to show on the TV (as text and
//! a QR code). The phone opens that page, pastes a TOML/M3U/YouTube URL, and
//! submits; the server delivers it over a channel the UI polls, which then
//! pre-fills the add-library form. LAN-only and unauthenticated — a home TV on
//! the same Wi-Fi as the phone, the same trust model as the rest of the app.

use std::net::UdpSocket;
use std::sync::mpsc::{Receiver, Sender};

use axum::extract::State;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;

/// A running hand-off server: the URL to show, and the channel submitted
/// library URLs arrive on. Dropping it lets the server thread wind down (its
/// sends start failing); the socket closes with the process.
pub struct Handoff {
    /// `http://<lan-ip>:<port>/` — shown on the TV and encoded in the QR.
    pub url: String,
    /// Library URLs submitted from the phone.
    pub rx: Receiver<String>,
}

/// Start the hand-off server on an ephemeral port, bound to all interfaces.
pub fn start() -> std::io::Result<Handoff> {
    // Bind synchronously so we know the port before the async runtime spins up;
    // the listener is already listening, so requests queue until it accepts.
    let listener = std::net::TcpListener::bind("0.0.0.0:0")?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let ip = local_ip().unwrap_or_else(|| "0.0.0.0".to_string());
    let url = format!("http://{ip}:{port}/");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                return;
            };
            let app = Router::new()
                .route("/", get(form_page))
                .route("/submit", post(submit))
                .with_state(tx);
            let _ = axum::serve(listener, app).await;
        });
    });
    Ok(Handoff { url, rx })
}

async fn form_page() -> Html<&'static str> {
    Html(FORM_HTML)
}

#[derive(Deserialize)]
struct Submission {
    url: String,
}

/// axum's `Form` extractor parses `application/x-www-form-urlencoded` (percent-
/// and `+`-decoding included), so we just forward the URL to the UI.
async fn submit(
    State(tx): State<Sender<String>>,
    Form(form): Form<Submission>,
) -> Html<&'static str> {
    let url = form.url.trim();
    if !url.is_empty() {
        let _ = tx.send(url.to_string());
    }
    Html(DONE_HTML)
}

/// The LAN IP of the interface that would route outward. "Connecting" a UDP
/// socket sends nothing but picks the source address, which is that IP.
fn local_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

/// The QR modules for `text` as `(width, dark-flags)` in row-major order, or
/// `None` if the text won't fit a QR code.
pub fn qr_matrix(text: &str) -> Option<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Some((width, dark))
}

const FORM_HTML: &str = r#"<!doctype html>
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Add a library</title>
<body style="font-family:system-ui,sans-serif;max-width:32rem;margin:2rem auto;padding:0 1rem;line-height:1.5">
<h2>Add a library to the TV</h2>
<p>Paste a TOML or M3U file URL, or a YouTube playlist URL:</p>
<form method="POST" action="/submit">
<input name="url" type="url" placeholder="https://…" autofocus autocapitalize="off" autocorrect="off"
 style="width:100%;box-sizing:border-box;font-size:1.2rem;padding:.6rem">
<p><button style="font-size:1.1rem;padding:.6rem 1.2rem">Send to TV</button></p>
</form>
</body>"#;

const DONE_HTML: &str = r#"<!doctype html>
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sent</title>
<body style="font-family:system-ui,sans-serif;max-width:32rem;margin:2rem auto;padding:0 1rem;line-height:1.5">
<h2>Sent ✓</h2>
<p>Return to the TV to finish adding the library.</p>
</body>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn builds_a_qr() {
        let (w, dark) = qr_matrix("http://192.168.1.5:8099/").unwrap();
        assert!(w >= 21); // smallest QR is 21x21
        assert_eq!(dark.len(), w * w);
    }

    #[test]
    fn submitting_a_form_delivers_the_url() {
        let handoff = start().unwrap();
        let port = handoff
            .url
            .rsplit(':')
            .next()
            .unwrap()
            .trim_end_matches('/');
        // POST as a browser form would; the server is already listening.
        let resp = ureq::post(&format!("http://127.0.0.1:{port}/submit"))
            .send_form(&[("url", "https://example.com/lib.toml")])
            .unwrap();
        assert_eq!(resp.status(), 200);
        let got = handoff.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, "https://example.com/lib.toml");
    }

    #[test]
    fn get_serves_the_form_page() {
        let handoff = start().unwrap();
        let port = handoff
            .url
            .rsplit(':')
            .next()
            .unwrap()
            .trim_end_matches('/');
        let body = ureq::get(&format!("http://127.0.0.1:{port}/"))
            .call()
            .unwrap()
            .into_string()
            .unwrap();
        assert!(body.contains("Add a library to the TV"));
    }
}
