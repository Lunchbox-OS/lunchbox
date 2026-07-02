//! Manual firewall enforcement test for Snap entries.
//!
//! Verifies the BPF cgroup_skb program attached by
//! `shepherd-firewall-helper apply-cgroup` actually filters traffic out of
//! a Snap-runtime scope. Cannot run in CI (needs the helper installed,
//! polkit grants the action, snapd installed, and a pre-built test snap).
//!
//! Run via `scripts/integration-tests/test-firewall-snap.sh`, which builds
//! the test snap from this repo, installs it via `snap try --classic`, and
//! invokes this test with the right env vars set.

use anyhow::{Context, Result};
use serde_json::json;
use shepherd_e2e::{TestHarness, json_body};
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const HELPER_PATH: &str = "/usr/libexec/shepherd-firewall-helper";
const POLKIT_ACTION: &str = "org.shepherd.firewall.apply-process";

/// Returns `None` when this host can run the test, or `Some(reason)` if
/// not. Mirrors the predicate in firewall_real.rs but adds snapd and the
/// test snap's presence.
fn skip_reason(snap_name: &str) -> Option<String> {
    if !Path::new(HELPER_PATH).exists() {
        return Some(format!("{HELPER_PATH} is not installed"));
    }
    let pid = std::process::id().to_string();
    match Command::new("pkcheck")
        .args(["--action-id", POLKIT_ACTION, "--process", &pid])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            return Some(format!(
                "polkit denies {POLKIT_ACTION}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Err(e) => return Some(format!("could not exec pkcheck: {e}")),
    }
    if Command::new("snap").arg("version").output().is_err() {
        return Some("snap CLI not on PATH".into());
    }
    let snap_present = Command::new("snap")
        .args(["info", snap_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !snap_present {
        return Some(format!(
            "snap '{snap_name}' is not installed. Run scripts/integration-tests/test-firewall-snap.sh \
             which provisions it via `snap try --classic`."
        ));
    }
    None
}

fn deny_target_reachable(target: &str) -> bool {
    let Ok(addr) = target.parse::<SocketAddr>() else {
        return false;
    };
    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn snap_firewall_enforcement_with_real_helper() -> Result<()> {
    let snap_name = std::env::var("SHEPHERD_FIREWALL_PROBE_SNAP")
        .unwrap_or_else(|_| "shepherd-firewall-probe".into());
    let deny_target =
        std::env::var("SHEPHERD_FIREWALL_PROBE_DENY").unwrap_or_else(|_| "8.8.8.8:53".into());

    if let Some(reason) = skip_reason(&snap_name) {
        eprintln!(
            "[SKIP] snap_firewall_enforcement_with_real_helper: {reason}.\n\
             Run scripts/integration-tests/test-firewall-snap.sh on a properly-configured host."
        );
        return Ok(());
    }
    if !deny_target_reachable(&deny_target) {
        eprintln!("[SKIP] deny target {deny_target} unreachable from this host.");
        return Ok(());
    }

    // Allow target: in-process loopback listener on an ephemeral port. The
    // probe inside the snap will TCP-connect to it through the BPF filter.
    let listener = TcpListener::bind("127.0.0.1:0").context("bind allow-target listener")?;
    let allow_port = listener.local_addr()?.port();
    listener
        .set_nonblocking(true)
        .context("listener nonblocking")?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let accept_task = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            drop(sock);
        }
    });
    let allow_target = format!("127.0.0.1:{allow_port}");

    // Probe log path. The orchestrator script creates a world-writable
    // tempdir and exports SHEPHERD_FIREWALL_PROBE_LOG; that lets the
    // classic-confined snap write back to a host path the test can read.
    let probe_log_path: PathBuf = std::env::var("SHEPHERD_FIREWALL_PROBE_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            tempfile::Builder::new()
                .prefix("shepherd-fw-snap-")
                .tempdir_in("/tmp")
                .unwrap()
                .keep()
                .join("probe.log")
        });
    if probe_log_path.exists() {
        let _ = fs::remove_file(&probe_log_path);
    }
    fs::create_dir_all(probe_log_path.parent().unwrap()).ok();

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
id = "snap-firewall-probe"
label = "Snap Firewall Probe"
[entries.kind]
type = "snap"
snap_name = "{snap_name}"
[entries.kind.env]
SHEPHERD_FIREWALL_PROBE_LOG = "{log}"
SHEPHERD_FIREWALL_PROBE_ALLOW = "{allow}"
SHEPHERD_FIREWALL_PROBE_DENY = "{deny}"
SHEPHERD_FIREWALL_PROBE_HOLD_SECONDS = "120"
# 5s gives shepherdd's wait_for_scope poll loop and the
# pkexec→helper→BPF-attach round trip time to land before the probe
# starts testing. snap-run startup itself usually consumes most of this
# already, but the explicit delay makes the test deterministic.
SHEPHERD_FIREWALL_PROBE_INITIAL_DELAY = "5"
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 180
[entries.firewall]
default = "deny"
allow = ["127.0.0.0/8", "::1/128"]
deny = []
"#,
        snap_name = snap_name,
        log = probe_log_path.display(),
        allow = allow_target,
        deny = deny_target,
    );

    let h = TestHarness::builder().config_toml(&config).start().await?;
    let http = h.http();

    let resp = http
        .rpc("launch", json!({ "id": "snap-firewall-probe" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    assert!(json_body(&resp)?["Approved"].is_object());

    // Snap startup is slower than a bare process: a fresh `snap run` has to
    // initialize the runtime + apparmor profile + create the systemd scope
    // BEFORE shepherdd's apply-cgroup race can attach. The probe itself
    // runs inside the scope and finishes in <3s once the helper has
    // attached.
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(60);
    while start.elapsed() < timeout {
        if probe_log_path.exists()
            && fs::metadata(&probe_log_path)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let contents = fs::read_to_string(&probe_log_path)
        .with_context(|| format!("read probe log {}", probe_log_path.display()))?;
    eprintln!("---- probe log ----\n{contents}-------------------");

    assert!(
        contents.contains("allow=OPEN"),
        "expected loopback connection to succeed (firewall allowed 127.0.0.0/8). \
         Probe log:\n{contents}"
    );
    assert!(
        contents.contains("deny=BLOCKED"),
        "expected external connection to be dropped by the firewall. If you see deny=OPEN, \
         the BPF cgroup attach didn't happen — check that shepherdd logged 'Applied firewall \
         (BPF) to scope' and that the helper's apply-cgroup didn't fail. Probe log:\n{contents}"
    );

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
