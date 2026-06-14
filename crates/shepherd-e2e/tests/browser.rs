//! End-to-end test for the supervised-browser activity wiring.
//!
//! CI cannot run real Chrome, but it *can* verify everything shepherdd does
//! around it: on launch the daemon must (1) write the Chromium managed-policy
//! JSON under the browser root, (2) append the `--user-data-dir` / `--kiosk` /
//! start-URL flags to the activity's argv, and (3) wipe the per-profile
//! user-data-dir after the activity exits when `wipe_on_exit` is set.
//!
//! A `process`-kind entry stands in for `flatpak run com.google.Chrome` (the
//! browser materialization path fires for both), with a fake "chrome" script
//! that records its argv. `SHEPHERD_BROWSER_ROOT` redirects all writes into a
//! tempdir so the test never touches a real `~/.var/app/...`.
//!
//! Run alongside the other e2e tests with
//! `cargo test -p shepherd-e2e -- --include-ignored --test-threads=1`.

use anyhow::{Context, Result};
use serde_json::json;
use shepherd_e2e::{TestHarness, json_body};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const BROWSER_CONFIG: &str = r#"
config_version = 1

[service]
default_max_run_seconds = 3600

[service.management_api]
enabled = true
port = {HTTP_PORT}
bind = "127.0.0.1"
{AUTH_TOKEN_LINE}

[[entries]]
id = "chrome-school"
label = "School"
[entries.kind]
type = "process"
command = "@SCRIPT@"
[entries.kind.env]
SHEPHERD_TEST_ARGV = "@ARGV@"
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 300
[entries.browser]
profile_id = "school"
mode = "kiosk"
start_url = "https://classroom.google.com"
url_allowlist = ["https://*.google.com/*"]
disable_dev_tools = true
disable_incognito = true
disable_extensions = true
wipe_on_exit = true
"#;

fn write_executable(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

async fn wait_for_file(path: &Path, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("file not present after {:?}: {}", timeout, path.display())
}

async fn wait_for_gone(path: &Path, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if !path.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("path still present after {:?}: {}", timeout, path.display())
}

/// Full-stack browser wiring: managed policy written, launch flags appended,
/// and the ephemeral profile wiped on exit — all driven through the HTTP API.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn browser_materializes_policy_flags_and_wipes_profile() -> Result<()> {
    // `work` holds the fake chrome + argv log; `root` is the redirected
    // browser root. Keep them separate so wiping the profile never deletes the
    // argv log.
    let work = tempfile::Builder::new()
        .prefix("shepherd-e2e-browser-work-")
        .tempdir()
        .context("create work dir")?;
    let root = tempfile::Builder::new()
        .prefix("shepherd-e2e-browser-root-")
        .tempdir()
        .context("create browser root")?;

    let script = work.path().join("fake-chrome.sh");
    let argv_log = work.path().join("argv.log");
    // Record argv (one element per line), then linger briefly so the session is
    // observably running before it exits and the wipe fires.
    write_executable(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$SHEPHERD_TEST_ARGV\"\nsleep 1\n",
    )?;

    // The per-profile user-data-dir, as the daemon will compute it. Pre-create
    // it to stand in for Chrome having written a profile there.
    let user_data_dir = root
        .path()
        .join(".var/app/com.google.Chrome/config/google-chrome/school");
    fs::create_dir_all(user_data_dir.join("Default")).context("precreate profile")?;
    fs::write(user_data_dir.join("Default/Cookies"), b"x").context("seed profile")?;

    let config = BROWSER_CONFIG
        .replace("@SCRIPT@", &script.display().to_string())
        .replace("@ARGV@", &argv_log.display().to_string());

    let h = TestHarness::builder()
        .config_toml(config)
        .shepherdd_env("SHEPHERD_BROWSER_ROOT", root.path().display().to_string())
        .start()
        .await?;
    let http = h.http();

    let resp = http
        .post_json("/api/v1/sessions", &json!({ "entry_id": "chrome-school" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    assert_eq!(json_body(&resp)?["result"], json!("approved"));

    // (1) The managed-policy JSON is written under the browser root.
    let policy = root
        .path()
        .join(".var/app/com.google.Chrome/config/chromium/policies/managed/chrome-school.json");
    wait_for_file(&policy, Duration::from_secs(5)).await?;
    let policy_body = fs::read_to_string(&policy)?;
    assert!(
        policy_body.contains("URLAllowlist"),
        "policy missing URLAllowlist:\n{policy_body}"
    );
    assert!(
        policy_body.contains("\"*\""),
        "policy missing catch-all blocklist:\n{policy_body}"
    );

    // (2) The launch flags reach the activity's argv.
    wait_for_file(&argv_log, Duration::from_secs(5)).await?;
    let argv = fs::read_to_string(&argv_log)?;
    assert!(
        argv.lines()
            .any(|l| l == format!("--user-data-dir={}", user_data_dir.display())),
        "missing --user-data-dir; argv:\n{argv}"
    );
    for needle in ["--kiosk", "https://classroom.google.com"] {
        assert!(
            argv.lines().any(|l| l == needle),
            "missing argv element {needle:?}; argv:\n{argv}"
        );
    }

    // (3) When the activity exits, the ephemeral profile is wiped.
    wait_for_gone(&user_data_dir, Duration::from_secs(10)).await?;

    h.shutdown().await?;
    drop(work);
    drop(root);
    Ok(())
}
