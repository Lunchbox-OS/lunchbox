//! Waydroid (Android activity kind) integration.
//!
//! Waydroid runs a single global Android (LineageOS) container on the host and,
//! in multi-window mode, presents each app as its own Wayland toplevel with
//! `app_id = "waydroid.<package>"`. This module wraps the `waydroid` CLI and
//! provides the pure helpers the adapter needs to launch, locate, and stop an
//! Android app.
//!
//! Privilege split (validated on the bench — see
//! `docs/ai/history/2026-06-28 002 android-phase0-host-spike.md`):
//!
//! * `waydroid app launch` / `waydroid prop` / `waydroid status` run as the
//!   **session user** (the same user shepherdd runs as).
//! * `waydroid shell …` (and thus `am force-stop`) needs **root**. shepherdd is
//!   unprivileged, so [`force_stop`] is best-effort: it only succeeds where a
//!   privileged path is available. The reliable, user-level way to end a
//!   session is to close the app's Wayland toplevel via Sway (the adapter does
//!   this in `stop`), which removes it from screen immediately; Android may keep
//!   the process cached, which `force_stop` reclaims when it can.

use shepherd_host_api::{HostError, HostResult};
use tokio::process::Command;
use tracing::{debug, warn};

/// The Wayland `app_id` waydroid's hwcomposer assigns an app in multi-window
/// mode: the literal `waydroid.` followed by the Android package name
/// (confirmed from `wayland-hwc.cpp` and on the bench).
pub fn app_id_for_package(package: &str) -> String {
    format!("waydroid.{package}")
}

/// `argv` to launch an Android app as the session user.
pub fn launch_argv(package: &str) -> Vec<String> {
    vec![
        "waydroid".into(),
        "app".into(),
        "launch".into(),
        package.into(),
    ]
}

/// `argv` to force-stop an Android app. Goes through `waydroid shell`, which
/// requires root — run via a privileged seam, not directly from shepherdd.
pub fn force_stop_argv(package: &str) -> Vec<String> {
    vec![
        "waydroid".into(),
        "shell".into(),
        "am".into(),
        "force-stop".into(),
        package.into(),
    ]
}

/// Parse the `Session:\tRUNNING|STOPPED` line out of `waydroid status` output.
/// Returns `true` only when the session is explicitly `RUNNING`.
pub fn parse_session_running(status_output: &str) -> bool {
    status_output.lines().any(|line| {
        let mut parts = line.split_whitespace();
        matches!(
            (parts.next(), parts.next()),
            (Some("Session:"), Some("RUNNING"))
        )
    })
}

/// Whether the Waydroid session is currently running. A stopped session means
/// no app can be launched yet (the daemon should preboot it).
pub async fn session_running() -> bool {
    match Command::new("waydroid").arg("status").output().await {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            parse_session_running(&text)
        }
        Err(e) => {
            debug!(error = %e, "waydroid status failed; treating session as not running");
            false
        }
    }
}

/// Launch an Android app by package name (session user). Returns once the
/// launch command returns — the app's window appears asynchronously after.
pub async fn launch_app(package: &str) -> HostResult<()> {
    let argv = launch_argv(package);
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .await
        .map_err(|e| HostError::SpawnFailed(format!("failed to invoke waydroid: {e}")))?;
    if !status.success() {
        return Err(HostError::SpawnFailed(format!(
            "waydroid app launch {package} exited with {status}"
        )));
    }
    Ok(())
}

/// Best-effort force-stop of an Android app. Requires root; shepherdd is
/// unprivileged, so this logs and returns `Ok(())` on failure rather than
/// failing the stop — the authoritative session end is closing the Wayland
/// window. When a privileged seam (pkexec helper) lands, route this through it.
pub async fn force_stop(package: &str) {
    let argv = force_stop_argv(package);
    match Command::new(&argv[0]).args(&argv[1..]).status().await {
        Ok(status) if status.success() => {
            debug!(package, "force-stopped Android app");
        }
        Ok(status) => {
            // Expected when running unprivileged ("Action \"shell\" needs root").
            debug!(
                package,
                %status, "waydroid force-stop did not succeed (likely needs root); window close already ended the session"
            );
        }
        Err(e) => {
            warn!(package, error = %e, "failed to invoke waydroid force-stop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_matches_waydroid_convention() {
        assert_eq!(
            app_id_for_package("com.android.calculator2"),
            "waydroid.com.android.calculator2"
        );
    }

    #[test]
    fn launch_argv_is_user_level() {
        assert_eq!(
            launch_argv("com.mojang.minecraftpe"),
            vec!["waydroid", "app", "launch", "com.mojang.minecraftpe"]
        );
    }

    #[test]
    fn force_stop_argv_uses_am() {
        assert_eq!(
            force_stop_argv("org.khanacademy.android.kids"),
            vec![
                "waydroid",
                "shell",
                "am",
                "force-stop",
                "org.khanacademy.android.kids"
            ]
        );
    }

    #[test]
    fn session_running_parses_status() {
        assert!(parse_session_running(
            "Session:\tRUNNING\nVendor type:\tMAINLINE\n"
        ));
        assert!(!parse_session_running(
            "Session:\tSTOPPED\nVendor type:\tMAINLINE\n"
        ));
        assert!(!parse_session_running(""));
        // Defensive: a RUNNING substring elsewhere must not be a false positive.
        assert!(!parse_session_running("Note: not RUNNING right now"));
    }
}
