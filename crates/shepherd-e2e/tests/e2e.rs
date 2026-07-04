//! End-to-end tests for the full shepherd stack.
//!
//! Each `#[ignore]` test boots its own Sway, shepherdd, and (optionally) UIs
//! in an isolated temp environment, then drives the daemon through the
//! JSON-RPC HTTP endpoint (`POST /api/v1/rpc`) and the IPC socket. Run with:
//!
//! ```sh
//! cargo test -p shepherd-e2e -- --include-ignored --test-threads=1
//! ```

use anyhow::{Context, Result};
use nix::sys::signal::Signal;
use serde_json::json;
use shepherd_e2e::{HarnessProcess, TestHarness, json_body, proc_inspect};
use std::time::Duration;

/// Boot test: shepherdd comes up with a working `health` RPC, IPC
/// accepts a ping, and SIGTERM produces a clean shutdown.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn boot_health_and_clean_shutdown() -> Result<()> {
    let mut h = TestHarness::builder().start().await?;

    let resp = h.http().rpc("health", json!({})).await?;
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

/// Launch and stop an activity through the RPC endpoint. Verifies the
/// child process appears under /proc and is reaped after the stop RPC.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn http_launch_and_stop_session() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let http = h.http();

    // Launch the long-lived sleeper.
    let resp = http.rpc("launch", json!({ "id": "sleeper" })).await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    let body = json_body(&resp)?;
    let approved = body["Approved"].as_object().context("expected Approved")?;
    let session_id = approved["session_id"]
        .as_str()
        .context("missing session_id")?
        .to_owned();
    assert!(!session_id.is_empty());

    // current_session should reflect the running session.
    let resp = http.rpc("current_session", json!({})).await?;
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

    // Stop.
    let resp = http.rpc("stop_current", json!({})).await?;
    assert_eq!(resp.status, 200, "stop body: {}", resp.body);

    // sleep child should disappear.
    proc_inspect::wait_until_no_process("sleep", Duration::from_secs(10)).await?;

    // current_session should be null.
    let resp = http.rpc("current_session", json!({})).await?;
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

    let resp = http.rpc("launch", json!({ "id": "ephemeral" })).await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);

    let event = sse
        .wait_for(Duration::from_secs(15), |ev| {
            ev["payload"]["type"] == "session_ended" && ev["payload"]["reason"]["type"] == "expired"
        })
        .await?;
    assert_eq!(event["payload"]["entry_id"], json!("ephemeral"));

    proc_inspect::wait_until_no_process("sleep", Duration::from_secs(5)).await?;

    h.shutdown().await?;
    Ok(())
}

/// Rewrite the on-disk config to remove the `ephemeral` entry, then verify
/// the daemon picks up the change (either via the file watcher or via an
/// explicit `reload_config` RPC).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn config_reload_picks_up_changes() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let http = h.http();

    // Baseline: 3 entries.
    let resp = http.rpc("list_entries", json!({})).await?;
    assert_eq!(resp.status, 200);
    let body = json_body(&resp)?;
    let entries = body.as_array().context("entries body not array")?;
    assert_eq!(entries.len(), 3, "expected 3 entries initially");

    // Write a new config with only the `sleeper` entry and force a reload.
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

    let resp = http.rpc("reload_config", json!({})).await?;
    assert_eq!(resp.status, 200, "reload body: {}", resp.body);

    // After reload there should be only 1 entry.
    let resp = http.rpc("list_entries", json!({})).await?;
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
    let resp = h.http().rpc("health", json!({})).await?;
    assert_eq!(resp.status, 200, "auth body: {}", resp.body);

    // Unauthenticated client is 401.
    let unauth = shepherd_e2e::HttpClient::new(h.http_port(), None);
    let resp = unauth.rpc("health", json!({})).await?;
    assert_eq!(resp.status, 401, "unauth body: {}", resp.body);

    h.shutdown().await?;
    Ok(())
}
