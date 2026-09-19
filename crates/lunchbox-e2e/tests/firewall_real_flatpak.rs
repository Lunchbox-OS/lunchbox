//! Manual firewall enforcement test for Flatpak entries.
//!
//! Same shape as `firewall_real_snap.rs` but exercises the flatpak path of
//! `apply_firewall_to_existing_scope`. The runtime-managed scope name
//! pattern is `app-flatpak-<app_id>-<n>.scope`; lunchboxd polls for it,
//! invokes the helper via pkexec, and the helper attaches the
//! cgroup_skb/egress BPF program to that cgroup.
//!
//! Run via `scripts/integration-tests/test-firewall-flatpak.sh`, which
//! provisions a tiny test flatpak via flatpak-builder and exports the env
//! the test needs.

// Fixture code: spawns probes and stand-ins by name, which the ban makes
// deliberate rather than accidental (issue #144).
#![allow(clippy::disallowed_methods)]

use anyhow::{Context, Result};
use serde_json::json;
use lunchbox_e2e::{TestHarness, json_body};
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const HELPER_PATH: &str = "/usr/libexec/lunchbox-firewall-helper";
const POLKIT_ACTION: &str = "org.shepherd.firewall.apply-process";

fn skip_reason(app_id: &str) -> Option<String> {
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
    if Command::new("flatpak").arg("--version").output().is_err() {
        return Some("flatpak CLI not on PATH".into());
    }
    let installed = Command::new("flatpak")
        .args(["--user", "info", app_id])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !installed {
        return Some(format!(
            "flatpak '{app_id}' is not installed for the current user. Run \
             scripts/integration-tests/test-firewall-flatpak.sh which builds + \
             installs it via flatpak-builder."
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
async fn flatpak_firewall_enforcement_with_real_helper() -> Result<()> {
    let app_id = std::env::var("SHEPHERD_FIREWALL_PROBE_FLATPAK")
        .unwrap_or_else(|_| "org.shepherd.firewall.Probe".into());
    let deny_target =
        std::env::var("SHEPHERD_FIREWALL_PROBE_DENY").unwrap_or_else(|_| "8.8.8.8:53".into());

    if let Some(reason) = skip_reason(&app_id) {
        eprintln!(
            "[SKIP] flatpak_firewall_enforcement_with_real_helper: {reason}.\n\
             Run scripts/integration-tests/test-firewall-flatpak.sh on a properly-configured host."
        );
        return Ok(());
    }
    if !deny_target_reachable(&deny_target) {
        eprintln!("[SKIP] deny target {deny_target} unreachable from this host.");
        return Ok(());
    }

    // Allow target: in-process loopback listener on an ephemeral port.
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

    // The user's real XDG_DATA_HOME ($HOME/.local/share by default).
    // flatpak looks here for `--user`-installed apps.
    let xdg_data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
        format!(
            "{}/.local/share",
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
        )
    });

    // Probe log path under /tmp -- the test flatpak's manifest declares
    // `--filesystem=/tmp` so the sandboxed app can write here.
    let probe_log_path: PathBuf = std::env::var("SHEPHERD_FIREWALL_PROBE_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            tempfile::Builder::new()
                .prefix("shepherd-fw-fp-")
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
id = "flatpak-firewall-probe"
label = "Flatpak Firewall Probe"
[entries.kind]
type = "flatpak"
app_id = "{app_id}"
[entries.kind.env]
# The harness overrides XDG_DATA_HOME to a tempdir so lunchboxd doesn't
# pollute the user's data; flatpak then looks for user-installed apps in
# $TEMPDIR/flatpak/app and fails ("app/<id>/x86_64/master not installed").
# Override back to the real user dir so `flatpak run` finds the test app.
XDG_DATA_HOME = "{xdg_data_home}"
SHEPHERD_FIREWALL_PROBE_LOG = "{log}"
SHEPHERD_FIREWALL_PROBE_ALLOW = "{allow}"
SHEPHERD_FIREWALL_PROBE_DENY = "{deny}"
SHEPHERD_FIREWALL_PROBE_HOLD_SECONDS = "120"
# 5s gives lunchboxd's wait_for_scope poll loop and the
# pkexec→helper→BPF-attach round trip time to land before the probe
# starts testing. flatpak-run startup itself usually consumes most of
# this already, but the explicit delay makes the test deterministic.
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
        app_id = app_id,
        log = probe_log_path.display(),
        allow = allow_target,
        deny = deny_target,
        xdg_data_home = xdg_data_home,
    );

    let h = TestHarness::builder().config_toml(&config).start().await?;
    let http = h.http();

    let resp = http
        .rpc("launch", json!({ "id": "flatpak-firewall-probe" }))
        .await?;
    assert_eq!(resp.status, 200, "launch body: {}", resp.body);
    assert!(json_body(&resp)?["Approved"].is_object());

    // flatpak run startup is similar to snap: dbus activation + sandbox
    // setup before the probe even starts. Up to 60s so the probe has
    // time after INITIAL_DELAY.
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
         the BPF cgroup attach didn't happen — check that lunchboxd logged 'Applied firewall \
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
