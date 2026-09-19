//! #144's acceptance test: a peer lunchboxd did not start cannot call a
//! mutating method over the management socket, and the refusal is audited.
//!
//! The unit tests in `lunchbox-ipc` cover `classify` against a socketpair; this
//! covers the daemon — a real lunchboxd, its real socket, and a peer in a
//! cgroup of its own, which is what an activity is.
//!
//! It runs with the check *armed* (`restrict_ipc_peers`), unlike every other
//! e2e test. Note that the harness's own cgroup is a delegated user scope, so
//! the check is not a security boundary here — a peer could move itself in.
//! That does not weaken this test: the probe does not move itself in, so what
//! is under test is whether the comparison is made and acted on.

// Spawns `systemd-run` and the probe by name: fixture code placing a peer in a
// cgroup of its own, not a daemon choosing a helper (issue #144).
#![allow(clippy::disallowed_methods)]

use anyhow::Result;
use lunchbox_e2e::TestHarness;
use std::process::Command;

/// A peer in a cgroup of its own, standing in for an activity.
fn probe_in_own_cgroup(socket: &str, method: &str) -> Option<String> {
    let out = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--collect",
            "--quiet",
            // Unique per call: a scope name is a systemd unit name, and
            // reusing one while the previous scope is still winding down makes
            // `systemd-run` fail silently — which reads as an empty probe
            // result rather than as a refusal.
            &format!(
                "--unit=lunchbox-peer-probe-{}-{}.scope",
                std::process::id(),
                method.replace('_', "-")
            ),
            "--",
            env!("CARGO_BIN_EXE_peer-probe"),
            socket,
            method,
        ])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The same probe, left in the harness's cgroup — lunchboxd's own.
fn probe_in_our_cgroup(socket: &str, method: &str) -> Result<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_peer-probe"))
        .args([socket, method])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[tokio::test]
#[ignore = "needs a user manager to put a peer in a cgroup of its own"]
async fn a_peer_outside_lunchboxs_cgroup_cannot_call_a_mutating_method() -> Result<()> {
    if !lunchbox_ipc::kernel_supports_peer_cgroup() {
        eprintln!("[SKIP] this kernel cannot report a peer's cgroup");
        return Ok(());
    }

    let h = TestHarness::builder()
        .restrict_ipc_peers(true)
        .start()
        .await?;
    let socket = h.socket_path().display().to_string();

    // Control: Lunchbox's own clients share its cgroup and must still work,
    // or a passing refusal below would prove nothing.
    let ours = probe_in_our_cgroup(&socket, "health")?;
    assert!(
        ours.starts_with("ACCEPTED"),
        "a peer in Lunchbox's own cgroup must be accepted, got: {ours}"
    );

    let Some(foreign) = probe_in_own_cgroup(&socket, "adjust_tokens") else {
        eprintln!("[SKIP] systemd-run --user unavailable");
        return Ok(());
    };
    if foreign.is_empty() {
        eprintln!("[SKIP] the probe never ran in its own scope");
        return Ok(());
    }
    assert!(
        foreign.starts_with("REFUSED"),
        "a peer outside Lunchbox's cgroup must not reach adjust_tokens, got: {foreign}"
    );

    // ...and the same for the other method #144 names. Empty means the probe
    // never ran, which is a skip rather than a pass — an absent refusal must
    // never read as a refusal.
    match probe_in_own_cgroup(&socket, "extend_current") {
        Some(extend) if !extend.is_empty() => assert!(
            extend.starts_with("REFUSED"),
            "a peer outside Lunchbox's cgroup must not reach extend_current, got: {extend}"
        ),
        _ => eprintln!("[SKIP] extend_current probe never ran in its own scope"),
    }

    // The refusal has to survive as a record, not just a log line: #144's
    // acceptance asks for it to be audited. Read back through the store's own
    // API rather than by poking at the table, so this keeps testing the thing
    // an operator would actually read.
    use lunchbox_store::{SqliteStore, Store};
    let store = SqliteStore::open(h.data_dir().join("lunchboxd.db"))?;
    let audits = store.get_recent_audits(50)?;
    let recorded = audits
        .iter()
        .any(|e| format!("{:?}", e.event).contains("ClientRejected"));
    assert!(
        recorded,
        "the refusal must reach the audit log; the last events were: {:?}",
        audits
            .iter()
            .rev()
            .take(5)
            .map(|e| format!("{:?}", e.event))
            .collect::<Vec<_>>()
    );
    Ok(())
}
