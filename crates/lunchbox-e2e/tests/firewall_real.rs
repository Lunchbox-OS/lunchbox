//! Manual end-to-end firewall *enforcement* test.
//!
//! This is the real thing: a `lunchboxd` boots, an activity is launched
//! through the privileged `lunchbox-firewall-helper`, and a probe inside
//! the activity's systemd scope confirms that the BPF address filter is
//! actually active (loopback connections succeed; an external target gets
//! dropped).
//!
//! It cannot run in CI — `CAP_NET_ADMIN`, the *system* systemd manager, a
//! running polkit, the helper installed at `/usr/libexec/...`, and the
//! invoking user a member of the `shepherd-firewall` group are all
//! prerequisites. When any of those is missing the test prints a `[SKIP]`
//! line and returns Ok, so the same `cargo test --include-ignored`
//! command works in both environments.
//!
//! Set up a host with:
//!   sudo ./scripts/integration-tests/setup-firewall-dev.sh
//!   # log out + back in for the new group membership to take effect
//!
//! Run via the orchestrator (which prints clearer diagnostics):
//!   ./scripts/integration-tests/test-firewall.sh
//!
//! Or directly:
//!   cargo test -p lunchbox-e2e --test firewall_real -- \
//!       --include-ignored --test-threads=1 --nocapture

// Fixture code: spawns probes and stand-ins by name, which the ban makes
// deliberate rather than accidental (issue #144).
#![allow(clippy::disallowed_methods)]

use anyhow::{Context, Result};
use serde_json::json;
use lunchbox_e2e::{TestHarness, json_body};
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const HELPER_PATH: &str = "/usr/libexec/lunchbox-firewall-helper";
const POLKIT_ACTION: &str = "org.shepherd.firewall.apply-process";
/// Public, well-known TCP endpoint used as the deny target. Must be
/// reachable from outside the firewall scope, otherwise the deny check
/// passes for the wrong reason.
const DENY_TARGET: &str = "8.8.8.8:53";

/// Returns `None` when this host can run the test, or `Some(reason)`
/// describing why it must be skipped.
fn skip_reason() -> Option<String> {
    if !Path::new(HELPER_PATH).exists() {
        return Some(format!("{HELPER_PATH} is not installed"));
    }
    let pid = std::process::id().to_string();
    match Command::new("pkcheck")
        .args(["--action-id", POLKIT_ACTION, "--process", &pid])
        .output()
    {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(format!(
            "polkit denies {POLKIT_ACTION} for this user: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Some(format!("could not exec pkcheck: {e}")),
    }
}

/// Pre-flight: confirm the deny target is reachable from outside the
/// firewall scope. Without this the in-scope failure could be "no
/// internet" rather than "firewall blocked it".
fn deny_target_reachable_from_host() -> bool {
    let Ok(addr) = DENY_TARGET.parse::<SocketAddr>() else {
        return false;
    };
    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn firewall_enforcement_with_real_helper() -> Result<()> {
    if let Some(reason) = skip_reason() {
        eprintln!(
            "[SKIP] firewall_enforcement_with_real_helper: {reason}.\n\
             Run sudo ./scripts/integration-tests/setup-firewall-dev.sh and\n\
             re-login. This is the expected outcome on CI."
        );
        return Ok(());
    }
    if !deny_target_reachable_from_host() {
        eprintln!(
            "[SKIP] deny target {DENY_TARGET} is not reachable from this host. \
             The deny check would pass for the wrong reason."
        );
        return Ok(());
    }

    // Listener on an ephemeral loopback port — the activity probes it
    // *through* the firewall to confirm 127.0.0.0/8 is allowed.
    let listener = TcpListener::bind("127.0.0.1:0").context("bind allow-target listener")?;
    let allow_target_port = listener.local_addr()?.port();
    listener
        .set_nonblocking(true)
        .context("listener nonblocking")?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let accept_task = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            drop(sock);
        }
    });
    let allow_target = format!("127.0.0.1:{allow_target_port}");

    // Probe log: in a per-test temp dir we own. The activity runs as the
    // same uid (the helper's `--uid=$PKEXEC_UID`) so it can write here.
    let temp = tempfile::Builder::new()
        .prefix("shepherd-e2e-fwreal-")
        .tempdir()?;
    let log_path = temp.path().join("probe.log");

    let probe_script = std::env::current_dir()?
        .ancestors()
        .find_map(|d| {
            let p = d.join("scripts/integration-tests/run-firewall-probe.sh");
            if p.exists() { Some(p) } else { None }
        })
        .context("could not find scripts/integration-tests/run-firewall-probe.sh")?;

    let config = format!(
        r#"
config_version = 1

[service]
default_max_run_seconds = 3600

[service.management_api]
enabled = true
port = {{HTTP_PORT}}
bind = "127.0.0.1"
{{AUTH_TOKEN_LINE}}

[[entries]]
id = "firewall-probe"
label = "Firewall Probe"
[entries.kind]
type = "process"
command = "{probe}"
[entries.kind.env]
SHEPHERD_FIREWALL_PROBE_LOG = "{log}"
SHEPHERD_FIREWALL_PROBE_ALLOW = "{allow}"
SHEPHERD_FIREWALL_PROBE_DENY = "{deny}"
SHEPHERD_FIREWALL_PROBE_HOLD_SECONDS = "120"
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 180
[entries.firewall]
default = "deny"
allow = ["127.0.0.0/8", "::1/128"]
deny = []
"#,
        probe = probe_script.display(),
        log = log_path.display(),
        allow = allow_target,
        deny = DENY_TARGET,
    );

    let h = TestHarness::builder().config_toml(&config).start().await?;
    let http = h.http();

    let resp = http
        .rpc("launch", json!({ "id": "firewall-probe" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    assert!(json_body(&resp)?["Approved"].is_object());

    // Wait up to 30s for the probe to finish and atomically publish its log.
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(30);
    while start.elapsed() < timeout {
        if log_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        log_path.exists(),
        "probe log not produced at {} within {:?}",
        log_path.display(),
        timeout
    );
    let contents =
        fs::read_to_string(&log_path).with_context(|| format!("read {}", log_path.display()))?;
    eprintln!("---- probe log ----\n{contents}-------------------");

    // The verdicts. Both must hold for the test to pass.
    assert!(
        contents.contains("allow=OPEN"),
        "expected loopback connection to succeed (firewall allowed 127.0.0.0/8). \
         Probe log:\n{contents}"
    );
    assert!(
        contents.contains("deny=BLOCKED"),
        "expected external connection to be dropped by the firewall. \
         If you see deny=OPEN here, the BPF filter is NOT being attached -- \
         check that lunchboxd reports `firewall_enforcement_status() == Supported` \
         at startup. Probe log:\n{contents}"
    );

    // Stop the activity early; the script's trailing sleep is a safety net.
    let resp = http.rpc("stop_current", json!({})).await?;
    assert!(
        resp.status == 204 || resp.status == 200,
        "stop body (status {}): {}",
        resp.status,
        resp.body
    );

    accept_task.abort();
    let _ = accept_task.await;
    h.shutdown().await?;
    Ok(())
}
