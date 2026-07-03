//! End-to-end test for the supervised-browser activity wiring.
//!
//! CI can't run real Chrome, but it *can* verify everything shepherdd does
//! around it. On launch of a `com.google.Chrome` flatpak entry with
//! `[entries.browser]`, the daemon must (1) write the managed-policy JSON under
//! the browser root, (2) rebuild the launch into the policy-injection form
//! (`flatpak run --command=bash --env=SHEPHERD_POLICY=… com.google.Chrome -c
//! <shim> bash <chrome flags>`), and (3) wipe the per-profile user-data-dir
//! after the activity exits when `wipe_on_exit` is set.
//!
//! A stub `flatpak` on `PATH` records the argv it was invoked with and exits,
//! so no real Chrome (or flatpak) is needed. `SHEPHERD_BROWSER_ROOT` redirects
//! all writes into a tempdir.
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
type = "flatpak"
app_id = "com.google.Chrome"
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
url_allowlist = ["https://google.com"]
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

/// Full-stack browser wiring: managed policy written, launch rebuilt into the
/// injection form, and the ephemeral profile wiped on exit — all through the
/// HTTP API, with a stub `flatpak` standing in for real Chrome.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn browser_materializes_policy_injection_and_wipes_profile() -> Result<()> {
    // `work` holds the stub flatpak + argv log; `root` is the redirected
    // browser root. Separate so wiping the profile never deletes the argv log.
    let work = tempfile::Builder::new()
        .prefix("shepherd-e2e-browser-work-")
        .tempdir()
        .context("create work dir")?;
    let root = tempfile::Builder::new()
        .prefix("shepherd-e2e-browser-root-")
        .tempdir()
        .context("create browser root")?;

    let argv_log = work.path().join("flatpak-argv.log");
    // Stub flatpak: record argv (one per line), then exit so the monitor fires
    // the wipe. `$SHEPHERD_TEST_ARGV` is set from [entries.kind.env].
    write_executable(
        &work.path().join("flatpak"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$SHEPHERD_TEST_ARGV\"\n",
    )?;
    let augmented_path = format!(
        "{}:{}",
        work.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // Pre-create the per-profile user-data-dir, standing in for Chrome's.
    let user_data_dir = root
        .path()
        .join(".var/app/com.google.Chrome/config/google-chrome/school");
    fs::create_dir_all(user_data_dir.join("Default")).context("precreate profile")?;
    fs::write(user_data_dir.join("Default/Cookies"), b"x").context("seed profile")?;

    let config = BROWSER_CONFIG.replace("@ARGV@", &argv_log.display().to_string());

    let h = TestHarness::builder()
        .config_toml(config)
        .shepherdd_env("SHEPHERD_BROWSER_ROOT", root.path().display().to_string())
        .shepherdd_env("PATH", augmented_path)
        .start()
        .await?;
    let http = h.http();

    let resp = http.rpc("launch", json!({ "id": "chrome-school" })).await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    assert!(json_body(&resp)?["Approved"].is_object());

    // (1) The managed-policy JSON is written under the per-user policy dir.
    let policy = root
        .path()
        .join(".var/app/com.google.Chrome/config/shepherd-policies/chrome-school.json");
    wait_for_file(&policy, Duration::from_secs(5)).await?;
    let policy_body = fs::read_to_string(&policy)?;
    assert!(
        policy_body.contains("URLAllowlist"),
        "policy missing URLAllowlist:\n{policy_body}"
    );

    // (2) The launch is the injection form. The stub records the flatpak argv.
    wait_for_file(&argv_log, Duration::from_secs(5)).await?;
    let argv = fs::read_to_string(&argv_log)?;
    let lines: Vec<&str> = argv.lines().collect();
    for needle in [
        "run",
        "--command=bash",
        "com.google.Chrome",
        "-c",
        "bash",
        // Kiosk mode launches a chromeless `--app` window (sway denies real
        // fullscreen, so `--kiosk` is intentionally not used). See
        // shepherd-host-linux `chrome_flags`.
        "--app=https://classroom.google.com",
    ] {
        assert!(
            lines.contains(&needle),
            "missing argv element {needle:?}; argv:\n{argv}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|l| *l == format!("--env=SHEPHERD_POLICY={}", policy.display())),
        "missing --env=SHEPHERD_POLICY; argv:\n{argv}"
    );
    assert!(
        lines
            .iter()
            .any(|l| *l == format!("--user-data-dir={}", user_data_dir.display())),
        "missing --user-data-dir; argv:\n{argv}"
    );
    // The shim seeds the *sandbox's* /etc and execs the flatpak's own launcher.
    assert!(
        argv.contains("/etc/opt/chrome/policies/managed") && argv.contains("exec /app/bin/chrome"),
        "argv shim not the policy-injection form:\n{argv}"
    );

    // (3) When the activity exits, the ephemeral profile is wiped.
    wait_for_gone(&user_data_dir, Duration::from_secs(10)).await?;

    h.shutdown().await?;
    drop(work);
    drop(root);
    Ok(())
}
