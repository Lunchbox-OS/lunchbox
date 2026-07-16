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
//! * `waydroid app launch` / `waydroid prop` / `waydroid status` /
//!   `waydroid session start|stop` run as the **session user** (the same user
//!   shepherdd runs as).
//! * `waydroid shell …` (`am force-stop`) and `systemctl start
//!   waydroid-container` need **root**. shepherdd is unprivileged, so those go
//!   through the `shepherd-waydroid-helper` via pkexec ([`force_stop`],
//!   [`preboot_container`]). Both are best-effort: closing the Wayland toplevel
//!   already ends the visible session; the helper just reclaims the cached
//!   process and ensures the container is up.

use std::time::Duration;

use shepherd_host_api::{HostError, HostResult};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// The log line `waydroid session start` prints once Android is fully up.
pub const READY_MARKER: &str = "Android with user 0 is ready";

/// True if `line` is the session-ready marker.
pub fn is_ready_line(line: &str) -> bool {
    line.contains(READY_MARKER)
}

/// Path to the privileged Waydroid helper. Overridable via
/// `SHEPHERD_WAYDROID_HELPER` for development installs (mirrors
/// `SHEPHERD_FIREWALL_HELPER`).
const DEFAULT_WAYDROID_HELPER_PATH: &str = "/usr/libexec/shepherd-waydroid-helper";

fn waydroid_helper_path() -> String {
    std::env::var("SHEPHERD_WAYDROID_HELPER")
        .unwrap_or_else(|_| DEFAULT_WAYDROID_HELPER_PATH.to_string())
}

/// The Wayland `app_id` waydroid's hwcomposer assigns an app in multi-window
/// mode: the literal `waydroid.` followed by the Android package name
/// (confirmed from `wayland-hwc.cpp` and on the bench).
pub fn app_id_for_package(package: &str) -> String {
    format!("waydroid.{package}")
}

/// The single Wayland `app_id` of the full-UI surface (`waydroid show-full-ui`),
/// which renders the whole Android display. The `locktask` path tracks this
/// instead of a per-app toplevel, because Lock Task Mode suppresses those.
/// Confirmed exactly `"Waydroid"` on the bench.
pub const FULL_UI_APP_ID: &str = "Waydroid";

/// `argv` to launch an Android app as the session user.
pub fn launch_argv(package: &str) -> Vec<String> {
    vec![
        "waydroid".into(),
        "app".into(),
        "launch".into(),
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

/// Whether Android inside the running session has finished booting
/// (`sys.boot_completed == 1`), via the privileged helper (`waydroid shell
/// getprop` needs root). This differs from [`session_running`], which only
/// reports the *session process* is up — true within a second of `session
/// start`, ~20-60s before Android is usable. Launching in that window drops the
/// child on the boot animation, so the readiness gate keys on this instead.
/// Returns false if the session is down or the helper isn't installed.
pub async fn boot_completed() -> bool {
    match Command::new("pkexec")
        .arg(waydroid_helper_path())
        .arg("boot-completed")
        .status()
        .await
    {
        Ok(status) => status.success(),
        Err(e) => {
            debug!(error = %e, "boot-completed helper failed; treating Android as not booted");
            false
        }
    }
}

/// Best-effort: grow a just-launched multi-window app to fill the display, via
/// the privileged helper (`pkexec shepherd-waydroid-helper maximize`). Waydroid
/// opens each multi-window app in a small default freeform window and does not
/// resize the Android task to follow the host window, so without this the app is
/// a scaled partial window; the helper `am task resize`s it to the full display
/// (freeform, so the app fills in landscape rather than honoring a portrait
/// lock). Only meaningful in the multi-window modes (statusbar/off), not
/// locktask's single full-UI surface. Logs and returns on failure.
pub async fn maximize(package: &str) {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .args(["maximize", "--package", package])
        .status()
        .await;
    match result {
        Ok(status) if status.success() => debug!(package, "maximized Android app window"),
        Ok(status) => debug!(
            package,
            %status,
            "maximize helper did not succeed (helper not installed, app not on top, or resize unsupported)"
        ),
        Err(e) => warn!(package, error = %e, "failed to invoke maximize helper"),
    }
}

/// Best-effort: send Android `KEYCODE_BACK` to the foreground app, via the
/// privileged helper (`pkexec shepherd-waydroid-helper back`). Drives the HUD's
/// back button — in `lock_mode = "statusbar"` the app is fullscreened under the
/// HUD, which hides Android's own caption back button. Logs and returns on
/// failure.
pub async fn back() {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .arg("back")
        .status()
        .await;
    match result {
        Ok(status) if status.success() => debug!("sent Android back key"),
        Ok(status) => {
            debug!(%status, "back helper did not succeed (helper not installed or polkit denied?)")
        }
        Err(e) => warn!(error = %e, "failed to invoke back helper"),
    }
}

/// How long to wait for `waydroid app launch` before giving up. It normally
/// returns in ~1s (it just delivers the launch intent; the window maps later).
/// But when the Waydroid session's platform service is wedged — e.g. after many
/// launch/close cycles — it instead loops "Failed to get service
/// waydroidplatform" *forever*. Without a bound, `spawn_android` blocks on it and
/// the launcher is stuck on a "Loading" spinner with no way to cancel (issue:
/// close/reopen wedge). Bounding it turns the wedge into a clean SpawnFailed so
/// the session ends and the launcher returns to the grid.
const LAUNCH_APP_TIMEOUT: Duration = Duration::from_secs(20);

/// Why [`launch_app`] failed, so the adapter can react differently.
pub enum LaunchError {
    /// `waydroid app launch` did not return within [`LAUNCH_APP_TIMEOUT`] — the
    /// session's host↔platform (`waydroidplatform`) bridge is wedged (seen after
    /// racing launch/close churn). Recover by restarting the session.
    Wedged,
    /// Any other launch failure.
    Failed(HostError),
}

/// Launch an Android app by package name (session user). Returns once the
/// launch command returns — the app's window appears asynchronously after.
pub async fn launch_app(package: &str) -> Result<(), LaunchError> {
    let argv = launch_argv(package);
    // Spawn (not `.status()`) so we can kill it if it hangs. `kill_on_drop`
    // reaps it if we bail on any error path.
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            LaunchError::Failed(HostError::SpawnFailed(format!(
                "failed to invoke waydroid: {e}"
            )))
        })?;
    let status = match tokio::time::timeout(LAUNCH_APP_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            return Err(LaunchError::Failed(HostError::SpawnFailed(format!(
                "waydroid app launch {package} failed: {e}"
            ))));
        }
        Err(_) => {
            let _ = child.start_kill();
            warn!(
                package,
                "waydroid app launch wedged (platform bridge stuck)"
            );
            return Err(LaunchError::Wedged);
        }
    };
    if !status.success() {
        return Err(LaunchError::Failed(HostError::SpawnFailed(format!(
            "waydroid app launch {package} exited with {status}"
        ))));
    }
    Ok(())
}

/// Best-effort force-stop of an Android app via the privileged helper
/// (`pkexec shepherd-waydroid-helper force-stop --package <pkg>`). Logs and
/// returns on failure rather than erroring — the authoritative session end is
/// closing the Wayland window; this just reclaims the cached Android process
/// where the helper + polkit rule are installed.
pub async fn force_stop(package: &str) {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .args(["force-stop", "--package", package])
        .status()
        .await;
    match result {
        Ok(status) if status.success() => debug!(package, "force-stopped Android app"),
        Ok(status) => debug!(
            package,
            %status,
            "force-stop helper did not succeed (helper not installed or polkit denied?); window close already ended the session"
        ),
        Err(e) => warn!(package, error = %e, "failed to invoke force-stop helper"),
    }
}

/// Whether `package` has a live Android process, via the privileged helper
/// (`pkexec shepherd-waydroid-helper is-running`). False if it isn't running or
/// the helper couldn't run — the caller then proceeds (treats it as stopped).
pub async fn is_app_running(package: &str) -> bool {
    match Command::new("pkexec")
        .arg(waydroid_helper_path())
        .args(["is-running", "--package", package])
        .status()
        .await
    {
        Ok(status) => status.success(),
        Err(e) => {
            debug!(package, error = %e, "is-running helper failed; treating app as stopped");
            false
        }
    }
}

/// How long the pre-launch guard waits for a previous instance to die.
const APP_STOP_TIMEOUT: Duration = Duration::from_secs(6);
/// How long to let Android settle after the process is gone (ActivityManager
/// finishes removing the task) before relaunching.
const APP_STOP_SETTLE: Duration = Duration::from_millis(400);

/// Make sure no previous instance of `package` is still tearing down before we
/// relaunch it. Rapid close/reopen otherwise races the teardown and wedges the
/// host↔platform bridge (see [`LaunchError::Wedged`]); a graceful reopen that
/// waits is fine. Force-stops it (in case a prior stop is still in flight), then
/// polls [`is_app_running`] until the process is gone (bounded — if it won't die
/// we relaunch anyway rather than block forever), plus a short settle. Fast when
/// nothing is running (one `is-running` check ≈ tens of ms).
pub async fn ensure_app_stopped(package: &str) {
    if !is_app_running(package).await {
        return; // already gone — no wait, no force-stop
    }
    force_stop(package).await;
    let deadline = Instant::now() + APP_STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !is_app_running(package).await {
            tokio::time::sleep(APP_STOP_SETTLE).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    warn!(
        package,
        "previous instance still running after {}s; launching anyway",
        APP_STOP_TIMEOUT.as_secs()
    );
}

/// Best-effort: ensure the (root) `waydroid-container` service is up, via the
/// privileged helper (`pkexec shepherd-waydroid-helper preboot`). On a host
/// where the service is already enabled at boot this is a no-op; it exists so
/// preboot can self-heal a stopped container.
pub async fn preboot_container() {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .arg("preboot")
        .status()
        .await;
    match result {
        Ok(status) if status.success() => debug!("waydroid-container started (preboot)"),
        Ok(status) => {
            debug!(%status, "preboot helper did not succeed (helper not installed or polkit denied?)")
        }
        Err(e) => warn!(error = %e, "failed to invoke preboot helper"),
    }
}

/// Best-effort: harden the running Android session against the child leaving
/// the kiosk app, via the privileged helper (`pkexec shepherd-waydroid-helper
/// lock-down` → `cmd statusbar send-disable-flag …`). Disables the notification
/// shade / quick settings (the route to Android Settings) and the nav-bar
/// home/recents/search buttons. System-wide and persistent until SystemUI
/// restarts, so the adapter re-applies it per launch.
pub async fn lock_down() {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .arg("lock-down")
        .status()
        .await;
    match result {
        Ok(status) if status.success() => debug!("applied Waydroid kiosk lock-down"),
        Ok(status) => {
            debug!(%status, "lock-down helper did not succeed (helper not installed or polkit denied?)")
        }
        Err(e) => warn!(error = %e, "failed to invoke lock-down helper"),
    }
}

/// Present the Waydroid full UI — a single `Waydroid` Wayland surface rendering
/// the whole Android display — as a detached child. Used by the `locktask`
/// launch path, where the pinned app has no per-app toplevel. Runs as the
/// session user (attaches to the caller's compositor via WAYLAND_DISPLAY /
/// SWAYSOCK); the process exits when its window is closed or it is killed.
pub fn show_full_ui() -> std::io::Result<Child> {
    Command::new("waydroid").arg("show-full-ui").spawn()
}

/// Best-effort: launch `package` pinned in Lock Task Mode via the DPC, through
/// the privileged helper (`pkexec shepherd-waydroid-helper pin --package <pkg>`
/// → the DPC's `LaunchActivity`). Used by the `lock_mode = "locktask"` launch
/// path; requires the DPC installed + set as device owner.
pub async fn pin(package: &str) {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .args(["pin", "--package", package])
        .status()
        .await;
    match result {
        Ok(status) if status.success() => debug!(package, "pinned Android app in Lock Task Mode"),
        Ok(status) => debug!(
            package,
            %status,
            "pin helper did not succeed (helper not installed, DPC not device owner, or polkit denied?)"
        ),
        Err(e) => warn!(package, error = %e, "failed to invoke pin helper"),
    }
}

/// Best-effort: clear the DPC's Lock Task allowlist so a locked session can end,
/// via the privileged helper (`pkexec shepherd-waydroid-helper unlock` → the
/// DPC's `ControlReceiver`).
pub async fn unlock() {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .arg("unlock")
        .status()
        .await;
    match result {
        Ok(status) if status.success() => debug!("cleared Waydroid Lock Task allowlist"),
        Ok(status) => {
            debug!(%status, "unlock helper did not succeed (helper not installed or polkit denied?)")
        }
        Err(e) => warn!(error = %e, "failed to invoke unlock helper"),
    }
}

/// Read a persistent Waydroid property (session user), trimmed. `None` on any
/// failure or empty value.
pub async fn get_prop(key: &str) -> Option<String> {
    let out = Command::new("waydroid")
        .args(["prop", "get", key])
        .output()
        .await
        .ok()?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Set a persistent Waydroid property (session user).
pub async fn set_prop(key: &str, value: &str) -> HostResult<()> {
    let status = Command::new("waydroid")
        .args(["prop", "set", key, value])
        .status()
        .await
        .map_err(|e| HostError::Internal(format!("failed to invoke waydroid prop set: {e}")))?;
    if !status.success() {
        return Err(HostError::Internal(format!(
            "waydroid prop set {key} {value} exited with {status}"
        )));
    }
    Ok(())
}

/// Stop the Waydroid session (session user). Best-effort.
pub async fn session_stop() {
    if let Err(e) = Command::new("waydroid")
        .args(["session", "stop"])
        .status()
        .await
    {
        warn!(error = %e, "failed to stop waydroid session");
    }
}

/// Start the Waydroid session (session user) and wait until it reports
/// [`READY_MARKER`] on stdout, or `timeout` elapses. The session process keeps
/// running after this returns (we only read its early output). Returns whether
/// readiness was observed. If the session is already running, returns `true`
/// immediately.
pub async fn start_session_and_wait(timeout: Duration) -> bool {
    if session_running().await {
        return true;
    }
    let mut child = match Command::new("waydroid")
        .args(["session", "start"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to start waydroid session");
            return false;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        return session_running().await;
    };
    // Detach the child: dropping a tokio `Child` does not kill it (no
    // kill_on_drop), so the session survives for the lifetime of the host.
    let deadline = Instant::now() + timeout;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            warn!("waydroid session did not report ready within timeout");
            return false;
        }
        match tokio::time::timeout(remaining, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if is_ready_line(&line) {
                    info!("waydroid session ready");
                    return true;
                }
            }
            // stdout closed, read error, or our timeout fired.
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return session_running().await,
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
    fn ready_line_detection() {
        assert!(is_ready_line("[09:02:41] Android with user 0 is ready"));
        assert!(!is_ready_line("[09:02:41] Starting Android container"));
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
