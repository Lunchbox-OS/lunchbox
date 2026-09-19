//! Teardown of a firewalled Process-kind activity's systemd scope.
//!
//! A firewalled activity runs inside a transient scope in the **system**
//! manager (it needs `CAP_NET_ADMIN` to attach the cgroup BPF programs behind
//! `IPAddressDeny=`), so stopping it means going back through the privileged
//! helper. `lunchbox-firewall-helper stop-scope` existed for exactly this and
//! had **no callers at all** — the escalation was dead code, and the scope name
//! was not even recorded on the session, so it could not have been
//! reconstructed at stop time (issue #136).
//!
//! Emptying the scope's cgroup reaches processes our own signals may not, which
//! is the whole point: it is the last escalation before an activity is declared
//! escaped.
//!
//! Prerequisites — the same set as the other `firewall_real*` tests:
//!   * `/usr/libexec/lunchbox-firewall-helper` installed
//!   * polkit granting `com.lunchboxos.firewall.apply-process` without a prompt
//!   * a running system systemd manager
//!
//! Set up with:
//!   sudo ./scripts/integration-tests/setup-firewall-dev.sh
//!
//! Run with:
//!   cargo test -p lunchbox-host-linux --test firewall_teardown -- \
//!       --include-ignored --test-threads=1 --nocapture
//!
//! When a prerequisite is missing the test prints `[SKIP]` and passes, so the
//! same command works on CI.

// Spawns stand-ins by name, which is what a fixture is for; the ban is aimed at
// a daemon on a device choosing a helper through `$PATH` (issue #144).
#![allow(clippy::disallowed_methods)]

use lunchbox_api::EntryKind;
use lunchbox_host_api::{FirewallSpec, HostAdapter, SpawnOptions};
use lunchbox_host_linux::{LinuxHost, make_scope_name, pid_is_live, stop_firewall_scope};
use lunchbox_util::SessionId;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const HELPER_PATH: &str = "/usr/libexec/lunchbox-firewall-helper";
const POLKIT_ACTION: &str = "com.lunchboxos.firewall.apply-process";

/// `None` when this host can run the test, `Some(reason)` otherwise.
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

fn scope_is_active(scope: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", scope])
        .status()
        .is_ok_and(|s| s.success())
}

/// Poll until `cond` holds or `within` elapses.
fn wait_for(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cond()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn stop_scope_tears_down_a_firewalled_activity() {
    if let Some(reason) = skip_reason() {
        eprintln!(
            "[SKIP] stop_scope_tears_down_a_firewalled_activity: {reason}.\n\
             Run sudo ./scripts/integration-tests/setup-firewall-dev.sh.\n\
             This is the expected outcome on CI."
        );
        return;
    }

    let host = LinuxHost::new();
    let _rx = host.subscribe();

    let session_id = SessionId::new();
    // The name `spawn` derives internally, and therefore the one recorded on
    // the session for teardown to escalate to.
    let scope = make_scope_name(&session_id.to_string());

    let entry = EntryKind::Process {
        // A long-lived child so the scope is observably up before we stop it.
        command: "tail".into(),
        args: vec!["-f".into(), "/dev/null".into()],
        env: HashMap::new(),
        cwd: None,
    };
    let options = SpawnOptions {
        firewall: Some(FirewallSpec {
            // Allow everything: this test is about teardown, not enforcement.
            // `firewall_real.rs` covers whether the filter actually bites.
            default_deny: false,
            allow: vec![],
            deny: vec![],
        }),
        ..Default::default()
    };

    let handle = host
        .spawn(session_id.clone(), &entry, options)
        .await
        .expect("spawn firewalled activity");

    let pid = match handle.payload() {
        lunchbox_host_api::HostHandlePayload::Linux { pid, .. } => *pid,
        other => panic!("expected a Linux handle, got {other:?}"),
    };

    assert!(
        wait_for(Duration::from_secs(15), || scope_is_active(&scope)),
        "the activity should be running inside transient scope {scope}; \
         without it there is nothing for teardown to escalate to"
    );

    // The call that had no callers.
    let stopped = tokio::task::spawn_blocking({
        let scope = scope.clone();
        move || stop_firewall_scope(&scope)
    })
    .await
    .expect("join stop_firewall_scope");

    assert!(
        stopped,
        "stop_firewall_scope must report success for a scope that exists"
    );
    assert!(
        wait_for(Duration::from_secs(10), || !scope_is_active(&scope)),
        "scope {scope} should be gone after stop-scope"
    );
    assert!(
        wait_for(Duration::from_secs(10), || !pid_is_live(pid)),
        "stopping the scope must empty its cgroup, taking the activity with it"
    );
}

/// Stopping a scope that does not exist must fail rather than report success —
/// otherwise a teardown that escalated to nothing would look like it worked.
#[tokio::test]
#[ignore]
async fn stop_scope_reports_failure_for_a_scope_that_does_not_exist() {
    if let Some(reason) = skip_reason() {
        eprintln!("[SKIP] stop_scope_reports_failure_for_a_scope_that_does_not_exist: {reason}");
        return;
    }

    let missing = make_scope_name(&SessionId::new().to_string());
    assert!(!scope_is_active(&missing), "precondition: no such scope");

    let stopped = tokio::task::spawn_blocking(move || stop_firewall_scope(&missing))
        .await
        .unwrap();
    assert!(
        !stopped,
        "a scope that was never created cannot be reported as stopped"
    );
}
