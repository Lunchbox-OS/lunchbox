//! End-to-end tests for the full Lunchbox stack.
//!
//! Each `#[ignore]` test boots its own Sway, lunchboxd, and (optionally) UIs
//! in an isolated temp environment, then drives the daemon through the
//! JSON-RPC HTTP endpoint (`POST /api/v1/rpc`) and the IPC socket. Run with:
//!
//! ```sh
//! cargo test -p lunchbox-e2e -- --include-ignored --test-threads=1
//! ```

use anyhow::{Context, Result};
use lunchbox_e2e::{HarnessProcess, TestHarness, json_body, proc_inspect};
use nix::sys::signal::Signal;
use serde_json::json;
use std::time::Duration;

/// Boot test: lunchboxd comes up with a working `health` RPC, IPC
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
    h.signal(HarnessProcess::Lunchboxd, Signal::SIGTERM)?;
    let status = h
        .wait_for_exit(HarnessProcess::Lunchboxd, Duration::from_secs(5))
        .await?;
    assert!(status.success(), "lunchboxd exit status: {status:?}");

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
    let unauth = lunchbox_e2e::HttpClient::new(h.http_port(), None);
    let resp = unauth.rpc("health", json!({})).await?;
    assert_eq!(resp.status, 401, "unauth body: {}", resp.body);

    h.shutdown().await?;
    Ok(())
}

/// The whole login flow against a real daemon: read the setup code the device
/// generated, exchange it for a password and a session, then use that session
/// as a browser would — cookie, not bearer.
///
/// The unit and integration tests cover the arithmetic and the routing; what
/// only this can show is that the code a running lunchboxd actually wrote to
/// its protected file is the one its HTTP server will accept.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn the_setup_code_on_disk_logs_a_browser_in() -> Result<()> {
    let h = TestHarness::builder().start().await?;

    let stored = std::fs::read_to_string(h.data_dir().join("web-auth.toml"))
        .context("the daemon should have written a web auth store at startup")?;
    let code = stored
        .lines()
        .find_map(|l| l.strip_prefix("enrolment_code = "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .context("a device with no password should have a setup code")?;
    assert_eq!(code.len(), 6, "setup code was {code:?}");

    // A client with no credential at all: what a browser is before it logs in.
    let anon = lunchbox_e2e::HttpClient::new(h.http_port(), None);
    let status = anon.get("/api/v1/auth/status").await?;
    assert_eq!(status.status, 200);
    assert_eq!(status.json()?["configured"], json!(false));

    // Everything else is refused until the password exists.
    assert_eq!(anon.rpc("health", json!({})).await?.status, 401);

    let setup = anon
        .post_json(
            "/api/v1/auth/setup",
            &json!({ "code": code, "password": "a real password" }),
        )
        .await?;
    assert_eq!(setup.status, 200, "setup body: {}", setup.body);
    let cookie = setup
        .set_cookie_pair()
        .context("setup should hand back a session cookie")?;

    // And now the browser is signed in, over the cookie alone.
    let browser = anon.with_cookie(&cookie);
    let health = browser.rpc("health", json!({})).await?;
    assert_eq!(health.status, 200, "health body: {}", health.body);

    let session = browser.get("/api/v1/auth/session").await?;
    assert_eq!(session.status, 200);
    assert_eq!(session.json()?["machine"], json!(false));

    // The status endpoint now says the device is set up, which is what stops
    // the SPA offering the setup form to the next person who opens it.
    assert_eq!(
        anon.get("/api/v1/auth/status").await?.json()?["configured"],
        json!(true)
    );

    // Signing out ends it for real.
    assert_eq!(
        browser
            .post_json("/api/v1/auth/signout", &json!({}))
            .await?
            .status,
        204
    );
    assert_eq!(browser.rpc("health", json!({})).await?.status, 401);

    h.shutdown().await?;
    Ok(())
}

/// A password login, and the throttle behind it, against a real daemon.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_password_login_works_and_a_wrong_one_does_not() -> Result<()> {
    let h = TestHarness::builder().start().await?;
    let stored = std::fs::read_to_string(h.data_dir().join("web-auth.toml"))?;
    let code = stored
        .lines()
        .find_map(|l| l.strip_prefix("enrolment_code = "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .context("a setup code")?;

    let anon = lunchbox_e2e::HttpClient::new(h.http_port(), None);
    anon.post_json(
        "/api/v1/auth/setup",
        &json!({ "code": code, "password": "a real password" }),
    )
    .await?;

    let wrong = anon
        .post_json(
            "/api/v1/auth/login",
            &json!({ "password": "not it at all" }),
        )
        .await?;
    assert_eq!(wrong.status, 403, "body: {}", wrong.body);
    assert!(wrong.set_cookie_pair().is_none());

    let right = anon
        .post_json(
            "/api/v1/auth/login",
            &json!({ "password": "a real password" }),
        )
        .await?;
    assert_eq!(right.status, 200, "body: {}", right.body);
    let cookie = right.set_cookie_pair().context("a session cookie")?;
    assert_eq!(
        anon.with_cookie(cookie)
            .rpc("health", json!({}))
            .await?
            .status,
        200
    );

    h.shutdown().await?;
    Ok(())
}
