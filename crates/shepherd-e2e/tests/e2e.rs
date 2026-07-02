//! End-to-end tests for the full shepherd stack.
//!
//! Each `#[ignore]` test boots its own Sway, shepherdd, and (optionally) UIs
//! in an isolated temp environment, then drives the daemon through its HTTP
//! management API and IPC socket. Run with:
//!
//! ```sh
//! cargo test -p shepherd-e2e -- --include-ignored --test-threads=1
//! ```

use anyhow::{Context, Result};
use nix::sys::signal::Signal;
use serde_json::json;
use shepherd_e2e::{HarnessProcess, TestHarness, json_body, proc_inspect};
use std::time::Duration;

/// Boot test: shepherdd comes up with a working /api/v1/health endpoint, IPC
/// accepts a Ping, and SIGTERM produces a clean shutdown.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn boot_health_and_clean_shutdown() -> Result<()> {
    let mut h = TestHarness::builder().start().await?;

    // /api/v1/health
    let resp = h.http().get("/api/v1/health").await?;
    assert_eq!(resp.status, 200, "health body: {}", resp.body);
    let body = json_body(&resp)?;
    assert_eq!(body["live"], json!(true));
    assert_eq!(body["ready"], json!(true));

    // IPC ping
    let mut ipc = h.connect_ipc().await?;
    ipc.ping().await.context("IPC ping")?;

    // Clean SIGTERM shutdown
    h.signal(HarnessProcess::Shepherdd, Signal::SIGTERM)?;
    let status = h
        .wait_for_exit(HarnessProcess::Shepherdd, Duration::from_secs(5))
        .await?;
    assert!(status.success(), "shepherdd exit status: {status:?}");

    h.shutdown().await?;
    Ok(())
}

/// Launch and stop an activity through the HTTP management API. Verifies the
/// child process appears under /proc and is reaped after DELETE.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn http_launch_and_stop_session() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let http = h.http();

    // Launch the long-lived sleeper.
    let resp = http
        .post_json("/api/v1/sessions", &json!({ "entry_id": "sleeper" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    let body = json_body(&resp)?;
    assert_eq!(body["result"], json!("approved"));
    let session_id = body["session_id"]
        .as_str()
        .context("missing session_id")?
        .to_owned();
    assert!(!session_id.is_empty());

    // /sessions/current should reflect the running session.
    let resp = http.get("/api/v1/sessions/current").await?;
    assert_eq!(resp.status, 200);
    let body = json_body(&resp)?;
    assert_eq!(body["entry_id"], json!("sleeper"));

    // The sleep child should appear under /proc within a moment.
    let mut found = false;
    for _ in 0..30 {
        if proc_inspect::any_process_matching("sleep") {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        found,
        "no `sleep` process appeared under /proc after launch"
    );

    // Stop via DELETE.
    let resp = http.delete("/api/v1/sessions/current").await?;
    assert_eq!(
        resp.status, 204,
        "stop body (status {}): {}",
        resp.status, resp.body
    );

    // sleep child should disappear.
    proc_inspect::wait_until_no_process("sleep", Duration::from_secs(10)).await?;

    // /sessions/current should return null.
    let resp = http.get("/api/v1/sessions/current").await?;
    assert_eq!(resp.status, 200);
    assert_eq!(
        resp.body.trim(),
        "null",
        "expected null body, got: {}",
        resp.body
    );

    h.shutdown().await?;
    Ok(())
}

/// Launch the `ephemeral` entry (max_run_seconds=2) and observe the
/// `SessionEnded` event with reason `expired` on the SSE stream.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn session_expires_via_timeout() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let http = h.http();

    let mut sse = http.sse("/api/v1/events").await?;

    let resp = http
        .post_json("/api/v1/sessions", &json!({ "entry_id": "ephemeral" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);

    // Wait up to 15s for SessionEnded with reason.expired.
    let event = sse
        .wait_for(Duration::from_secs(15), |ev| {
            ev["payload"]["type"] == "session_ended" && ev["payload"]["reason"]["type"] == "expired"
        })
        .await?;
    assert_eq!(event["payload"]["entry_id"], json!("ephemeral"));

    // Confirm the sleep child is gone.
    proc_inspect::wait_until_no_process("sleep", Duration::from_secs(5)).await?;

    h.shutdown().await?;
    Ok(())
}

/// Launch a session, extend it via `POST /sessions/current/extend`, then stop it.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn extend_session_via_http() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let http = h.http();

    let resp = http
        .post_json("/api/v1/sessions", &json!({ "entry_id": "sleeper" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    let original_deadline = json_body(&resp)?["deadline"].as_str().map(str::to_owned);
    assert!(
        original_deadline.is_some(),
        "sleeper session should have a finite deadline"
    );

    let resp = http
        .post_json(
            "/api/v1/sessions/current/extend",
            &json!({ "seconds": 600 }),
        )
        .await?;
    assert_eq!(resp.status, 200, "extend body: {}", resp.body);
    let body = json_body(&resp)?;
    let new_deadline = body["new_deadline"]
        .as_str()
        .context("extend response missing new_deadline")?
        .to_owned();
    assert_ne!(
        Some(new_deadline.clone()),
        original_deadline,
        "deadline should advance after extend"
    );

    let resp = http.delete("/api/v1/sessions/current").await?;
    assert_eq!(resp.status, 204);

    h.shutdown().await?;
    Ok(())
}

/// Set a daily override that disables the `always-on` entry, then verify the
/// entry surfaces as unavailable in `GET /api/v1/entries/{id}`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn daily_override_disables_entry() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let http = h.http();

    // Baseline: entry should be enabled.
    let resp = http.get("/api/v1/entries/always-on").await?;
    assert_eq!(resp.status, 200);
    let body = json_body(&resp)?;
    assert_eq!(body["entry_id"], json!("always-on"));
    assert_eq!(
        body["enabled"],
        json!(true),
        "expected always-on to be enabled before override"
    );

    // Disable via override.
    let resp = http
        .put_json(
            "/api/v1/overrides/always-on",
            &json!({ "availability": false }),
        )
        .await?;
    assert_eq!(resp.status, 200, "upsert override body: {}", resp.body);

    // Entry should now report disabled.
    let resp = http.get("/api/v1/entries/always-on").await?;
    assert_eq!(resp.status, 200);
    let body = json_body(&resp)?;
    assert_eq!(
        body["enabled"],
        json!(false),
        "expected always-on to be disabled after override; body: {}",
        resp.body
    );

    // Clean up: delete override.
    let resp = http.delete("/api/v1/overrides/always-on").await?;
    assert_eq!(resp.status, 204);

    h.shutdown().await?;
    Ok(())
}

/// Rewrite the on-disk config to remove the `ephemeral` entry, then verify
/// the daemon picks up the change (either via the file watcher or via an
/// explicit `POST /config/reload`).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn config_reload_picks_up_changes() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let http = h.http();

    // Baseline: 3 entries.
    let resp = http.get("/api/v1/entries").await?;
    assert_eq!(resp.status, 200);
    let body = json_body(&resp)?;
    let entries = body.as_array().context("entries body not array")?;
    assert_eq!(entries.len(), 3, "expected 3 entries initially");

    // Write a new config with only the `sleeper` entry and force a reload via
    // the HTTP endpoint (avoids relying on the inotify watcher in tests).
    let trimmed = r#"
config_version = 1

[service]
default_max_run_seconds = 3600

[service.management_api]
enabled = true
port = {HTTP_PORT}
bind = "127.0.0.1"
{AUTH_TOKEN_LINE}

[[entries]]
id = "sleeper"
label = "Sleeper"
[entries.kind]
type = "process"
command = "/usr/bin/sleep"
args = ["600"]
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 300
"#;
    h.rewrite_config(trimmed)?;

    let resp = http.post_json("/api/v1/config/reload", &json!({})).await?;
    assert_eq!(resp.status, 200, "reload body: {}", resp.body);

    // After reload there should be only 1 entry.
    let resp = http.get("/api/v1/entries").await?;
    assert_eq!(resp.status, 200);
    let body = json_body(&resp)?;
    let entries = body.as_array().context("entries body not array")?;
    assert_eq!(
        entries.len(),
        1,
        "expected 1 entry after reload, got {}: {}",
        entries.len(),
        resp.body
    );
    assert_eq!(entries[0]["entry_id"], json!("sleeper"));

    h.shutdown().await?;
    Ok(())
}

/// Verify Bearer auth is enforced when configured.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn auth_token_is_required_when_configured() -> Result<()> {
    let h = TestHarness::builder()
        .auth_token("e2e-secret")
        .start()
        .await?;

    // Authenticated client succeeds.
    let resp = h.http().get("/api/v1/health").await?;
    assert_eq!(resp.status, 200, "auth body: {}", resp.body);

    // Unauthenticated client is 401.
    let unauth = shepherd_e2e::HttpClient::new(h.http_port(), None);
    let resp = unauth.get("/api/v1/health").await?;
    assert_eq!(resp.status, 401, "unauth body: {}", resp.body);

    h.shutdown().await?;
    Ok(())
}
