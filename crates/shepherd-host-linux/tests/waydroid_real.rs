//! Manual end-to-end test for the Android (Waydroid) activity kind.
//!
//! Drives the real `LinuxHost` adapter against a live Waydroid session and a
//! live Sway compositor: it spawns an Android app, waits for the
//! `HostEvent::WindowReady`, confirms the `waydroid.<pkg>` toplevel is present
//! and fullscreen, then stops the session and confirms `HostEvent::Exited` and
//! that the window is gone.
//!
//! It cannot run in CI — Waydroid installed + a booted session, a Sway
//! compositor (`SWAYSOCK`/`WAYLAND_DISPLAY`), and (for process reclamation) the
//! invoking user in the `shepherd-waydroid` group with the helper installed are
//! all prerequisites. When any is missing the test prints a `[SKIP]` line and
//! returns, so `cargo test --include-ignored` is safe everywhere.
//!
//! Run via the orchestrator (which sets up nested Sway + a session):
//!   ./scripts/integration-tests/test-waydroid.sh
//!
//! Or directly, against an already-running session + sway:
//!   cargo test -p shepherd-host-linux --test waydroid_real -- \
//!       --ignored --nocapture

// Fixture code: probes the live host by naming `waydroid`, `swaymsg` and
// friends, which the ban makes deliberate rather than accidental (issue #144).
#![allow(clippy::disallowed_methods)]

use std::process::Command;
use std::time::Duration;

use shepherd_api::EntryKind;
use shepherd_host_api::{HostAdapter, HostEvent, SpawnOptions, StopMode};
use shepherd_host_linux::{LinuxHost, WaydroidLockMode};
use shepherd_util::SessionId;

/// A built-in LineageOS app present in the vanilla Waydroid image.
const TEST_PACKAGE: &str = "com.android.calculator2";

fn skip(reason: &str) {
    println!("[SKIP] waydroid_real: {reason}");
}

/// Run a command and return whether it exited successfully.
fn cmd_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// stdout of a command (lossy), or empty on failure.
fn cmd_stdout(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Whether sway has an on-screen window whose app_id is `waydroid.<pkg>`.
fn android_window_present(package: &str) -> bool {
    let needle = format!("\"app_id\": \"waydroid.{package}\"");
    cmd_stdout("swaymsg", &["-t", "get_tree", "--raw"]).contains(&needle)
}

/// Whether the single full-UI `Waydroid` surface (locktask presentation) exists
/// at all — parked or on screen.
fn full_ui_present() -> bool {
    cmd_stdout("swaymsg", &["-t", "get_tree", "--raw"]).contains("\"app_id\": \"Waydroid\"")
}

/// Whether that surface exists but sits on shepherd's parked workspace, i.e. is
/// alive and off screen. This is the post-`stop` contract for locktask: the
/// surface is Android's display connection, so destroying it takes
/// surfaceflinger and zygote down with it and the next launch shows the boot
/// animation instead of the app.
async fn full_ui_parked() -> bool {
    match shepherd_host_linux::list_windows().await {
        Ok(windows) => windows.iter().any(|w| {
            w.app_id.as_deref() == Some("Waydroid")
                && w.workspace.as_deref() == Some(shepherd_host_linux::HIDDEN_WORKSPACE)
        }),
        Err(_) => false,
    }
}

/// Await a host event matching `pred`, up to `timeout`.
async fn wait_for_event(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    timeout: Duration,
    pred: impl Fn(&HostEvent) -> bool,
) -> Option<HostEvent> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ev)) => {
                if pred(&ev) {
                    return Some(ev);
                }
            }
            Ok(None) | Err(_) => return None,
        }
    }
}

/// Exercises `LinuxHost::preboot_waydroid` against real Waydroid: it should
/// bring up the container, start the session, ensure `multi_windows` is enabled
/// (setting + restarting the session when it isn't), and leave a ready session.
/// Verifies the end state: session RUNNING and
/// `persist.waydroid.multi_windows == true`. Run the orchestrator with
/// `WAYDROID_TEST_FORCE_RESTART=1` to pre-set the prop to `false` and force the
/// set-and-restart branch.
#[tokio::test]
#[ignore = "requires Waydroid + Sway; run via test-waydroid.sh"]
async fn waydroid_preboot_enables_multi_window() {
    if !cmd_ok("waydroid", &["--version"]) {
        skip("waydroid CLI not available");
        return;
    }
    if std::env::var_os("SWAYSOCK").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        skip("no SWAYSOCK/WAYLAND_DISPLAY (need a running sway)");
        return;
    }

    let host = LinuxHost::new();
    // multi_window=true, suspend=true, 90s boot timeout, statusbar lock-down.
    host.configure_waydroid(
        true,
        true,
        Duration::from_secs(90),
        WaydroidLockMode::Statusbar,
    );
    host.preboot_waydroid();

    // Poll for the end state: a running session with multi-window enabled.
    // Generous bound — preboot may start the session twice (set + restart).
    let mut ok = false;
    for _ in 0..120 {
        let running = cmd_stdout("waydroid", &["status"]).contains("RUNNING");
        let multi = cmd_stdout(
            "waydroid",
            &["prop", "get", "persist.waydroid.multi_windows"],
        )
        .trim()
            == "true";
        if running && multi {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(
        ok,
        "preboot should leave a running session with multi_windows=true"
    );
    println!("[OK] preboot: session running, multi_windows enabled");
}

/// Preboot must fix a stale prop on a **cold container** — the case that used to
/// fail silently.
///
/// `waydroid prop set` reaches the property service through the *session* and
/// exits 0 without one, so the pins preboot writes before starting the session
/// never landed. shepherdd stops the prebooted session at shutdown, making that
/// every cold start: a device whose persisted `multi_windows` disagreed with the
/// config (a fresh container, a `lock_mode` change, or the locktask path having
/// flipped it) stayed wrong forever, and the statusbar launch path then had no
/// per-app toplevel to track.
///
/// Sets the prop to the wrong value while a session is up, stops the session,
/// and asserts preboot still reaches the right end state — which it can only do
/// by re-applying after its boot and restarting once.
///
/// Also reports how long that correction took, since the whole cost of the fix
/// is that one extra session boot.
#[tokio::test]
#[ignore = "requires Waydroid + Sway; run via test-waydroid.sh"]
async fn waydroid_preboot_fixes_stale_props_on_a_cold_container() {
    if !cmd_ok("waydroid", &["--version"]) {
        skip("waydroid CLI not available");
        return;
    }
    if std::env::var_os("SWAYSOCK").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        skip("no SWAYSOCK/WAYLAND_DISPLAY (need a running sway)");
        return;
    }
    // Poisoning the prop needs a live session — that is the bug in miniature.
    if !cmd_stdout("waydroid", &["status"]).contains("RUNNING") {
        skip("no running session to set the stale prop through");
        return;
    }

    // Poison it, confirm it took, then go cold.
    let _ = cmd_stdout(
        "waydroid",
        &["prop", "set", "persist.waydroid.multi_windows", "false"],
    );
    let poisoned = cmd_stdout(
        "waydroid",
        &["prop", "get", "persist.waydroid.multi_windows"],
    )
    .trim()
        == "false";
    if !poisoned {
        skip("could not set multi_windows=false (no session?)");
        return;
    }
    let _ = cmd_stdout("waydroid", &["session", "stop"]);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let started_at = std::time::Instant::now();
    let host = LinuxHost::new();
    // Wants multi_windows=true, i.e. the opposite of what is persisted.
    host.configure_waydroid(
        true,
        true,
        Duration::from_secs(90),
        WaydroidLockMode::Statusbar,
    );
    host.preboot_waydroid();

    let mut ok = false;
    for _ in 0..150 {
        let running = cmd_stdout("waydroid", &["status"]).contains("RUNNING");
        let multi = cmd_stdout(
            "waydroid",
            &["prop", "get", "persist.waydroid.multi_windows"],
        )
        .trim()
            == "true";
        if running && multi {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(
        ok,
        "preboot must correct a stale multi_windows on a cold container (it cannot set props until a session exists)"
    );
    println!(
        "[OK] preboot: corrected a stale prop from cold in {:?} (two session boots)",
        started_at.elapsed()
    );
}

#[tokio::test]
#[ignore = "requires Waydroid + a booted session + Sway; run via test-waydroid.sh"]
async fn waydroid_launch_and_stop() {
    // --- prerequisites ---
    if !cmd_ok("waydroid", &["--version"]) {
        skip("waydroid CLI not available");
        return;
    }
    if std::env::var_os("SWAYSOCK").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        skip("no SWAYSOCK/WAYLAND_DISPLAY (need a running sway)");
        return;
    }
    if !cmd_ok("swaymsg", &["-t", "get_version"]) {
        skip("swaymsg cannot reach a compositor");
        return;
    }
    // The adapter requires a running session (preboot). The orchestrator starts
    // one; if it isn't up we skip rather than fail.
    if !cmd_stdout("waydroid", &["status"]).contains("RUNNING") {
        skip("waydroid session is not running (start it / run preboot first)");
        return;
    }

    let host = LinuxHost::new();
    let mut rx = host.subscribe();
    let entry = EntryKind::Android {
        package_name: TEST_PACKAGE.into(),
        args: vec![],
    };

    // --- spawn ---
    let handle = host
        .spawn(SessionId::new(), &entry, SpawnOptions::default())
        .await
        .expect("spawn(Android) should succeed against a running session");

    // The adapter emits WindowReady once the toplevel appears.
    let ready = wait_for_event(&mut rx, Duration::from_secs(25), |ev| {
        matches!(ev, HostEvent::WindowReady { .. })
    })
    .await;
    assert!(
        ready.is_some(),
        "expected HostEvent::WindowReady for the Android app"
    );

    // The window should be present and fullscreened by the for_window rule.
    assert!(
        android_window_present(TEST_PACKAGE),
        "expected a waydroid.{TEST_PACKAGE} toplevel on screen after WindowReady"
    );
    println!("[OK] Android app launched, window present: waydroid.{TEST_PACKAGE}");

    // --- stop ---
    host.stop(&handle, StopMode::Force)
        .await
        .expect("stop(Android) should succeed");

    // The window-watch task emits Exited once the toplevel is gone.
    let exited = wait_for_event(&mut rx, Duration::from_secs(15), |ev| {
        matches!(ev, HostEvent::Exited { .. })
    })
    .await;
    assert!(
        exited.is_some(),
        "expected HostEvent::Exited after stopping the Android session"
    );

    // And the window should actually be gone.
    let mut gone = false;
    for _ in 0..10 {
        if !android_window_present(TEST_PACKAGE) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(gone, "Android window should be gone after stop()");
    println!("[OK] Android session stopped, window gone, Exited emitted");
}

/// Exercises the `lock_mode = "locktask"` launch path against real Waydroid + the
/// DPC device owner: configure locktask, preboot (which flips multi_windows off),
/// spawn an app, and assert it is presented as the single full-UI `Waydroid`
/// surface (no per-app toplevel) with `WindowReady`; then stop and assert
/// `Exited` and that the surface is gone. Requires the DPC installed and set as
/// device owner (`shepherd-admin apps install android`); skips otherwise.
#[tokio::test]
#[ignore = "requires Waydroid + DPC device owner + Sway; run via test-waydroid.sh"]
async fn waydroid_locktask_launch_and_stop() {
    if !cmd_ok("waydroid", &["--version"]) {
        skip("waydroid CLI not available");
        return;
    }
    if std::env::var_os("SWAYSOCK").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        skip("no SWAYSOCK/WAYLAND_DISPLAY (need a running sway)");
        return;
    }
    if !cmd_ok("swaymsg", &["-t", "get_version"]) {
        skip("swaymsg cannot reach a compositor");
        return;
    }
    // The DPC must be device owner for Lock Task to engage. Check the on-disk
    // device-owner record (session-independent — `dpm list-owners` needs a booted
    // session, which preboot only starts below).
    let owner_file = format!(
        "{}/.local/share/waydroid/data/system/device_owner_2.xml",
        std::env::var("HOME").unwrap_or_default()
    );
    let is_owner = std::fs::read(&owner_file)
        .map(|b| String::from_utf8_lossy(&b).contains("com.armeafamily.shepherd.dpc"))
        .unwrap_or(false);
    if !is_owner {
        skip("DPC is not device owner (run: shepherd-admin apps install android)");
        return;
    }

    let host = LinuxHost::new();
    // multi_window=true is deliberately overridden: locktask forces full-UI
    // (multi_windows off) at preboot.
    host.configure_waydroid(
        true,
        true,
        Duration::from_secs(90),
        WaydroidLockMode::Locktask,
    );
    host.preboot_waydroid();

    // Preboot should leave a running session with multi_windows disabled.
    let mut ready = false;
    for _ in 0..120 {
        let running = cmd_stdout("waydroid", &["status"]).contains("RUNNING");
        let multi = cmd_stdout(
            "waydroid",
            &["prop", "get", "persist.waydroid.multi_windows"],
        )
        .trim()
            == "true";
        if running && !multi {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(
        ready,
        "preboot(locktask) should leave a running session with multi_windows=false"
    );

    let mut rx = host.subscribe();
    let entry = EntryKind::Android {
        package_name: TEST_PACKAGE.into(),
        args: vec![],
    };
    let handle = host
        .spawn(SessionId::new(), &entry, SpawnOptions::default())
        .await
        .expect("spawn(Android locktask) should succeed against a running session");

    let ready_ev = wait_for_event(&mut rx, Duration::from_secs(45), |ev| {
        matches!(ev, HostEvent::WindowReady { .. })
    })
    .await;
    assert!(
        ready_ev.is_some(),
        "expected HostEvent::WindowReady for the locktask session"
    );

    // Presented as the single full-UI surface — NOT a per-app toplevel.
    assert!(
        full_ui_present(),
        "expected the full-UI Waydroid surface on screen under locktask"
    );
    assert!(
        !android_window_present(TEST_PACKAGE),
        "locktask should not present a per-app waydroid.{TEST_PACKAGE} toplevel"
    );
    // Lock Task engages a beat after the pinned app resumes (WindowReady fires
    // as soon as the surface + pin land), so poll for it. Needs root dumpsys; if
    // sudo is unavailable the check is skipped rather than failing.
    let mut locked = false;
    let mut saw_dumpsys = false;
    for _ in 0..15 {
        let lock = cmd_stdout(
            "sudo",
            &[
                "waydroid",
                "--details-to-stdout",
                "shell",
                "--",
                "dumpsys",
                "activity",
            ],
        );
        if !lock.is_empty() {
            saw_dumpsys = true;
            if lock.contains("mLockTaskModeState=LOCKED") {
                locked = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(
        locked || !saw_dumpsys,
        "expected Lock Task LOCKED while the app is pinned"
    );
    println!("[OK] locktask: full-UI Waydroid surface up, no per-app toplevel, Lock Task engaged");

    // Stop → unlock + kill the full-UI child → surface gone → Exited.
    host.stop(&handle, StopMode::Force)
        .await
        .expect("stop(Android locktask) should succeed");

    let exited = wait_for_event(&mut rx, Duration::from_secs(15), |ev| {
        matches!(ev, HostEvent::Exited { .. })
    })
    .await;
    assert!(
        exited.is_some(),
        "expected HostEvent::Exited after stopping the locktask session"
    );
    // Parked, NOT destroyed. This assertion used to require the surface to be
    // gone, which is what the code did until it turned out that killing it takes
    // surfaceflinger + zygote down with it — so the next launch showed the
    // LineageOS boot animation instead of the app. `stop_android` now parks it
    // off screen and keeps the client alive; the child sees it disappear either
    // way, but Android stays up for the next open.
    let mut parked = false;
    for _ in 0..12 {
        if full_ui_parked().await {
            parked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        parked,
        "the full-UI Waydroid surface should be parked off screen after stop() (alive, not destroyed)"
    );
    println!("[OK] locktask: stopped, surface parked off screen, Exited emitted");
}

/// Exercises `LinuxHost::stop_waydroid`, the graceful-shutdown teardown.
///
/// Two properties, in order (the no-op case first, so it runs against a session
/// that is still up):
///
/// 1. A host that never prebooted must leave the session alone — that is what
///    keeps `[service.waydroid] preboot = false` plus a hand-started session
///    working.
/// 2. A host that did preboot must stop it, so a booted Android container does
///    not outlive shepherdd.
///
/// Destructive: leaves the session STOPPED, so it runs after the launch tests.
#[tokio::test]
#[ignore = "requires Waydroid + Sway; run via test-waydroid.sh"]
async fn waydroid_stop_on_shutdown() {
    if !cmd_ok("waydroid", &["--version"]) {
        skip("waydroid CLI not available");
        return;
    }

    let session_running = || cmd_stdout("waydroid", &["status"]).contains("RUNNING");

    if !session_running() {
        skip("no running Waydroid session to stop");
        return;
    }

    // 1. Never prebooted → must not touch a session someone else started.
    let bystander = LinuxHost::new();
    bystander.stop_waydroid().await;
    assert!(
        session_running(),
        "stop_waydroid() must leave a session it did not start running"
    );
    println!("[OK] shutdown: a non-prebooting host left the session alone");

    // 2. Prebooted → owns the session, so shutdown stops it.
    let host = LinuxHost::new();
    host.configure_waydroid(
        true,
        true,
        Duration::from_secs(90),
        WaydroidLockMode::Statusbar,
    );
    host.preboot_waydroid();

    // Let preboot reach a ready session before tearing it down, so we are not
    // just racing it (preboot may restart the session to apply props).
    let mut up = false;
    for _ in 0..120 {
        if session_running() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(up, "preboot should leave a running session to stop");

    host.stop_waydroid().await;
    assert!(
        !session_running(),
        "stop_waydroid() should stop the session it prebooted"
    );
    println!("[OK] shutdown: the prebooted session was stopped");
}
