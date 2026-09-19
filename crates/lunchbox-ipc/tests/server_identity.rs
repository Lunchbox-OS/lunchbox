//! A client must not talk to something that merely took the socket's name
//! (issue #144, finding 2 of `docs/ai/history/2026-08-29 004`).
//!
//! The socket lives in a directory owned by the uid every activity runs as, so
//! an activity can `unlink()` it and bind its own listener at the same path.
//! Measured: a root-owned directory stops lunchboxd binding at all (`EACCES`),
//! and the sticky bit restricts deletion to the file's *owner* — which an
//! activity is, since it shares the daemon's uid. No file mode fixes it, so the
//! client identifies who answered instead.

// The impostor is deliberately spawned by name: it is a fixture standing in for
// an activity, not a helper a daemon chose (issue #144).
#![allow(clippy::disallowed_methods)]

use lunchbox_ipc::{IpcClient, IpcServer};

/// The positive path, and the one that would break every device if the check
/// were wrong: shepherd's own clients live in the daemon's cgroup, so a genuine
/// daemon must always verify.
#[tokio::test]
async fn a_client_accepts_the_daemon_in_its_own_cgroup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("lunchboxd.sock");

    let mut server = IpcServer::new(&path);
    server.start().await.expect("bind");
    let server = std::sync::Arc::new(server);
    let running = server.clone();
    tokio::spawn(async move { running.run().await });

    // Same process, so the same cgroup — exactly the relationship the launcher,
    // the HUD and sway's one-shots have with lunchboxd.
    IpcClient::connect(&path)
        .await
        .expect("a client must accept the daemon in its own cgroup");
}

/// The negative path. Needs a user manager to put the impostor in a cgroup of
/// its own, so it skips rather than fails where there is none (the e2e harness
/// runs with a temp `XDG_RUNTIME_DIR` and no bus).
#[tokio::test]
async fn a_client_refuses_an_impostor_in_another_cgroup() {
    // Below the kernel floor the client cannot identify anything, so it warns
    // and proceeds and there is no refusal to assert on. CI runs in a container
    // on the runner's kernel, which is older than the image suggests.
    if lunchbox_ipc::skip_without_peer_cgroup("a_client_refuses_an_impostor_in_another_cgroup") {
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("lunchboxd.sock");

    // A listener in a scope of its own, standing in for an activity that
    // unlinked the real socket and bound this one.
    let script = format!(
        "import socket,time\ns=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)\ns.bind({:?})\ns.listen(5)\ntime.sleep(30)",
        path.to_string_lossy()
    );
    let impostor = std::process::Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--collect",
            "--quiet",
            &format!("--unit=shepherd-impostor-test-{}.scope", std::process::id()),
            "--",
            "python3",
            "-c",
            &script,
        ])
        .spawn();
    let Ok(mut impostor) = impostor else {
        eprintln!("[SKIP] a_client_refuses_an_impostor_in_another_cgroup: no systemd-run");
        return;
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !path.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if !path.exists() {
        let _ = impostor.kill();
        eprintln!("[SKIP] a_client_refuses_an_impostor_in_another_cgroup: impostor never bound");
        return;
    }

    let result = IpcClient::connect(&path).await;
    let _ = impostor.kill();
    let _ = impostor.wait();

    let err = result
        .err()
        .expect("a client must refuse a listener outside the session's cgroup");
    let msg = err.to_string();
    assert!(
        msg.contains("not shepherd's daemon") || msg.contains("could not identify"),
        "refused, but unhelpfully: {msg}"
    );
}

/// The daemon should notice its socket being taken, so a session that has gone
/// unreachable says so instead of just failing to work.
#[tokio::test]
async fn the_daemon_notices_its_socket_being_replaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("lunchboxd.sock");

    let mut server = IpcServer::new(&path);
    server.start().await.expect("bind");
    assert!(
        !server.socket_was_replaced(),
        "a freshly bound socket must not look replaced"
    );

    // What an activity does: take the name.
    std::fs::remove_file(&path).expect("unlink");
    assert!(
        server.socket_was_replaced(),
        "an unlinked socket must be noticed"
    );

    let _impostor = std::os::unix::net::UnixListener::bind(&path).expect("impostor binds");
    assert!(
        server.socket_was_replaced(),
        "a different socket at the same path must be noticed"
    );
}
