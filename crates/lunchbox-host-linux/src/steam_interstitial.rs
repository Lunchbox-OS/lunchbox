//! Detect and dismiss Steam's blocking "launch interstitials".
//!
//! Between a launch request and the game actually starting, Steam can show a
//! blocking modal — an "interstitial" in Steam's own terminology — that the
//! user must acknowledge. Examples: the offline "Unable to Sync" Steam Cloud
//! warning, and the "Grab a controller…" advisory for controller-recommended
//! games. On a kiosk these render inside Steam's (hidden) CEF UI, so the launch
//! hangs on the launcher spinner forever (issue #50).
//!
//! The only reliable handle on them is Steam's CEF (Chromium) remote-debugging
//! endpoint: with `<SteamRoot>/.cef-enable-remote-debugging` present, Steam
//! exposes the Chrome DevTools Protocol on a loopback port. We enumerate the
//! page targets, match each enabled interstitial's signature against the DOM,
//! and click its affirmative button.
//!
//! Which interstitials we're allowed to dismiss is policy
//! ([`InterstitialKind`], from config); the per-kind DOM signatures live here.
//! This module speaks just enough of the protocol by hand: a plain HTTP `GET
//! /json` to list targets, then a WebSocket `Runtime.evaluate` per page. It
//! never talks to anything but loopback.

use futures_util::{SinkExt, StreamExt};
use lunchbox_api::InterstitialKind;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tracing::debug;

/// Default loopback port Steam's CEF debugger listens on.
pub const DEFAULT_CEF_PORT: u16 = 8080;

/// Per-target operation timeout so one wedged page can't stall the watchdog.
const TARGET_TIMEOUT: Duration = Duration::from_secs(3);

/// DOM signature for an interstitial: a body-text pattern that identifies the
/// modal and an affirmative-button pattern to click. Both are JS regex sources,
/// matched case-insensitively. The body pattern is the discriminator (so we
/// never click the wrong dialog); the button pattern is just which control.
///
/// Verified signatures (seen live): [`InterstitialKind::CloudSync`],
/// [`InterstitialKind::ControllerRecommended`]. The rest are best-effort and
/// only ever consulted when explicitly enabled.
fn signature(kind: InterstitialKind) -> (&'static str, &'static str) {
    match kind {
        InterstitialKind::CloudSync => ("unable to sync", "play anyway"),
        InterstitialKind::ControllerRecommended => (
            "grab a controller|controller for best experience|we recommend[a-z ]*controller",
            "^ok$",
        ),
        InterstitialKind::SteamInputIntro => ("intro to steam input|using steam input", "^ok$"),
        InterstitialKind::ControllerRequired => {
            ("requires a controller|controller is required", "^ok$")
        }
        InterstitialKind::VrRequired => ("requires.{0,20}(vr|virtual reality|headset)", "^ok$"),
    }
}

/// JS evaluated in each page. Given a list of `{kind, body, button}` specs, it
/// finds the first whose body pattern matches the page text, clicks a button
/// matching its button pattern, and returns that spec's `kind`. Returns `""`
/// when nothing matched. Built by injecting the specs JSON in place of the
/// `SPECS_PLACEHOLDER` token to avoid `format!` brace-escaping.
const DISMISS_JS_TEMPLATE: &str = r#"
(function (specs) {
  try {
    var body = document.body ? (document.body.innerText || '') : '';
    var nodes = Array.prototype.slice.call(
      document.querySelectorAll('button,[role="button"],[tabindex]'));
    for (var i = 0; i < specs.length; i++) {
      var s = specs[i];
      if (!(new RegExp(s.body, 'i')).test(body)) continue;
      var krx = new RegExp(s.button, 'i');
      var btn = nodes.filter(function (e) {
        return krx.test((e.innerText || e.textContent || '').trim());
      })[0];
      if (btn) { btn.click(); return s.kind; }
    }
    return '';
  } catch (e) { return ''; }
})(SPECS_PLACEHOLDER)
"#;

/// What a dismissal sweep found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissOutcome {
    /// An interstitial of this kind was present and dismissed.
    Dismissed(InterstitialKind),
    /// CEF was reachable but no enabled interstitial is currently showing.
    NoModal,
}

#[derive(Debug, thiserror::Error)]
pub enum InterstitialError {
    #[error("CEF debugger not reachable: {0}")]
    Unreachable(String),
    #[error("CEF debugger protocol error: {0}")]
    Protocol(String),
}

#[derive(Debug, Deserialize)]
struct CefTarget {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default, rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
}

/// Sweep the CEF page targets for any of the `enabled` interstitials and, if one
/// is showing, click its affirmative button.
///
/// Returns `Dismissed(kind)` if it dismissed one, `NoModal` if CEF was reachable
/// but nothing matched, or an error if the endpoint couldn't be reached or spoke
/// unexpectedly. The caller treats any non-`Dismissed` result as "not resolved
/// yet" and keeps waiting until its own deadline.
pub async fn try_dismiss_interstitials(
    port: u16,
    enabled: &HashSet<InterstitialKind>,
) -> Result<DismissOutcome, InterstitialError> {
    if enabled.is_empty() {
        return Ok(DismissOutcome::NoModal);
    }

    // Build specs in catalog order (so verified, benign kinds match first).
    let specs: Vec<serde_json::Value> = InterstitialKind::ALL
        .into_iter()
        .filter(|k| enabled.contains(k))
        .map(|k| {
            let (body, button) = signature(k);
            serde_json::json!({ "kind": k.slug(), "body": body, "button": button })
        })
        .collect();
    let js = DISMISS_JS_TEMPLATE.replace(
        "SPECS_PLACEHOLDER",
        &serde_json::Value::Array(specs).to_string(),
    );

    let targets = match timeout(TARGET_TIMEOUT, fetch_targets(port)).await {
        Ok(Ok(targets)) => targets,
        Ok(Err(e)) => return Err(InterstitialError::Unreachable(e.to_string())),
        Err(_) => return Err(InterstitialError::Unreachable("GET /json timed out".into())),
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
        match timeout(TARGET_TIMEOUT, evaluate_on_target(&ws_url, &js)).await {
            Ok(Ok(value)) if !value.is_empty() => {
                if let Some(kind) = InterstitialKind::from_slug(&value) {
                    return Ok(DismissOutcome::Dismissed(kind));
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => debug!(error = %e, "CEF evaluate failed on a page target"),
            Err(_) => debug!("CEF evaluate timed out on a page target"),
        }
    }

    if saw_page {
        Ok(DismissOutcome::NoModal)
    } else {
        Err(InterstitialError::Protocol("no page targets".into()))
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
        if let Some(idx) = find_subslice(&buf, b"\r\n\r\n") {
            let body = &buf[idx + 4..];
            let mut de = serde_json::Deserializer::from_slice(body);
            if let Ok(targets) = Vec::<CefTarget>::deserialize(&mut de) {
                return Ok(targets);
            }
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incomplete or invalid /json response",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Open the page's WebSocket and run `js`, returning the string it produced.
async fn evaluate_on_target(ws_url: &str, js: &str) -> Result<String, InterstitialError> {
    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| InterstitialError::Protocol(e.to_string()))?;

    let enable = serde_json::json!({"id": 1, "method": "Runtime.enable"});
    ws.send(Message::Text(enable.to_string()))
        .await
        .map_err(|e| InterstitialError::Protocol(e.to_string()))?;

    let eval = serde_json::json!({
        "id": 2,
        "method": "Runtime.evaluate",
        "params": { "expression": js, "returnByValue": true },
    });
    ws.send(Message::Text(eval.to_string()))
        .await
        .map_err(|e| InterstitialError::Protocol(e.to_string()))?;

    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| InterstitialError::Protocol(e.to_string()))?;
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
    Err(InterstitialError::Protocol(
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
/// failure — Steam still launches, we just can't auto-dismiss interstitials.
pub fn ensure_cef_debug_enabled() {
    let Some(path) = cef_debug_flag_path() else {
        tracing::warn!("Could not resolve home directory for Steam CEF debug flag");
        return;
    };
    if path.exists() {
        return;
    }
    match std::fs::File::create(&path) {
        Ok(_) => debug!(path = %path.display(), "Enabled Steam CEF remote debugging"),
        Err(e) => tracing::warn!(error = %e, path = %path.display(),
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

    #[test]
    fn every_kind_has_a_signature() {
        // Guards against adding a kind to the catalog without a recognizer.
        for kind in InterstitialKind::ALL {
            let (body, button) = signature(kind);
            assert!(!body.is_empty(), "{kind:?} has empty body pattern");
            assert!(!button.is_empty(), "{kind:?} has empty button pattern");
        }
    }

    #[tokio::test]
    async fn empty_set_is_noop() {
        let out = try_dismiss_interstitials(1, &HashSet::new()).await;
        assert_eq!(out.unwrap(), DismissOutcome::NoModal);
    }

    #[tokio::test]
    async fn unreachable_port_is_an_error() {
        // Port 1 is privileged and nothing listens there; connect must fail.
        let enabled = HashSet::from([InterstitialKind::CloudSync]);
        let result = try_dismiss_interstitials(1, &enabled).await;
        assert!(matches!(result, Err(InterstitialError::Unreachable(_))));
    }

    /// Live check against a real Steam with CEF debugging enabled. Ignored by
    /// default; run with a known interstitial showing:
    ///   cargo test -p lunchbox-host-linux -- --ignored live_dismiss
    #[tokio::test]
    #[ignore]
    async fn live_dismiss() {
        let enabled = HashSet::from([
            InterstitialKind::CloudSync,
            InterstitialKind::ControllerRecommended,
        ]);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match try_dismiss_interstitials(DEFAULT_CEF_PORT, &enabled).await {
                Ok(DismissOutcome::Dismissed(kind)) => {
                    eprintln!("dismissed {kind:?}");
                    return;
                }
                other => eprintln!("not yet: {other:?}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no interstitial dismissed within 20s"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
