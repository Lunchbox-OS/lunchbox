//! Process management utilities

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::RwLock;
use tracing::{debug, info, warn};

use shepherd_host_api::{ExitStatus, FirewallSpec, HostError, HostResult};

/// Path to the privileged firewall helper. Overridable via
/// `SHEPHERD_FIREWALL_HELPER` for development installs.
pub const DEFAULT_FIREWALL_HELPER_PATH: &str = "/usr/libexec/shepherd-firewall-helper";

/// Resolve the firewall helper path, honoring the env override.
pub fn firewall_helper_path() -> String {
    std::env::var("SHEPHERD_FIREWALL_HELPER")
        .unwrap_or_else(|_| DEFAULT_FIREWALL_HELPER_PATH.to_string())
}

/// Whether the host can actually enforce per-cgroup IP filters.
///
/// systemd backs `IPAddressDeny=`/`IPAddressAllow=` with cgroup BPF
/// (`cgroup_skb`), which requires `CAP_NET_ADMIN` to attach. The per-user
/// systemd manager doesn't have that capability, so we cannot apply IP filters
/// directly from shepherdd. We delegate to the privileged helper at
/// `/usr/libexec/shepherd-firewall-helper`, which is invoked via `pkexec`.
///
/// "Supported" therefore means: the helper is installed AND polkit grants the
/// current user the `org.shepherd.firewall.apply-process` action without an
/// auth prompt. Both are checked at startup.
#[derive(Debug, Clone)]
pub enum FirewallEnforcementStatus {
    Supported,
    Unsupported { reason: String },
}

impl FirewallEnforcementStatus {
    pub fn is_supported(&self) -> bool {
        matches!(self, FirewallEnforcementStatus::Supported)
    }
}

static FW_STATUS: RwLock<Option<FirewallEnforcementStatus>> = RwLock::new(None);

/// Cached probe of whether per-cgroup IP filters can be enforced from this
/// process. Checks for the helper binary and a non-prompted polkit grant.
///
/// Cached because the launch path consults it on every spawn and the probe
/// execs `pkcheck`. Refreshable because it used to be a `OnceLock`, which meant
/// an administrator who installed the helper stayed "unsupported" until the
/// daemon restarted — and, now that this gates launches and raises a diagnostic
/// (issue #143), that a fixed host would keep losing activities and keep
/// showing a stale problem.
pub fn firewall_enforcement_status() -> FirewallEnforcementStatus {
    if let Some(cached) = FW_STATUS.read().expect("fw status lock").clone() {
        return cached;
    }
    refresh_firewall_enforcement()
}

/// Re-run the probe and replace the cached value. Called by the daemon's
/// diagnostic sweep, so installing the helper takes effect within the sweep
/// interval rather than at the next restart.
pub fn refresh_firewall_enforcement() -> FirewallEnforcementStatus {
    let fresh = probe_firewall_enforcement();
    *FW_STATUS.write().expect("fw status lock") = Some(fresh.clone());
    fresh
}

fn probe_firewall_enforcement() -> FirewallEnforcementStatus {
    let helper = firewall_helper_path();
    if !std::path::Path::new(&helper).exists() {
        return FirewallEnforcementStatus::Unsupported {
            reason: format!(
                "shepherd-firewall-helper not installed at {} -- run \
                 scripts/integration-tests/setup-firewall-dev.sh (dev) or \
                 `shepherd install firewall` (production), or set \
                 SHEPHERD_FIREWALL_HELPER to its path",
                helper
            ),
        };
    }
    match probe_polkit_grant() {
        Ok(()) => FirewallEnforcementStatus::Supported,
        Err(e) => FirewallEnforcementStatus::Unsupported {
            reason: format!(
                "polkit denies non-prompted access to org.shepherd.firewall.apply-process: \
                 {}. Install dist/polkit/50-shepherd-firewall.rules and add this user to \
                 the `shepherd-firewall` group (see scripts/integration-tests/setup-firewall-dev.sh).",
                e
            ),
        },
    }
}

/// Run `pkcheck` (without `--allow-user-interaction`) so it returns success
/// only if the action is granted with no auth prompt required.
fn probe_polkit_grant() -> Result<(), String> {
    let pid = std::process::id();
    let output = std::process::Command::new("pkcheck")
        .args([
            "--action-id",
            "org.shepherd.firewall.apply-process",
            "--process",
            &pid.to_string(),
        ])
        .output()
        .map_err(|e| format!("could not exec pkcheck: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.trim().to_string())
    }
}

/// Build the argv prefix for spawning a Process-kind activity through the
/// privileged firewall helper. The full argv handed to `Command::spawn` is the
/// returned prefix plus the activity's own command + args.
///
/// `inherit_env` is the environment shepherdd would otherwise have set on the
/// activity. We pass it as `--env KEY=VAL` to the helper because `pkexec`
/// strips the parent environment.
pub fn firewall_helper_argv_prefix(
    spec: &FirewallSpec,
    scope_name: &str,
    uid: u32,
    gid: u32,
    inherit_env: &HashMap<String, String>,
    cwd: Option<&std::path::Path>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "pkexec".into(),
        // Preserve shepherdd's cwd so the activity's effective cwd matches
        // the no-firewall path (pkexec otherwise resets to root's home).
        "--keep-cwd".into(),
        firewall_helper_path(),
        "apply-process".into(),
        "--scope-name".into(),
        scope_name.into(),
        "--uid".into(),
        uid.to_string(),
        "--gid".into(),
        gid.to_string(),
        "--default".into(),
        if spec.default_deny { "deny" } else { "allow" }.into(),
    ];
    for r in &spec.allow {
        args.push("--allow".into());
        args.push(r.clone());
    }
    for r in &spec.deny {
        args.push("--deny".into());
        args.push(r.clone());
    }
    if let Some(c) = cwd {
        args.push("--cwd".into());
        args.push(c.to_string_lossy().into_owned());
    }
    // Pass env through args because pkexec sanitizes its parent's env.
    let mut keys: Vec<&String> = inherit_env.keys().collect();
    keys.sort(); // deterministic for tests / logs
    for k in keys {
        args.push("--env".into());
        args.push(format!("{}={}", k, inherit_env[k]));
    }
    args.push("--".into());
    args
}

/// Generate a unique systemd scope name for a session. Used as the
/// `--scope-name` argument to the helper.
pub fn make_scope_name(session_id: &str) -> String {
    // systemd unit names allow alnum + `-_.\:@`. Session IDs are UUIDs from
    // shepherd-util, which only contain hex + dashes -- safe to embed verbatim.
    format!("shepherd-{}.scope", session_id)
}

/// Variables shepherdd inherits from its own env when launching an activity.
/// Centralized so both the direct spawn path and the firewall-helper path use
/// the same set.
const INHERITED_ENV_VARS: &[&str] = &[
    // Core paths
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    // Display/graphics - both X11 and Wayland
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_DESKTOP",
    "XDG_CURRENT_DESKTOP",
    "XAUTHORITY",
    // XDG directories
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    // Snap support
    "SNAP",
    "SNAP_USER_DATA",
    "SNAP_USER_COMMON",
    "SNAP_REAL_HOME",
    "SNAP_NAME",
    "SNAP_INSTANCE_NAME",
    "SNAP_ARCH",
    "SNAP_VERSION",
    "SNAP_REVISION",
    "SNAP_COMMON",
    "SNAP_DATA",
    "SNAP_LIBRARY_PATH",
    // Locale
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    // D-Bus
    "DBUS_SESSION_BUS_ADDRESS",
    // Graphics/GPU
    "LIBGL_ALWAYS_SOFTWARE",
    "__GLX_VENDOR_LIBRARY_NAME",
    "VK_ICD_FILENAMES",
    "MESA_LOADER_DRIVER_OVERRIDE",
    // Audio
    "PULSE_SERVER",
    "PULSE_COOKIE",
    // GTK/GLib
    "GTK_MODULES",
    "GIO_EXTRA_MODULES",
    "GSETTINGS_SCHEMA_DIR",
    "GSETTINGS_BACKEND",
    // SSL/TLS
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    // Desktop session info
    "DESKTOP_SESSION",
    "GNOME_DESKTOP_SESSION_ID",
];

/// Build the env map that an activity should run with: inherited vars from
/// shepherdd's own env, plus a few hardcoded overrides, plus the user-specified
/// vars from the entry config (which take precedence). Used by both the direct
/// spawn path and the firewall-helper path.
pub fn build_inherited_env(user_env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for &var in INHERITED_ENV_VARS {
        if let Ok(val) = std::env::var(var) {
            out.insert(var.to_string(), val);
        }
    }
    // Java AWT/Swing on Sway needs this for correct rendering.
    out.insert("_JAVA_AWT_WM_NONREPARENTING".to_string(), "1".to_string());
    // Chromium / Electron password store.
    out.insert("PASSWORD_STORE".to_string(), "gnome".to_string());
    // SHEPHERD_WAYLAND_DISPLAY override (used when the service runs on the
    // parent compositor but apps need to launch into a nested one).
    if let Ok(d) = std::env::var("SHEPHERD_WAYLAND_DISPLAY") {
        out.insert("WAYLAND_DISPLAY".to_string(), d);
    }
    // Entry-specific vars override the inherited defaults.
    for (k, v) in user_env {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// Managed child process with process group tracking
pub struct ManagedProcess {
    pub child: Child,
    pub pid: u32,
    pub pgid: u32,
    /// The command name (for fallback killing via pkill)
    pub command_name: String,
    /// The snap name if this is a snap app (for cgroup-based killing)
    pub snap_name: Option<String>,
}

/// Initialize process management (called once at startup)
pub fn init() {
    info!("Process management initialized");
    match firewall_enforcement_status() {
        FirewallEnforcementStatus::Supported => {
            info!("Per-entry firewall enforcement is available");
        }
        FirewallEnforcementStatus::Unsupported { reason } => {
            warn!(
                reason = %reason,
                "Per-entry firewall enforcement is NOT available; \
                 entries with [entries.firewall] configured will be spawned without filtering"
            );
        }
    }
}

/// Apply a firewall spec to an already-running systemd scope (e.g. a flatpak
/// or snap app whose scope was created by the runtime, not by us).
///
/// Polls the user cgroup hierarchy for a scope whose name starts with
/// `scope_prefix` for up to `timeout`, then invokes the privileged helper's
/// `apply-cgroup` subcommand. The helper attaches a `cgroup_skb/egress` BPF
/// program directly to the cgroup and exits, so the filter persists for the
/// life of the scope. Returns the scope name on success.
///
/// `systemctl --user --runtime set-property IPAddressDeny=…` was the older
/// approach here; per-user systemd lacks `CAP_NET_ADMIN`/`CAP_BPF`, so it
/// silently accepted the property without attaching any BPF program. The
/// helper does the attach itself.
pub async fn apply_firewall_to_existing_scope(
    scope_prefix: &str,
    spec: &FirewallSpec,
    timeout: std::time::Duration,
) -> Option<String> {
    let scope_name = wait_for_scope(scope_prefix, timeout).await?;
    let uid = nix::unistd::getuid().as_raw();
    let cgroup_path = format!(
        "/sys/fs/cgroup/user.slice/user-{0}.slice/user@{0}.service/app.slice/{1}",
        uid, scope_name
    );

    let mut args: Vec<String> = vec![
        "--keep-cwd".into(),
        firewall_helper_path(),
        "apply-cgroup".into(),
        "--cgroup-path".into(),
        cgroup_path.clone(),
        "--default".into(),
        if spec.default_deny { "deny" } else { "allow" }.into(),
    ];
    for rule in &spec.allow {
        args.push("--allow".into());
        args.push(rule.clone());
    }
    for rule in &spec.deny {
        args.push("--deny".into());
        args.push(rule.clone());
    }

    let result = tokio::process::Command::new("pkexec")
        .args(&args)
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            info!(scope = %scope_name, cgroup = %cgroup_path, "Applied firewall (BPF) to scope");
            Some(scope_name)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(scope = %scope_name, stderr = %stderr, "helper apply-cgroup failed");
            None
        }
        Err(e) => {
            warn!(scope = %scope_name, error = %e, "Failed to run pkexec helper");
            None
        }
    }
}

async fn wait_for_scope(prefix: &str, timeout: std::time::Duration) -> Option<String> {
    let uid = nix::unistd::getuid().as_raw();
    let base = std::path::PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{}.slice/user@{}.service/app.slice",
        uid, uid
    ));

    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(prefix) && name_str.ends_with(".scope") {
                    return Some(name_str.into_owned());
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Kill all processes in a snap's cgroup using systemd
/// Snaps create scopes at: snap.<snap-name>.<snap-name>-<uuid>.scope
/// Direct signals don't work due to AppArmor confinement, but systemctl --user does
/// NOTE: We always use SIGKILL for snap apps because apps like Minecraft Launcher
/// have self-restart behavior and will spawn new instances when receiving SIGTERM
pub fn kill_snap_cgroup(snap_name: &str, _signal: Signal) -> bool {
    let uid = nix::unistd::getuid().as_raw();
    let base_path = format!(
        "/sys/fs/cgroup/user.slice/user-{}.slice/user@{}.service/app.slice",
        uid, uid
    );

    // Find all scope directories matching this snap
    let pattern = format!("snap.{}.{}-", snap_name, snap_name);

    let base = std::path::Path::new(&base_path);
    if !base.exists() {
        debug!(path = %base_path, "Snap cgroup base path doesn't exist");
        return false;
    }

    let mut stopped_any = false;

    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with(&pattern) && name_str.ends_with(".scope") {
                let scope_name = name_str.to_string();

                // Always use SIGKILL for snap apps to prevent self-restart behavior
                // Using systemctl kill --signal=KILL sends SIGKILL to all processes in scope
                let result = Command::new("systemctl")
                    .args(["--user", "kill", "--signal=KILL", &scope_name])
                    .output();

                match result {
                    Ok(output) => {
                        if output.status.success() {
                            info!(scope = %scope_name, "Killed snap scope via systemctl SIGKILL");
                            stopped_any = true;
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            warn!(scope = %scope_name, stderr = %stderr, "systemctl kill command failed");
                        }
                    }
                    Err(e) => {
                        warn!(scope = %scope_name, error = %e, "Failed to run systemctl");
                    }
                }
            }
        }
    }

    if stopped_any {
        info!(
            snap = snap_name,
            "Killed snap scope(s) via systemctl SIGKILL"
        );
    } else {
        debug!(snap = snap_name, "No snap scope found to kill");
    }

    stopped_any
}

/// Kill all processes in a Flatpak app's cgroup using systemd
/// Flatpak apps create scopes at: app-flatpak-<app_id>-<number>.scope
/// For example: app-flatpak-org.prismlauncher.PrismLauncher-12345.scope
/// Similar to snap apps, we use systemctl --user to manage the scopes.
pub fn kill_flatpak_cgroup(app_id: &str, _signal: Signal) -> bool {
    let uid = nix::unistd::getuid().as_raw();
    let base_path = format!(
        "/sys/fs/cgroup/user.slice/user-{}.slice/user@{}.service/app.slice",
        uid, uid
    );

    // Flatpak uses a different naming pattern than snap
    // The app_id dots are preserved: app-flatpak-org.example.App-<number>.scope
    let pattern = format!("app-flatpak-{}-", app_id);

    let base = std::path::Path::new(&base_path);
    if !base.exists() {
        debug!(path = %base_path, "Flatpak cgroup base path doesn't exist");
        return false;
    }

    let mut stopped_any = false;

    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with(&pattern) && name_str.ends_with(".scope") {
                let scope_name = name_str.to_string();

                // Always use SIGKILL for flatpak apps to prevent self-restart behavior
                // Using systemctl kill --signal=KILL sends SIGKILL to all processes in scope
                let result = Command::new("systemctl")
                    .args(["--user", "kill", "--signal=KILL", &scope_name])
                    .output();

                match result {
                    Ok(output) => {
                        if output.status.success() {
                            info!(scope = %scope_name, "Killed flatpak scope via systemctl SIGKILL");
                            stopped_any = true;
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            warn!(scope = %scope_name, stderr = %stderr, "systemctl kill command failed");
                        }
                    }
                    Err(e) => {
                        warn!(scope = %scope_name, error = %e, "Failed to run systemctl");
                    }
                }
            }
        }
    }

    if stopped_any {
        info!(
            app_id = app_id,
            "Killed flatpak scope(s) via systemctl SIGKILL"
        );
    } else {
        debug!(app_id = app_id, "No flatpak scope found to kill");
    }

    stopped_any
}

/// Find Steam game process IDs by Steam App ID (from environment variables)
pub fn find_steam_game_pids(app_id: u32) -> Vec<i32> {
    let mut pids = Vec::new();
    let target = app_id.to_string();
    let keys = ["SteamAppId", "SteamAppID", "STEAM_APP_ID"];

    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Ok(pid) = name_str.parse::<i32>() {
                let env_path = format!("/proc/{}/environ", pid);
                let Ok(env_bytes) = std::fs::read(&env_path) else {
                    continue;
                };

                for var in env_bytes.split(|b| *b == 0) {
                    if var.is_empty() {
                        continue;
                    }
                    let Ok(var_str) = std::str::from_utf8(var) else {
                        continue;
                    };
                    for key in &keys {
                        let prefix = format!("{}=", key);
                        if var_str
                            .strip_prefix(&prefix)
                            .is_some_and(|val| val == target)
                        {
                            pids.push(pid);
                        }
                    }
                }
            }
        }
    }

    pids
}

/// Whether Steam has finished its initial load enough to launch games.
///
/// Uses the presence of Steam's `steamwebhelper` process (its Chromium-based
/// UI/service backend) as the readiness signal: it comes up after the
/// bootstrapper has updated and the client core has initialized, and it must be
/// running for the modern client to launch a game. This is a best-effort
/// heuristic — like the interstitial signatures — and works regardless of
/// whether Steam's CEF remote-debugging endpoint is enabled. Callers pair it
/// with a fallback timeout so a missed signal never hides games forever
/// (issue #76).
pub fn steam_webhelper_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        // /proc entries that aren't PIDs (e.g. "self") aren't processes.
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm"))
            && comm_is_steamwebhelper(&comm)
        {
            return true;
        }
    }
    false
}

/// Whether a `/proc/<pid>/comm` value names Steam's `steamwebhelper` process.
///
/// `comm` is the process name with a trailing newline, truncated by the kernel
/// to 15 characters. "steamwebhelper" is 14, so it fits exactly and an exact
/// match (after trimming the newline) is correct.
fn comm_is_steamwebhelper(comm: &str) -> bool {
    comm.trim() == "steamwebhelper"
}

/// Kill Steam game processes by Steam App ID
pub fn kill_steam_game_processes(app_id: u32, signal: Signal) -> bool {
    let pids = find_steam_game_pids(app_id);
    if pids.is_empty() {
        return false;
    }

    for pid in pids {
        let _ = signal::kill(Pid::from_raw(pid), signal);
    }

    true
}

/// Stop the transient systemd scope a firewalled Process-kind activity runs in.
///
/// That scope lives in the *system* manager (it needs `CAP_NET_ADMIN` to attach
/// the cgroup BPF programs behind `IPAddressDeny=`), so tearing it down means
/// going back through the privileged helper. `systemctl stop` kills every
/// process in the unit's cgroup, which reaches an activity that has escaped our
/// process group or outlived the pids we know about.
///
/// The helper's single polkit action gates the binary as a whole, so this needs
/// no grant beyond the one `apply-process` already requires.
pub fn stop_firewall_scope(scope_name: &str) -> bool {
    let output = Command::new("pkexec")
        .args([
            &firewall_helper_path(),
            "stop-scope",
            "--scope-name",
            scope_name,
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            info!(scope = scope_name, "Stopped firewall scope via helper");
            true
        }
        Ok(out) => {
            warn!(
                scope = scope_name,
                status = ?out.status.code(),
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "Helper could not stop firewall scope"
            );
            false
        }
        Err(e) => {
            warn!(scope = scope_name, error = %e, "Failed to invoke firewall helper to stop scope");
            false
        }
    }
}

/// Whether `pid` names a process that is still actually running.
///
/// A zombie counts as gone: it has exited and is only waiting to be reaped, so
/// treating it as alive would make a successful kill look like a failure.
///
/// Asks the kernel rather than consulting our own bookkeeping, because the
/// `processes` map is only pruned by the background monitor — a stop that
/// judged liveness from the map would depend on the monitor running, and would
/// hang in any context without one.
pub fn pid_is_live(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false; // no such process
    };
    // "pid (comm) state ..." — comm may contain spaces and parens, so scan
    // from the last ')' rather than splitting from the left.
    let Some(after_comm) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
        return false;
    };
    !matches!(after_comm.split_whitespace().next(), Some("Z") | None)
}

/// Whether any process in the group `pgid` is still running.
///
/// The tracked pid is only the activity's *direct* child. Plenty of activities
/// outlive it — a launcher script that execs and exits, a program that forks a
/// worker — and those descendants stay in the process group unless they call
/// `setsid`. Checking the group as well as the pid keeps a stop from declaring
/// success while the thing the child is looking at is still on screen.
pub fn pgid_is_live(pgid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid_is_live(pid) && pid_in_group(pid, pgid) {
            return true;
        }
    }
    false
}

/// Whether `pid` belongs to the process group `pgid`.
///
/// Used to decide whether a window on screen belongs to the activity we
/// launched: the surface is often owned by a descendant rather than the
/// process we spawned.
pub fn pid_in_group(pid: u32, pgid: u32) -> bool {
    nix::unistd::getpgid(Some(Pid::from_raw(pid as i32))).is_ok_and(|g| g.as_raw() as u32 == pgid)
}

/// Signal every process in the group `pgid`.
///
/// Kept separate from [`ManagedProcess::terminate`] because that requires the
/// spawned process to still be tracked. It very often is not: the moment the
/// direct child is reaped its entry is dropped, and a descendant still holding
/// the screen becomes unreachable through it.
pub fn signal_group(pgid: u32, signal: Signal) {
    match signal::kill(Pid::from_raw(-(pgid as i32)), signal) {
        Ok(()) => debug!(pgid, ?signal, "Signalled process group"),
        Err(nix::errno::Errno::ESRCH) => {}
        Err(e) => debug!(pgid, ?signal, error = %e, "Failed to signal process group"),
    }
}

/// Kill processes by command name using pkill
pub fn kill_by_command(command_name: &str, signal: Signal) -> bool {
    let signal_name = match signal {
        Signal::SIGTERM => "TERM",
        Signal::SIGKILL => "KILL",
        _ => "TERM",
    };

    // Use pkill to find and kill processes by command name
    let result = Command::new("pkill")
        .args([&format!("-{}", signal_name), "-f", command_name])
        .output();

    match result {
        Ok(output) => {
            // pkill returns 0 if processes were found and signaled
            if output.status.success() {
                info!(
                    command = command_name,
                    signal = signal_name,
                    "Killed processes by command name"
                );
                true
            } else {
                // No processes found is not an error
                debug!(
                    command = command_name,
                    "No processes found matching command name"
                );
                false
            }
        }
        Err(e) => {
            warn!(command = command_name, error = %e, "Failed to run pkill");
            false
        }
    }
}

impl ManagedProcess {
    /// Spawn a new process in its own process group
    ///
    /// If `snap_name` is provided, the process is treated as a snap app and will use
    /// systemd scope-based killing instead of signal-based killing.
    ///
    /// If `log_path` is provided, stdout and stderr will be redirected to that file.
    /// For snap apps, we use `script` to capture output from all child processes
    /// via a pseudo-terminal, since snap child processes don't inherit file descriptors.
    ///
    /// `kill_name` is the command name to `pkill -f` as a last resort. It must
    /// be the *activity's* own command, which is not always `argv[0]`: a
    /// firewalled Process entry is launched as
    /// `pkexec … shepherd-firewall-helper … systemd-run … <activity>`, and
    /// pkill'ing `pkexec` would both miss the activity and signal unrelated
    /// privileged operations. `None` falls back to `argv[0]`.
    pub fn spawn(
        argv: &[String],
        env: &HashMap<String, String>,
        cwd: Option<&std::path::PathBuf>,
        log_path: Option<PathBuf>,
        snap_name: Option<String>,
        kill_name: Option<&str>,
    ) -> HostResult<Self> {
        if argv.is_empty() {
            return Err(HostError::SpawnFailed("Empty argv".into()));
        }

        // For snap apps with log capture, wrap with `script` to capture all child output
        // via a pseudo-terminal. Snap child processes don't inherit file descriptors,
        // but they do write to the controlling terminal.
        let (actual_argv, actual_log_path) = match (&snap_name, &log_path) {
            (Some(_), Some(log_file)) => {
                // Create parent directory if it doesn't exist
                if let Some(parent) = log_file.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    warn!(path = %parent.display(), error = %e, "Failed to create log directory");
                }

                // Build command: script -q -c "original command" logfile
                // -q: quiet mode (no start/done messages)
                // -c: command to run
                let original_cmd = argv
                    .iter()
                    .map(|arg| shell_escape::escape(std::borrow::Cow::Borrowed(arg)))
                    .collect::<Vec<_>>()
                    .join(" ");

                let script_argv = vec![
                    "script".to_string(),
                    "-q".to_string(),
                    "-c".to_string(),
                    original_cmd,
                    log_file.to_string_lossy().to_string(),
                ];

                info!(log_path = %log_file.display(), "Using script to capture snap output via pty");
                (script_argv, None) // script handles the log file itself
            }
            _ => (argv.to_vec(), log_path),
        };

        let program = &actual_argv[0];
        let args = &actual_argv[1..];

        let mut cmd = Command::new(program);
        cmd.args(args);

        // Build the env map and apply it. `build_inherited_env` is shared
        // with the firewall-helper path so the activity sees the same env
        // whether or not it goes through pkexec.
        cmd.env_clear();
        let activity_env = build_inherited_env(env);
        for (k, v) in &activity_env {
            cmd.env(k, v);
        }
        if let Ok(shepherd_display) = std::env::var("SHEPHERD_WAYLAND_DISPLAY") {
            debug!(display = %shepherd_display, "Using SHEPHERD_WAYLAND_DISPLAY override for child process");
        }

        // Set working directory
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        // Configure output handling
        // If actual_log_path is provided, redirect stdout/stderr to the log file
        // (For snap apps, we already wrapped with `script` which handles logging)
        // Otherwise, inherit from parent so we can see child output for debugging
        if let Some(ref path) = actual_log_path {
            // Create parent directory if it doesn't exist
            if let Some(parent) = path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                warn!(path = %parent.display(), error = %e, "Failed to create log directory");
            }

            // Open log file for appending (create if doesn't exist)
            match File::create(path) {
                Ok(file) => {
                    // Clone file handle for stderr (both point to same file)
                    let stderr_file = match file.try_clone() {
                        Ok(f) => f,
                        Err(e) => {
                            warn!(path = %path.display(), error = %e, "Failed to clone log file handle");
                            cmd.stdout(Stdio::inherit());
                            cmd.stderr(Stdio::inherit());
                            cmd.stdin(Stdio::null());
                            // Skip to spawn
                            return Self::spawn_with_cmd(
                                cmd,
                                program,
                                kill_name.unwrap_or(program),
                                snap_name,
                            );
                        }
                    };
                    cmd.stdout(Stdio::from(file));
                    cmd.stderr(Stdio::from(stderr_file));
                    info!(path = %path.display(), "Redirecting child output to log file");
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to open log file, inheriting output");
                    cmd.stdout(Stdio::inherit());
                    cmd.stderr(Stdio::inherit());
                }
            }
        } else {
            // Inherit from parent so we can see child output for debugging
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }

        cmd.stdin(Stdio::null());

        let kill_name = kill_name.unwrap_or(program).to_string();
        Self::spawn_with_cmd(cmd, program, &kill_name, snap_name)
    }

    /// Complete the spawn process with the configured command
    fn spawn_with_cmd(
        mut cmd: Command,
        program: &str,
        kill_name: &str,
        snap_name: Option<String>,
    ) -> HostResult<Self> {
        // The name to pkill by if signals to the group don't take -- the
        // activity's own command, not necessarily the program we exec'd.
        let command_name = kill_name.to_string();

        // Set up process group - this child becomes its own process group leader
        // SAFETY: This is safe in the pre-exec context
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid().map_err(|e| std::io::Error::other(e.to_string()))?;
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| HostError::SpawnFailed(format!("Failed to spawn {}: {}", program, e)))?;

        let pid = child.id();
        let pgid = pid; // After setsid, pid == pgid

        info!(pid = pid, pgid = pgid, program = %program, snap = ?snap_name, "Process spawned");

        Ok(Self {
            child,
            pid,
            pgid,
            command_name,
            snap_name,
        })
    }

    /// Get all descendant PIDs of this process using /proc
    fn get_descendant_pids(&self) -> Vec<i32> {
        let mut descendants = Vec::new();
        let mut to_check = vec![self.pid as i32];

        while let Some(parent_pid) = to_check.pop() {
            // Read /proc to find children of this PID
            if let Ok(entries) = std::fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();

                    // Skip non-numeric entries (not PIDs)
                    if let Ok(pid) = name_str.parse::<i32>() {
                        // Read the stat file to get parent PID
                        let stat_path = format!("/proc/{}/stat", pid);
                        if let Ok(stat) = std::fs::read_to_string(&stat_path) {
                            // Format: pid (comm) state ppid ...
                            // Find the closing paren to handle comm with spaces/parens
                            if let Some(paren_end) = stat.rfind(')') {
                                let after_comm = &stat[paren_end + 2..];
                                let fields: Vec<&str> = after_comm.split_whitespace().collect();
                                if fields.len() >= 2
                                    && let Ok(ppid) = fields[1].parse::<i32>()
                                    && ppid == parent_pid
                                {
                                    descendants.push(pid);
                                    to_check.push(pid);
                                }
                            }
                        }
                    }
                }
            }
        }

        descendants
    }

    /// Send SIGTERM to all processes in this session
    pub fn terminate(&self) -> HostResult<()> {
        // For snap apps, we rely on cgroup-based killing in the adapter, not pkill
        // Using pkill with broad patterns like "snap" would kill unrelated processes
        if self.snap_name.is_none() {
            kill_by_command(&self.command_name, Signal::SIGTERM);
        }

        // Also try to kill the process group
        let pgid = Pid::from_raw(-(self.pgid as i32)); // Negative for process group

        match signal::kill(pgid, Signal::SIGTERM) {
            Ok(()) => {
                debug!(pgid = self.pgid, "Sent SIGTERM to process group");
            }
            Err(nix::errno::Errno::ESRCH) => {
                // Process group already gone
            }
            Err(e) => {
                debug!(pgid = self.pgid, error = %e, "Failed to send SIGTERM to process group");
            }
        }

        // Also kill all descendants (they may have escaped the process group)
        let descendants = self.get_descendant_pids();
        for pid in &descendants {
            let _ = signal::kill(Pid::from_raw(*pid), Signal::SIGTERM);
        }
        if !descendants.is_empty() {
            debug!(descendants = ?descendants, "Sent SIGTERM to descendant processes");
        }

        Ok(())
    }

    /// Send SIGKILL to all processes in this session
    pub fn kill(&self) -> HostResult<()> {
        // For snap apps, we rely on cgroup-based killing in the adapter, not pkill
        // Using pkill with broad patterns like "snap" would kill unrelated processes
        if self.snap_name.is_none() {
            kill_by_command(&self.command_name, Signal::SIGKILL);
        }

        // Also try to kill the process group
        let pgid = Pid::from_raw(-(self.pgid as i32));

        match signal::kill(pgid, Signal::SIGKILL) {
            Ok(()) => {
                debug!(pgid = self.pgid, "Sent SIGKILL to process group");
            }
            Err(nix::errno::Errno::ESRCH) => {
                // Process group already gone
            }
            Err(e) => {
                debug!(pgid = self.pgid, error = %e, "Failed to send SIGKILL to process group");
            }
        }

        // Also kill all descendants (they may have escaped the process group)
        let descendants = self.get_descendant_pids();
        for pid in &descendants {
            let _ = signal::kill(Pid::from_raw(*pid), Signal::SIGKILL);
        }
        if !descendants.is_empty() {
            debug!(descendants = ?descendants, "Sent SIGKILL to descendant processes");
        }

        Ok(())
    }

    /// Check if the process has exited (non-blocking)
    pub fn try_wait(&mut self) -> HostResult<Option<ExitStatus>> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let exit_status = if let Some(code) = status.code() {
                    ExitStatus::with_code(code)
                } else {
                    // Killed by signal
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        if let Some(sig) = status.signal() {
                            ExitStatus::signaled(sig)
                        } else {
                            ExitStatus::with_code(-1)
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        ExitStatus::with_code(-1)
                    }
                };
                Ok(Some(exit_status))
            }
            Ok(None) => Ok(None), // Still running
            Err(e) => Err(HostError::Internal(format!("Wait failed: {}", e))),
        }
    }

    /// Wait for the process to exit (blocking)
    pub fn wait(&mut self) -> HostResult<ExitStatus> {
        match self.child.wait() {
            Ok(status) => {
                let exit_status = if let Some(code) = status.code() {
                    ExitStatus::with_code(code)
                } else {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        if let Some(sig) = status.signal() {
                            ExitStatus::signaled(sig)
                        } else {
                            ExitStatus::with_code(-1)
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        ExitStatus::with_code(-1)
                    }
                };
                Ok(exit_status)
            }
            Err(e) => Err(HostError::Internal(format!("Wait failed: {}", e))),
        }
    }

    /// Clean up resources associated with this process
    pub fn cleanup(&self) {
        // Nothing to clean up for systemd scopes - systemd handles it
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        // Nothing special to do for systemd scopes - systemd cleans up automatically
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comm_matches_only_steamwebhelper() {
        // The real /proc/<pid>/comm carries a trailing newline.
        assert!(comm_is_steamwebhelper("steamwebhelper\n"));
        assert!(comm_is_steamwebhelper("steamwebhelper"));
        assert!(!comm_is_steamwebhelper("steam\n"));
        assert!(!comm_is_steamwebhelper("steamwebhelperx\n"));
        assert!(!comm_is_steamwebhelper(""));
    }

    #[test]
    fn steam_webhelper_running_probe_does_not_panic() {
        // Result depends on whether Steam is running on the test host; we can
        // only assert the /proc scan completes without panicking.
        let _ = steam_webhelper_running();
    }

    #[test]
    fn firewall_enforcement_status_does_not_panic() {
        // Result depends on the test runner's caps -- we can't assert
        // Supported vs. Unsupported, only that the probe runs.
        let _ = firewall_enforcement_status();
    }

    #[test]
    fn firewall_enforcement_status_is_unsupported_when_helper_missing() {
        // When the privileged helper isn't installed (the case in CI and any
        // fresh checkout), the probe must report Unsupported. This is what
        // stops the silent-no-op bug from regressing: configuring firewall
        // rules without the helper present should *visibly* fail to enforce.
        let helper = firewall_helper_path();
        if !std::path::Path::new(&helper).exists() {
            assert!(
                matches!(
                    firewall_enforcement_status(),
                    FirewallEnforcementStatus::Unsupported { .. }
                ),
                "Probe should report Unsupported when helper at {} is missing",
                helper
            );
        }
    }

    #[test]
    fn helper_argv_prefix_serializes_rules_and_env() {
        let spec = FirewallSpec {
            default_deny: true,
            allow: vec!["127.0.0.0/8".into(), "::1/128".into()],
            deny: vec!["10.0.0.0/8".into()],
        };
        let mut env = HashMap::new();
        env.insert("WAYLAND_DISPLAY".to_string(), "wayland-0".to_string());
        env.insert(
            "DBUS_SESSION_BUS_ADDRESS".to_string(),
            "unix:abc".to_string(),
        );

        let prefix = firewall_helper_argv_prefix(
            &spec,
            "shepherd-test.scope",
            1000,
            1000,
            &env,
            Some(std::path::Path::new("/tmp")),
        );

        assert_eq!(prefix[0], "pkexec");
        assert!(prefix.iter().any(|a| a == "--keep-cwd"));
        assert!(prefix.iter().any(|a| a == "apply-process"));
        assert!(prefix.iter().any(|a| a == "shepherd-test.scope"));
        // uid/gid are emitted as two args (`--uid` + `1000`), matching the
        // separated form the helper's argv parser expects.
        let uid_idx = prefix.iter().position(|a| a == "--uid").unwrap();
        assert_eq!(prefix[uid_idx + 1], "1000");
        let gid_idx = prefix.iter().position(|a| a == "--gid").unwrap();
        assert_eq!(prefix[gid_idx + 1], "1000");
        assert!(prefix.iter().any(|a| a == "--default"));
        assert!(prefix.iter().any(|a| a == "deny"));
        assert!(prefix.iter().any(|a| a == "127.0.0.0/8"));
        assert!(prefix.iter().any(|a| a == "10.0.0.0/8"));
        assert!(prefix.iter().any(|a| a == "/tmp"));
        assert!(prefix.iter().any(|a| a == "WAYLAND_DISPLAY=wayland-0"));
        assert_eq!(prefix.last().unwrap(), "--");
    }

    #[test]
    fn spawn_simple_process() {
        let argv = vec!["true".to_string()];
        let env = HashMap::new();

        let mut proc = ManagedProcess::spawn(&argv, &env, None, None, None, None).unwrap();

        // Wait for it to complete
        let status = proc.wait().unwrap();
        assert!(status.is_success());
    }

    #[test]
    fn spawn_with_args() {
        let argv = vec!["echo".to_string(), "hello".to_string()];
        let env = HashMap::new();

        let mut proc = ManagedProcess::spawn(&argv, &env, None, None, None, None).unwrap();
        let status = proc.wait().unwrap();
        assert!(status.is_success());
    }

    #[test]
    fn terminate_sleeping_process() {
        let argv = vec!["sleep".to_string(), "60".to_string()];
        let env = HashMap::new();

        let proc = ManagedProcess::spawn(&argv, &env, None, None, None, None).unwrap();

        // Give it a moment to start
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Terminate it
        proc.terminate().unwrap();

        // Wait a bit and check
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Process should be gone or terminating
    }

    /// A killed-but-unreaped child is a zombie: it has exited, so `stop` must
    /// not mistake it for an activity that survived the kill (issue #136).
    #[test]
    fn pid_is_live_treats_a_zombie_as_gone() {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();

        // Wait for it to exit without reaping it.
        for _ in 0..200 {
            if !pid_is_live(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            !pid_is_live(pid),
            "an exited-but-unreaped child must read as gone"
        );
        let _ = child.wait();
    }

    #[test]
    fn pid_is_live_sees_a_running_process_and_a_missing_one() {
        let mut child = Command::new("sh")
            .args(["-c", "exec tail -f /dev/null"])
            .spawn()
            .expect("spawn tail");
        let pid = child.id();
        assert!(pid_is_live(pid), "a running process must read as live");

        let _ = child.kill();
        let _ = child.wait();
        assert!(!pid_is_live(pid), "a reaped process must read as gone");

        // A pid that cannot exist.
        assert!(!pid_is_live(u32::MAX));
    }

    /// A launcher script that backgrounds the real activity and exits leaves
    /// its child in the same process group. Treating the script's exit as the
    /// activity's is how a session ends under a running window (issue #136).
    #[test]
    fn pgid_is_live_sees_a_survivor_after_the_group_leader_exits() {
        // setsid gives the shell its own group; the backgrounded sleep-alike
        // inherits it and outlives the shell.
        let mut leader = Command::new("setsid")
            .args(["sh", "-c", "tail -f /dev/null & exit 0"])
            .spawn()
            .expect("spawn group leader");
        let pgid = leader.id();
        let _ = leader.wait();

        assert!(!pid_is_live(pgid), "the group leader itself has exited");
        assert!(
            pgid_is_live(pgid),
            "but its group still has a member, so the activity is still running"
        );

        signal_group(pgid, Signal::SIGKILL);
        for _ in 0..200 {
            if !pgid_is_live(pgid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !pgid_is_live(pgid),
            "signalling the group must reach the survivor"
        );
    }

    /// `command_name` is the `pkill -f` fallback, so it must name the
    /// *activity* — not whatever we happened to exec.
    ///
    /// A firewalled Process entry is launched as
    /// `pkexec … shepherd-firewall-helper … systemd-run … <activity>`, so
    /// deriving it from `argv[0]` yielded `"pkexec"`: useless against the
    /// activity, and `pkill -f pkexec` would signal unrelated privileged
    /// operations on the machine (issue #136).
    #[test]
    fn kill_name_overrides_argv0_for_wrapped_launches() {
        // What the firewall path really builds: the activity is the tail of
        // a pkexec/helper/systemd-run prefix, so argv[0] is `pkexec`.
        let argv = [
            "pkexec".to_string(),
            "--keep-cwd".to_string(),
            "/usr/libexec/shepherd-firewall-helper".to_string(),
            "apply-process".to_string(),
            "--".to_string(),
            "true".to_string(),
        ];
        let mut wrapped = ManagedProcess::spawn(
            &argv[argv.len() - 1..],
            &HashMap::new(),
            None,
            None,
            None,
            Some("/opt/games/my-activity"),
        )
        .expect("spawn");
        assert_eq!(
            wrapped.command_name, "/opt/games/my-activity",
            "the caller's kill name must win over argv[0]"
        );
        let _ = wrapped.wait();

        // With no override we still fall back to argv[0].
        let mut plain = ManagedProcess::spawn(
            &["true".to_string()],
            &HashMap::new(),
            None,
            None,
            None,
            None,
        )
        .expect("spawn");
        assert_eq!(plain.command_name, "true");
        let _ = plain.wait();
    }
}
