//! End-to-end tests for the per-entry firewall.
//!
//! CI cannot exercise actual BPF filter enforcement (that needs
//! `CAP_NET_ADMIN`, the *system* systemd manager, and a real polkit), so
//! these tests target the *wiring* that `shepherdd` does on top:
//!
//! 1. `firewall_unsupported_path_gates_activity` — when the helper isn't
//!    installed, `firewall_enforcement_status()` reports `Unsupported` and
//!    activities with `[entries.firewall]` configured must *not* launch
//!    (issue #143). The config promises the activity is filtered, so if that
//!    promise cannot be kept it does not run, rather than running unfiltered
//!    behind a log line nobody reads.
//!
//! 2. `firewall_supported_path_invokes_helper_with_expected_argv` — when
//!    the helper *is* available, the daemon spawns the activity through
//!    `pkexec <helper> apply-process …` with the right scope name, uid,
//!    gid, default policy, and allow rules. We can't run a real polkit /
//!    pkexec / privileged helper in a container, so we drop stub
//!    executables in front of `PATH` and point `SHEPHERD_FIREWALL_HELPER`
//!    at one of them.
//!
//! Run alongside the other e2e tests with
//! `cargo test -p shepherd-e2e -- --include-ignored --test-threads=1`.

use anyhow::{Context, Result};
use serde_json::json;
use shepherd_e2e::{TestHarness, json_body, proc_inspect};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const FIREWALL_CONFIG: &str = r#"
config_version = 1

[service]
default_max_run_seconds = 3600

[service.management_api]
enabled = true
port = {HTTP_PORT}
bind = "127.0.0.1"
{AUTH_TOKEN_LINE}

[[entries]]
id = "filtered-sleeper"
label = "Filtered Sleeper"
[entries.kind]
type = "process"
command = "/usr/bin/sleep"
args = ["600"]
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 300
[entries.firewall]
default = "deny"
allow = ["127.0.0.0/8", "::1/128"]
deny = []
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

/// Probe reports `Unsupported` when the helper isn't installed → the entry is
/// gated and does not launch (issue #143).
///
/// This tightens, rather than replaces, the guard the "Make firewall
/// enforcement failures explicit" change added. That change stopped the daemon
/// silently no-op'ing through `systemd-run --user --scope` and made it warn and
/// run anyway; the remaining hole was that the activity still ran *unfiltered*
/// while the configuration said it was filtered. Now it does not run at all,
/// and the administrator gets a `Critical` diagnostic naming the host problem
/// and the activity it costs.
///
/// Asserted on the child process as well as on the API, because "launch was
/// denied" and "launch was approved and then failed to spawn" are different
/// bugs that would both fail the API assertion alone.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn firewall_unsupported_path_gates_activity() -> Result<()> {
    let h = TestHarness::builder()
        .config_toml(FIREWALL_CONFIG)
        // Force "Unsupported": probe checks file existence first, and this
        // path is guaranteed to not exist.
        .shepherdd_env(
            "SHEPHERD_FIREWALL_HELPER",
            "/nonexistent/shepherd-firewall-helper",
        )
        .start()
        .await?;
    let http = h.http();

    // The gate is fed by the daemon's diagnostic sweep, which runs at startup
    // but not necessarily before the harness's first request lands.
    let mut entry = json_body(
        &http
            .rpc("get_entry", json!({ "id": "filtered-sleeper" }))
            .await?,
    )?;
    for _ in 0..40 {
        if entry["enabled"] == json!(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        entry = json_body(
            &http
                .rpc("get_entry", json!({ "id": "filtered-sleeper" }))
                .await?,
        )?;
    }

    assert_eq!(
        entry["enabled"],
        json!(false),
        "an entry whose configured firewall cannot be applied must not be available: {entry}"
    );
    let reasons = entry["reasons"].as_array().cloned().unwrap_or_default();
    assert!(
        reasons
            .iter()
            .any(|r| r["code"] == json!("protection_unavailable")),
        "expected a protection_unavailable reason, got: {reasons:?}"
    );

    // Denied, and nothing spawned. Checking only the API would not
    // distinguish a refused launch from an approved one that never ran.
    let resp = http
        .rpc("launch", json!({ "id": "filtered-sleeper" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    let body = json_body(&resp)?;
    assert!(
        body["Denied"].is_object(),
        "launch must be denied while the firewall cannot be enforced: {body}"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !proc_inspect::any_process_matching("sleep"),
        "no activity may spawn when its configured firewall cannot be applied"
    );

    // The administrator-facing half: the host cause and the activity it costs.
    let diagnostics = json_body(&http.rpc("list_diagnostics", json!({})).await?)?;
    let codes: Vec<_> = diagnostics["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|d| d["code"].clone())
        .collect();
    assert!(
        codes.contains(&json!("firewall_unenforceable")),
        "expected the host-wide diagnostic, got: {codes:?}"
    );
    assert!(
        codes.contains(&json!("firewall_not_applied")),
        "expected the per-activity diagnostic, got: {codes:?}"
    );

    h.shutdown().await?;
    Ok(())
}

/// When the helper is available, the daemon spawns the activity through
/// `pkexec <helper> apply-process …`. We stub all three binaries so the
/// chain runs in CI and the trailing `--`-delimited command (`/usr/bin/sleep
/// 600`) is execed.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn firewall_supported_path_invokes_helper_with_expected_argv() -> Result<()> {
    let stubs = tempfile::Builder::new()
        .prefix("shepherd-e2e-fw-stubs-")
        .tempdir()
        .context("create stub dir")?;
    let stubs_path = stubs.path().to_path_buf();
    let argv_log = stubs_path.join("helper-argv.log");

    // pkcheck → always grant. shepherdd runs this once during process::init().
    write_executable(&stubs_path.join("pkcheck"), "#!/bin/sh\nexit 0\n")?;

    // pkexec → drop --keep-cwd, exec the rest. Real pkexec scrubs env and
    // reruns as root; for the wiring test that doesn't matter.
    write_executable(
        &stubs_path.join("pkexec"),
        "#!/bin/sh\nif [ \"$1\" = \"--keep-cwd\" ]; then shift; fi\nexec \"$@\"\n",
    )?;

    // shepherd-firewall-helper → record argv (one element per line, with
    // BEGIN/END markers), then exec the trailing command after `--`.
    write_executable(
        &stubs_path.join("shepherd-firewall-helper"),
        &format!(
            "#!/bin/sh\n\
             {{\n\
                 printf '%s\\n' 'ARGV_BEGIN'\n\
                 for a in \"$@\"; do printf '%s\\n' \"$a\"; done\n\
                 printf '%s\\n' 'ARGV_END'\n\
             }} > '{log}'\n\
             # Skip the leading subcommand (apply-process).\n\
             if [ $# -gt 0 ]; then shift; fi\n\
             # Walk past helper flags up to the `--` separator.\n\
             while [ $# -gt 0 ] && [ \"$1\" != \"--\" ]; do shift; done\n\
             # Drop the `--` itself.\n\
             if [ $# -gt 0 ]; then shift; fi\n\
             exec \"$@\"\n",
            log = argv_log.display()
        ),
    )?;

    let helper_path = stubs_path.join("shepherd-firewall-helper");
    let augmented_path = format!(
        "{}:{}",
        stubs_path.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let h = TestHarness::builder()
        .config_toml(FIREWALL_CONFIG)
        .shepherdd_env(
            "SHEPHERD_FIREWALL_HELPER",
            helper_path.display().to_string(),
        )
        .shepherdd_env("PATH", augmented_path)
        .start()
        .await?;
    let http = h.http();

    let resp = http
        .rpc("launch", json!({ "id": "filtered-sleeper" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    assert!(json_body(&resp)?["Approved"].is_object());

    wait_for_file(&argv_log, Duration::from_secs(5)).await?;
    let recorded = fs::read_to_string(&argv_log)
        .with_context(|| format!("read helper log {}", argv_log.display()))?;

    // Each argv element is on its own line; check that the bits
    // `firewall_helper_argv_prefix` is responsible for are all present.
    for needle in &[
        "apply-process",
        "--scope-name",
        "--uid",
        "--gid",
        "--default",
        "deny",
        "--allow",
        "127.0.0.0/8",
        "::1/128",
        "--",
        "/usr/bin/sleep",
        "600",
    ] {
        assert!(
            recorded.lines().any(|l| l == *needle),
            "expected argv element {:?} in helper log; got:\n{}",
            needle,
            recorded
        );
    }

    let mut found = false;
    for _ in 0..30 {
        if proc_inspect::any_process_matching("sleep") {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "sleep child did not appear after helper stub exec");

    let resp = http.rpc("stop_current", json!({})).await?;
    assert_eq!(resp.status, 200);
    proc_inspect::wait_until_no_process("sleep", Duration::from_secs(5)).await?;

    h.shutdown().await?;
    drop(stubs);
    Ok(())
}
