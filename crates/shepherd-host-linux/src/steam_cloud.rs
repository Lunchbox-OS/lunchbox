//! Detect and dismiss Steam's offline "Unable to Sync" Steam Cloud modal.
//!
//! When a Steam game is launched while the host is offline and the game has
//! local saves that could not be uploaded, Steam blocks the launch behind an
//! "Unable to Sync" modal (`Play anyway` / `Cancel`). On a kiosk that modal is
//! never visible — it renders inside the main Steam window, which is hidden —
//! so the launch hangs on the launcher spinner forever (issue #50).
//!
//! The only reliable handle on that modal is Steam's CEF (Chromium) remote
//! debugging endpoint: with `<SteamRoot>/.cef-enable-remote-debugging` present,
//! Steam exposes the Chrome DevTools Protocol on a loopback port. We enumerate
//! the page targets, look for the modal in the DOM, and click "Play anyway".
//!
//! This module speaks just enough of the protocol by hand: a plain HTTP `GET
//! /json` to list targets, then a WebSocket `Runtime.evaluate` per page. It
//! never talks to anything but loopback.

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

/// Default loopback port Steam's CEF debugger listens on.
pub const DEFAULT_CEF_PORT: u16 = 8080;

/// Per-target operation timeout so one wedged page can't stall the watchdog.
const TARGET_TIMEOUT: Duration = Duration::from_secs(3);

/// JavaScript evaluated in each page: if the Steam Cloud "Unable to Sync" modal
/// is present, click its affirmative button and report `clicked`. Matching is
/// kept loose (button by accessible text, modal by body text) so minor UI
/// changes degrade to "no-modal" rather than a panic.
const DETECT_AND_CLICK_JS: &str = r#"
(function () {
  try {
    var nodes = Array.prototype.slice.call(
      document.querySelectorAll('button,[role="button"]'));
    var play = nodes.filter(function (e) {
      return /play anyway/i.test(e.innerText || e.textContent || '');
    })[0];
    var body = document.body ? (document.body.innerText || '') : '';
    var isModal = /unable to sync|steam cloud/i.test(body) && !!play;
    if (isModal) { play.click(); return 'clicked'; }
    return 'no-modal';
  } catch (e) { return 'error'; }
})()
"#;

/// What a resolution attempt found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The modal was present and "Play anyway" was clicked.
    Clicked,
    /// CEF was reachable but no cloud modal is currently showing.
    NoModal,
}

#[derive(Debug, thiserror::Error)]
pub enum SteamCloudError {
    #[error("CEF debugger not reachable: {0}")]
    Unreachable(String),
    #[error("CEF debugger protocol error: {0}")]
    Protocol(String),
}

#[derive(Debug, Deserialize)]
struct CefTarget {
    #[serde(default)]
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
}

/// Look for the offline Steam Cloud modal and, if present, click "Play anyway".
///
/// Returns `Clicked` if it dismissed the modal, `NoModal` if CEF was reachable
/// but nothing needed clicking, or an error if the debug endpoint could not be
/// reached or spoke unexpectedly. The caller treats any non-`Clicked` result as
/// "not resolved yet" and keeps waiting until its own deadline.
pub async fn try_resolve_cloud_modal(port: u16) -> Result<ResolveOutcome, SteamCloudError> {
    let targets = match timeout(TARGET_TIMEOUT, fetch_targets(port)).await {
        Ok(Ok(targets)) => targets,
        Ok(Err(e)) => return Err(SteamCloudError::Unreachable(e.to_string())),
        Err(_) => return Err(SteamCloudError::Unreachable("GET /json timed out".into())),
    };

    let mut saw_page = false;
    for target in targets {
        if target.kind != "page" {
            continue;
        }
        let Some(ws_url) = target.ws_url else {
            continue;
        };
        saw_page = true;
        match timeout(TARGET_TIMEOUT, evaluate_on_target(&ws_url)).await {
            Ok(Ok(value)) if value == "clicked" => return Ok(ResolveOutcome::Clicked),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => debug!(error = %e, "CEF evaluate failed on a page target"),
            Err(_) => debug!("CEF evaluate timed out on a page target"),
        }
    }

    if saw_page {
        Ok(ResolveOutcome::NoModal)
    } else {
        Err(SteamCloudError::Protocol("no page targets".into()))
    }
}

/// `GET /json` on the loopback debugger and parse the target list.
///
/// We read incrementally and return as soon as the response body parses as the
/// target array, rather than waiting for EOF: CEF's debug server uses HTTP
/// keep-alive and may not honour `Connection: close`, so a `read_to_end` would
/// block forever after the body has already arrived.
async fn fetch_targets(port: u16) -> std::io::Result<Vec<CefTarget>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    // The Host header must carry the port: CEF builds each target's
    // `webSocketDebuggerUrl` from it, and a portless Host yields portless ws://
    // URLs that then fail to connect.
    let request = format!(
        "GET /json HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        // Try to parse whatever body we have so far.
        if let Some(idx) = find_subslice(&buf, b"\r\n\r\n") {
            let body = &buf[idx + 4..];
            let mut de = serde_json::Deserializer::from_slice(body);
            if let Ok(targets) = Vec::<CefTarget>::deserialize(&mut de) {
                return Ok(targets);
            }
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            // EOF without a parseable body.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incomplete or invalid /json response",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Open the page's WebSocket and run the detect-and-click expression, returning
/// the string the expression produced (`clicked` / `no-modal` / `error`).
async fn evaluate_on_target(ws_url: &str) -> Result<String, SteamCloudError> {
    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| SteamCloudError::Protocol(e.to_string()))?;

    let enable = serde_json::json!({"id": 1, "method": "Runtime.enable"});
    ws.send(Message::Text(enable.to_string()))
        .await
        .map_err(|e| SteamCloudError::Protocol(e.to_string()))?;

    let eval = serde_json::json!({
        "id": 2,
        "method": "Runtime.evaluate",
        "params": { "expression": DETECT_AND_CLICK_JS, "returnByValue": true },
    });
    ws.send(Message::Text(eval.to_string()))
        .await
        .map_err(|e| SteamCloudError::Protocol(e.to_string()))?;

    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| SteamCloudError::Protocol(e.to_string()))?;
        let Message::Text(text) = msg else {
            continue;
        };
        let Ok(reply) = serde_json::from_str::<serde_json::Value>(text.as_str()) else {
            continue;
        };
        if reply.get("id").and_then(|v| v.as_u64()) == Some(2) {
            let value = reply
                .pointer("/result/result/value")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let _ = ws.close(None).await;
            return Ok(value);
        }
    }
    Err(SteamCloudError::Protocol(
        "connection closed before result".into(),
    ))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Path to the flag file that enables CEF remote debugging for the snap Steam.
///
/// Touching this before Steam starts makes the client expose the DevTools
/// protocol on [`DEFAULT_CEF_PORT`]. Returns `None` if the home directory can't
/// be resolved.
pub fn cef_debug_flag_path() -> Option<std::path::PathBuf> {
    dirs::home_dir()
        .map(|home| home.join("snap/steam/common/.local/share/Steam/.cef-enable-remote-debugging"))
}

/// Best-effort creation of the CEF debug flag file. Logs and continues on
/// failure — Steam still launches, we just can't auto-resolve the modal.
pub fn ensure_cef_debug_enabled() {
    let Some(path) = cef_debug_flag_path() else {
        warn!("Could not resolve home directory for Steam CEF debug flag");
        return;
    };
    if path.exists() {
        return;
    }
    match std::fs::File::create(&path) {
        Ok(_) => debug!(path = %path.display(), "Enabled Steam CEF remote debugging"),
        Err(e) => warn!(error = %e, path = %path.display(),
            "Failed to enable Steam CEF remote debugging"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devtools_json_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n[{\"type\":\"page\",\"webSocketDebuggerUrl\":\"ws://localhost:8080/devtools/page/AB\"},{\"type\":\"other\"}]";
        let body = &raw[find_subslice(raw, b"\r\n\r\n").unwrap() + 4..];
        let mut de = serde_json::Deserializer::from_slice(body);
        let targets = Vec::<CefTarget>::deserialize(&mut de).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].kind, "page");
        assert_eq!(
            targets[0].ws_url.as_deref(),
            Some("ws://localhost:8080/devtools/page/AB")
        );
        assert!(targets[1].ws_url.is_none());
    }

    #[tokio::test]
    async fn unreachable_port_is_an_error() {
        // Port 1 is privileged and nothing listens there; connect must fail.
        let result = try_resolve_cloud_modal(1).await;
        assert!(matches!(result, Err(SteamCloudError::Unreachable(_))));
    }

    /// Live check against a real Steam with CEF debugging enabled. Ignored by
    /// default; run with the offline "Unable to Sync" modal showing:
    ///   cargo test -p shepherd-host-linux -- --ignored live_resolve_clicks
    #[tokio::test]
    #[ignore]
    async fn live_resolve_clicks() {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match try_resolve_cloud_modal(DEFAULT_CEF_PORT).await {
                Ok(ResolveOutcome::Clicked) => {
                    eprintln!("clicked Play anyway");
                    return;
                }
                other => eprintln!("not yet: {other:?}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "modal not dismissed within 20s"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
