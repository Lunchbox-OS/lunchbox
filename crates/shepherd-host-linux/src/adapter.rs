//! Linux host adapter implementation

use async_trait::async_trait;
use shepherd_api::{EntryKind, InputCompatMode, WindowAction, WindowInfo};
use shepherd_host_api::{
    ExitStatus, HostAdapter, HostCapabilities, HostError, HostEvent, HostHandlePayload, HostResult,
    HostSessionHandle, SpawnOptions, StopMode,
};
use shepherd_util::SessionId;
use std::collections::{HashMap, HashSet};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::process::{
    FirewallEnforcementStatus, ManagedProcess, apply_firewall_to_existing_scope,
    build_inherited_env, find_steam_game_pids, firewall_enforcement_status,
    firewall_helper_argv_prefix, init, kill_by_command, kill_flatpak_cgroup, kill_snap_cgroup,
    kill_steam_game_processes, make_scope_name,
};
use crate::sidecar::{GamepadPreset, spawn_gamepad_bridge, spawn_touch_bridge, terminate_sidecar};

/// Best-effort query of the compositor output scale for the touch bridge.
///
/// The touch bridge maps absolute coordinates onto the output and must divide
/// by the scale to land in logical pixels. Returns the largest active-output
/// scale (matching the XWayland HiDPI convention in `shepherdd::hidpi`), or
/// `1.0` if sway can't be queried — in which case the bridge behaves as it did
/// before scale support, i.e. correct on unscaled outputs.
///
/// Limitation: the scale is sampled once at spawn. If an output's scale
/// changes mid-session (e.g. the XWayland native-resolution workaround drops
/// it to 1.0), the mapping won't follow until the bridge is restarted.
async fn touch_output_scale() -> f64 {
    match crate::sway::get_outputs().await {
        Ok(outputs) => outputs.iter().map(|o| o.scale).fold(1.0_f64, f64::max),
        Err(e) => {
            warn!(error = %e, "Failed to query sway output scale for touch bridge; assuming 1.0");
            1.0
        }
    }
}

/// Expand `~` at the beginning of a path to the user's home directory
fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return path.replacen("~", &home.to_string_lossy(), 1);
        }
    } else if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home.to_string_lossy().into_owned();
    }
    path.to_string()
}

/// Expand tilde in all arguments
fn expand_args(args: &[String]) -> Vec<String> {
    args.iter().map(|arg| expand_tilde(arg)).collect()
}

/// Pop any sidecars registered for `pid` and terminate them on a blocking
/// thread so the async monitor isn't stalled by SIGTERM/SIGKILL waits.
fn reap_sidecars(sidecars: &Arc<Mutex<HashMap<u32, Vec<Child>>>>, pid: u32) {
    let children = sidecars.lock().unwrap().remove(&pid);
    if let Some(children) = children
        && !children.is_empty()
    {
        tokio::task::spawn_blocking(move || {
            for child in children {
                terminate_sidecar(child, "touch-bridge");
            }
        });
    }
}

/// Information tracked for each session for cleanup purposes
#[derive(Clone, Debug)]
struct SessionInfo {
    command_name: String,
    snap_name: Option<String>,
    flatpak_app_id: Option<String>,
    steam_app_id: Option<u32>,
}

#[derive(Clone, Debug)]
struct SteamSession {
    pid: u32,
    pgid: u32,
    app_id: u32,
    seen_game: bool,
}

/// Linux host adapter
pub struct LinuxHost {
    capabilities: HostCapabilities,
    processes: Arc<Mutex<HashMap<u32, ManagedProcess>>>,
    /// Track session info for killing
    session_info: Arc<Mutex<HashMap<SessionId, SessionInfo>>>,
    steam_sessions: Arc<Mutex<HashMap<u32, SteamSession>>>,
    /// PIDs of preloaded Steam launcher processes (not session-tracked)
    steam_preload_pids: Arc<Mutex<HashSet<u32>>>,
    /// Per-activity sidecar processes (touch-bridge, etc.), keyed by the
    /// activity's pid so the monitor can reap them on natural exit too.
    sidecars: Arc<Mutex<HashMap<u32, Vec<Child>>>>,
    event_tx: mpsc::UnboundedSender<HostEvent>,
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<HostEvent>>>>,
}

impl LinuxHost {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        // Initialize process management
        init();

        Self {
            capabilities: HostCapabilities::linux_full(),
            processes: Arc::new(Mutex::new(HashMap::new())),
            session_info: Arc::new(Mutex::new(HashMap::new())),
            steam_sessions: Arc::new(Mutex::new(HashMap::new())),
            steam_preload_pids: Arc::new(Mutex::new(HashSet::new())),
            sidecars: Arc::new(Mutex::new(HashMap::new())),
            event_tx: tx,
            event_rx: Arc::new(Mutex::new(Some(rx))),
        }
    }

    /// Spawn Steam in the background so it is ready when a game is launched.
    ///
    /// Steam performs several startup steps (update, auth, cloud sync) before it
    /// can run a game. By starting Steam at daemon startup, these steps complete
    /// in the background and game launches feel nearly instant.
    pub fn preload_steam(&self) {
        // -silent tells Steam not to show its main window on startup
        let argv = vec![
            "snap".to_string(),
            "run".to_string(),
            "steam".to_string(),
            "-silent".to_string(),
        ];
        match ManagedProcess::spawn(
            &argv,
            &HashMap::new(),
            None,
            None,
            Some("steam".to_string()),
        ) {
            Ok(proc) => {
                let pid = proc.pid;
                self.processes.lock().unwrap().insert(pid, proc);
                self.steam_preload_pids.lock().unwrap().insert(pid);
                info!(pid = pid, "Steam preloaded in background");
            }
            Err(e) => {
                warn!(error = %e, "Failed to preload Steam");
            }
        }
    }

    /// Kill any preloaded Steam instance. Called during graceful shutdown.
    pub fn stop_steam_preload(&self) {
        let preload_pids: Vec<u32> = self
            .steam_preload_pids
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();
        if !preload_pids.is_empty() {
            info!("Stopping preloaded Steam");
            kill_snap_cgroup("steam", nix::sys::signal::Signal::SIGKILL);
            self.steam_preload_pids.lock().unwrap().clear();
        }
    }

    /// Start the background process monitor
    pub fn start_monitor(&self) -> tokio::task::JoinHandle<()> {
        let processes = self.processes.clone();
        let steam_sessions = self.steam_sessions.clone();
        let steam_preload_pids = self.steam_preload_pids.clone();
        let sidecars = self.sidecars.clone();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;

                let mut exited = Vec::new();
                let steam_pids: HashSet<u32> =
                    { steam_sessions.lock().unwrap().keys().cloned().collect() };
                let preload_pids: HashSet<u32> = { steam_preload_pids.lock().unwrap().clone() };

                {
                    let mut procs = processes.lock().unwrap();
                    for (pid, proc) in procs.iter_mut() {
                        match proc.try_wait() {
                            Ok(Some(status)) => {
                                let is_steam = steam_pids.contains(pid);
                                exited.push((*pid, proc.pgid, status, is_steam));
                            }
                            Ok(None) => {}
                            Err(e) => {
                                warn!(pid = pid, error = %e, "Error checking process status");
                            }
                        }
                    }

                    for (pid, _, _, _) in &exited {
                        procs.remove(pid);
                    }
                }

                for (pid, pgid, status, is_steam) in exited {
                    if is_steam {
                        info!(pid = pid, pgid = pgid, status = ?status, "Steam launch process exited");
                        continue;
                    }
                    if preload_pids.contains(&pid) {
                        info!(pid = pid, "Steam preload process exited");
                        steam_preload_pids.lock().unwrap().remove(&pid);
                        continue;
                    }
                    info!(pid = pid, pgid = pgid, status = ?status, "Process exited - sending HostEvent::Exited");

                    reap_sidecars(&sidecars, pid);

                    // We don't have the session_id here, so we use a placeholder
                    // The service should track the mapping
                    let handle = HostSessionHandle::new(
                        SessionId::new(), // This will be matched by PID
                        HostHandlePayload::Linux { pid, pgid },
                    );

                    let _ = event_tx.send(HostEvent::Exited { handle, status });
                }

                // Track Steam sessions by Steam App ID instead of process exit
                let steam_snapshot: Vec<SteamSession> =
                    { steam_sessions.lock().unwrap().values().cloned().collect() };

                let mut ended = Vec::new();

                for session in &steam_snapshot {
                    let has_game = !find_steam_game_pids(session.app_id).is_empty();
                    if has_game {
                        if let Ok(mut map) = steam_sessions.lock() {
                            map.entry(session.pid)
                                .and_modify(|entry| entry.seen_game = true);
                        }
                    } else if session.seen_game {
                        ended.push((session.pid, session.pgid));
                    }
                }

                if !ended.is_empty() {
                    let ended_pids: Vec<u32> = ended.iter().map(|(pid, _)| *pid).collect();
                    {
                        let mut map = steam_sessions.lock().unwrap();
                        let mut procs = processes.lock().unwrap();
                        for pid in &ended_pids {
                            map.remove(pid);
                            procs.remove(pid);
                        }
                    }

                    for pid in &ended_pids {
                        reap_sidecars(&sidecars, *pid);
                    }

                    for (pid, pgid) in ended {
                        let handle = HostSessionHandle::new(
                            SessionId::new(),
                            HostHandlePayload::Linux { pid, pgid },
                        );
                        let _ = event_tx.send(HostEvent::Exited {
                            handle,
                            status: ExitStatus::success(),
                        });
                    }
                }
            }
        })
    }
}

impl Default for LinuxHost {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HostAdapter for LinuxHost {
    fn capabilities(&self) -> &HostCapabilities {
        &self.capabilities
    }

    async fn spawn(
        &self,
        session_id: SessionId,
        entry_kind: &EntryKind,
        options: SpawnOptions,
    ) -> HostResult<HostSessionHandle> {
        // Extract argv, env, cwd, snap_name, flatpak_app_id, and steam_app_id based on entry kind
        let (argv, env, cwd, snap_name, flatpak_app_id, steam_app_id) = match entry_kind {
            EntryKind::Process {
                command,
                args,
                env,
                cwd,
            } => {
                let mut argv = vec![expand_tilde(command)];
                argv.extend(expand_args(args));
                let expanded_cwd = cwd
                    .as_ref()
                    .map(|c| std::path::PathBuf::from(expand_tilde(&c.to_string_lossy())));
                (argv, env.clone(), expanded_cwd, None, None, None)
            }
            EntryKind::Snap {
                snap_name,
                command,
                args,
                env,
            } => {
                // For snap apps, we need to use 'snap run <snap_name>' to launch them.
                // The command (if specified) is passed as an argument after the snap name,
                // followed by any additional args.
                let mut argv = vec!["snap".to_string(), "run".to_string(), snap_name.clone()];
                // If a custom command is specified (different from snap_name), add it
                if let Some(cmd) = command
                    && cmd != snap_name
                {
                    argv.push(cmd.clone());
                }
                argv.extend(expand_args(args));
                (argv, env.clone(), None, Some(snap_name.clone()), None, None)
            }
            EntryKind::Steam { app_id, args, env } => {
                // Steam games are launched via the Steam snap: snap run steam steam://rungameid/<app_id>
                let mut argv = vec![
                    "snap".to_string(),
                    "run".to_string(),
                    "steam".to_string(),
                    format!("steam://rungameid/{}", app_id),
                ];
                argv.extend(expand_args(args));
                (argv, env.clone(), None, None, None, Some(*app_id))
            }
            EntryKind::Flatpak { app_id, args, env } => {
                // `flatpak run` strips most environment variables before
                // exec'ing the sandboxed app; user-supplied
                // `[entries.kind.env]` entries only reach the app via the
                // explicit `--env=KEY=VAL` flag. Build them into the argv
                // (sorted for deterministic ordering and easier debugging).
                let mut argv = vec!["flatpak".to_string(), "run".to_string()];
                let mut keys: Vec<&String> = env.keys().collect();
                keys.sort();
                for k in keys {
                    argv.push(format!("--env={}={}", k, env[k]));
                }
                argv.push(app_id.clone());
                argv.extend(expand_args(args));
                (argv, env.clone(), None, None, Some(app_id.clone()), None)
            }
            EntryKind::Vm { driver, args } => {
                // Construct command line from VM driver
                let mut argv = vec![driver.clone()];
                for (key, value) in args {
                    argv.push(format!("--{}", key));
                    if let Some(v) = value.as_str() {
                        argv.push(v.to_string());
                    } else {
                        argv.push(value.to_string());
                    }
                }
                (argv, HashMap::new(), None, None, None, None)
            }
            EntryKind::Media {
                library_id,
                args: _,
            } => {
                // For media, we'd typically launch a media player
                // This is a placeholder - real implementation would integrate with a player
                let argv = vec!["xdg-open".to_string(), expand_tilde(library_id)];
                (argv, HashMap::new(), None, None, None, None)
            }
            EntryKind::Custom {
                type_name: _,
                payload: _,
            } => {
                return Err(HostError::UnsupportedKind);
            }
        };

        // Get the command name for fallback killing
        // For snap/flatpak apps, use the app name (not "snap"/"flatpak") to avoid killing unrelated processes
        let command_name = if let Some(ref snap) = snap_name {
            snap.clone()
        } else if steam_app_id.is_some() {
            "steam".to_string()
        } else if let Some(ref app_id) = flatpak_app_id {
            app_id.clone()
        } else {
            argv.first().cloned().unwrap_or_default()
        };

        // Determine if this is a sandboxed app (snap or flatpak)
        let sandboxed_app_name = snap_name.clone().or_else(|| flatpak_app_id.clone());

        // Apply firewall: for Process kind, hand the launch to the privileged
        // helper via pkexec, which runs `systemd-run --scope` against the
        // *system* manager (the one that can attach BPF cgroup programs).
        // Snap/flatpak go through `apply_firewall_to_existing_scope` below;
        // Steam isn't supported. If the helper isn't installed or polkit
        // doesn't grant us, skip the wrapper rather than spawning under a
        // silent no-op.
        let final_argv = if let Some(ref spec) = options.firewall {
            if sandboxed_app_name.is_none() && steam_app_id.is_none() {
                match firewall_enforcement_status() {
                    FirewallEnforcementStatus::Supported => {
                        let scope_name = make_scope_name(&session_id.to_string());
                        let activity_env = build_inherited_env(&env);
                        let uid = nix::unistd::getuid().as_raw();
                        let gid = nix::unistd::getgid().as_raw();
                        let mut prefixed = firewall_helper_argv_prefix(
                            spec,
                            &scope_name,
                            uid,
                            gid,
                            &activity_env,
                            cwd.as_deref(),
                        );
                        prefixed.extend(argv);
                        prefixed
                    }
                    FirewallEnforcementStatus::Unsupported { reason } => {
                        warn!(
                            command = ?argv.first(),
                            reason = %reason,
                            "Firewall configured but cannot be enforced; spawning without filter"
                        );
                        argv
                    }
                }
            } else {
                argv
            }
        } else {
            argv
        };

        // Spawn any input-compat sidecars before the activity. We log
        // failures but don't propagate them — the activity should still
        // launch even if (e.g.) no touchscreen or gamepad is present.
        let mut session_sidecars: Vec<Child> = Vec::new();
        for mode in &options.input_compat {
            match mode {
                InputCompatMode::TouchToMouse => {
                    let scale = touch_output_scale().await;
                    match spawn_touch_bridge(scale) {
                        Ok(child) => session_sidecars.push(child),
                        Err(e) => {
                            warn!(error = %e, "Failed to spawn touch-to-mouse bridge; continuing without it")
                        }
                    }
                }
                InputCompatMode::GamepadProductivity | InputCompatMode::GamepadGpd => {
                    let preset =
                        GamepadPreset::from_mode(*mode).expect("gamepad mode maps to a preset");
                    match spawn_gamepad_bridge(preset, &options.input_compat_options) {
                        Ok(child) => session_sidecars.push(child),
                        Err(e) => {
                            warn!(error = %e, preset = preset.as_cli(), "Failed to spawn gamepad bridge; continuing without it")
                        }
                    }
                }
            }
        }

        let proc = ManagedProcess::spawn(
            &final_argv,
            &env,
            cwd.as_ref(),
            options.log_path.clone(),
            sandboxed_app_name,
        )
        .inspect_err(|_| {
            // Tear down any sidecars if the activity itself fails to spawn.
            for child in std::mem::take(&mut session_sidecars) {
                terminate_sidecar(child, "touch-bridge");
            }
        })?;

        // For runtime-managed scopes (snap/flatpak), apply the firewall after
        // the scope appears. Steam is not yet supported.
        if let Some(spec) = options.firewall.clone() {
            match firewall_enforcement_status() {
                FirewallEnforcementStatus::Unsupported { reason } => {
                    if snap_name.is_some() || flatpak_app_id.is_some() {
                        warn!(
                            reason = %reason,
                            "Firewall configured but cannot be enforced; not applying to runtime scope"
                        );
                    } else if steam_app_id.is_some() {
                        warn!("Firewall is not yet supported for Steam entries; ignoring");
                    }
                }
                FirewallEnforcementStatus::Supported => {
                    if let Some(ref snap) = snap_name {
                        let pattern = format!("snap.{}.{}-", snap, snap);
                        tokio::spawn(async move {
                            apply_firewall_to_existing_scope(
                                &pattern,
                                &spec,
                                Duration::from_secs(5),
                            )
                            .await;
                        });
                    } else if let Some(ref app_id) = flatpak_app_id {
                        let pattern = format!("app-flatpak-{}-", app_id);
                        tokio::spawn(async move {
                            apply_firewall_to_existing_scope(
                                &pattern,
                                &spec,
                                Duration::from_secs(5),
                            )
                            .await;
                        });
                    } else if steam_app_id.is_some() {
                        warn!("Firewall is not yet supported for Steam entries; ignoring");
                    }
                }
            }
        }

        let pid = proc.pid;
        let pgid = proc.pgid;

        if !session_sidecars.is_empty() {
            self.sidecars.lock().unwrap().insert(pid, session_sidecars);
        }

        // Store the session info so we can use it for killing even after process exits
        let session_info_entry = SessionInfo {
            command_name: command_name.clone(),
            snap_name: snap_name.clone(),
            flatpak_app_id: flatpak_app_id.clone(),
            steam_app_id,
        };
        self.session_info
            .lock()
            .unwrap()
            .insert(session_id.clone(), session_info_entry);
        info!(session_id = %session_id, command = %command_name, snap = ?snap_name, flatpak = ?flatpak_app_id, "Tracking session info");

        let handle = HostSessionHandle::new(session_id, HostHandlePayload::Linux { pid, pgid });

        self.processes.lock().unwrap().insert(pid, proc);

        if let Some(app_id) = steam_app_id {
            self.steam_sessions.lock().unwrap().insert(
                pid,
                SteamSession {
                    pid,
                    pgid,
                    app_id,
                    seen_game: false,
                },
            );
        }

        info!(pid = pid, pgid = pgid, "Spawned process");

        Ok(handle)
    }

    async fn stop(&self, handle: &HostSessionHandle, mode: StopMode) -> HostResult<()> {
        let session_id = handle.session_id.clone();
        let (pid, _pgid) = match handle.payload() {
            HostHandlePayload::Linux { pid, pgid } => (*pid, *pgid),
            _ => return Err(HostError::SessionNotFound),
        };

        // Get the session's info for killing
        let session_info = self.session_info.lock().unwrap().get(&session_id).cloned();

        // Check if we have session info OR a tracked process
        let has_process = self.processes.lock().unwrap().contains_key(&pid);

        if session_info.is_none() && !has_process {
            warn!(session_id = %session_id, pid = pid, "No session info or tracked process found");
            return Err(HostError::SessionNotFound);
        }

        match mode {
            StopMode::Graceful { timeout } => {
                // If this is a snap or flatpak app, use cgroup-based killing (most reliable)
                if let Some(ref info) = session_info {
                    if let Some(ref snap) = info.snap_name {
                        kill_snap_cgroup(snap, nix::sys::signal::Signal::SIGTERM);
                        info!(snap = %snap, "Sent SIGTERM via snap cgroup");
                    } else if let Some(app_id) = info.steam_app_id {
                        let _ =
                            kill_steam_game_processes(app_id, nix::sys::signal::Signal::SIGTERM);
                        if let Ok(mut map) = self.steam_sessions.lock() {
                            map.entry(pid).and_modify(|entry| entry.seen_game = true);
                        }
                        info!(
                            steam_app_id = app_id,
                            "Sent SIGTERM to Steam game processes"
                        );
                    } else if let Some(ref app_id) = info.flatpak_app_id {
                        kill_flatpak_cgroup(app_id, nix::sys::signal::Signal::SIGTERM);
                        info!(flatpak = %app_id, "Sent SIGTERM via flatpak cgroup");
                    } else {
                        // Fall back to command name for non-sandboxed apps
                        kill_by_command(&info.command_name, nix::sys::signal::Signal::SIGTERM);
                        info!(command = %info.command_name, "Sent SIGTERM via command name");
                    }
                }

                // Also send SIGTERM via process handle (skip for Steam sessions)
                let is_steam = session_info
                    .as_ref()
                    .and_then(|info| info.steam_app_id)
                    .is_some();
                if !is_steam {
                    let procs = self.processes.lock().unwrap();
                    if let Some(p) = procs.get(&pid) {
                        let _ = p.terminate();
                    }
                }

                // Wait for graceful exit
                let start = std::time::Instant::now();
                loop {
                    if start.elapsed() >= timeout {
                        // Force kill after timeout using snap/flatpak cgroup or command name
                        if let Some(ref info) = session_info {
                            if let Some(ref snap) = info.snap_name {
                                kill_snap_cgroup(snap, nix::sys::signal::Signal::SIGKILL);
                                info!(snap = %snap, "Sent SIGKILL via snap cgroup (timeout)");
                            } else if let Some(app_id) = info.steam_app_id {
                                let _ = kill_steam_game_processes(
                                    app_id,
                                    nix::sys::signal::Signal::SIGKILL,
                                );
                                info!(
                                    steam_app_id = app_id,
                                    "Sent SIGKILL to Steam game processes (timeout)"
                                );
                            } else if let Some(ref app_id) = info.flatpak_app_id {
                                kill_flatpak_cgroup(app_id, nix::sys::signal::Signal::SIGKILL);
                                info!(flatpak = %app_id, "Sent SIGKILL via flatpak cgroup (timeout)");
                            } else {
                                kill_by_command(
                                    &info.command_name,
                                    nix::sys::signal::Signal::SIGKILL,
                                );
                                info!(command = %info.command_name, "Sent SIGKILL via command name (timeout)");
                            }
                        }

                        // Also force kill via process handle (skip for Steam sessions)
                        if !is_steam {
                            let procs = self.processes.lock().unwrap();
                            if let Some(p) = procs.get(&pid) {
                                let _ = p.kill();
                            }
                        }
                        break;
                    }

                    // Check if process is still running
                    let still_running = if is_steam {
                        let app_id = session_info.as_ref().and_then(|info| info.steam_app_id);
                        app_id
                            .map(|id| !find_steam_game_pids(id).is_empty())
                            .unwrap_or(false)
                    } else {
                        self.processes.lock().unwrap().contains_key(&pid)
                    };

                    if !still_running {
                        break;
                    }

                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            StopMode::Force => {
                // Force kill via snap/flatpak cgroup or command name
                if let Some(ref info) = session_info {
                    if let Some(ref snap) = info.snap_name {
                        kill_snap_cgroup(snap, nix::sys::signal::Signal::SIGKILL);
                        info!(snap = %snap, "Sent SIGKILL via snap cgroup");
                    } else if let Some(app_id) = info.steam_app_id {
                        let _ =
                            kill_steam_game_processes(app_id, nix::sys::signal::Signal::SIGKILL);
                        if let Ok(mut map) = self.steam_sessions.lock() {
                            map.entry(pid).and_modify(|entry| entry.seen_game = true);
                        }
                        info!(
                            steam_app_id = app_id,
                            "Sent SIGKILL to Steam game processes"
                        );
                    } else if let Some(ref app_id) = info.flatpak_app_id {
                        kill_flatpak_cgroup(app_id, nix::sys::signal::Signal::SIGKILL);
                        info!(flatpak = %app_id, "Sent SIGKILL via flatpak cgroup");
                    } else {
                        kill_by_command(&info.command_name, nix::sys::signal::Signal::SIGKILL);
                        info!(command = %info.command_name, "Sent SIGKILL via command name");
                    }
                }

                // Also force kill via process handle (skip for Steam sessions)
                let is_steam = session_info
                    .as_ref()
                    .and_then(|info| info.steam_app_id)
                    .is_some();
                if !is_steam {
                    let procs = self.processes.lock().unwrap();
                    if let Some(p) = procs.get(&pid) {
                        let _ = p.kill();
                    }
                }
            }
        }

        // Tear down any per-activity sidecars (e.g., touch-to-mouse bridge).
        let sidecars = self.sidecars.lock().unwrap().remove(&pid);
        if let Some(children) = sidecars {
            for child in children {
                terminate_sidecar(child, "touch-bridge");
            }
        }

        // Clean up the session info tracking
        self.session_info.lock().unwrap().remove(&session_id);

        Ok(())
    }

    async fn logout(&self) -> HostResult<()> {
        match tokio::process::Command::new("swaymsg")
            .arg("exit")
            .status()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(HostError::Internal(format!("swaymsg exit failed: {e}"))),
        }
    }

    async fn list_windows(&self) -> HostResult<Vec<WindowInfo>> {
        crate::sway::list_windows().await
    }

    async fn act_on_window(&self, window_id: u64, action: WindowAction) -> HostResult<()> {
        crate::sway::act_on_window(window_id, action).await
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<HostEvent> {
        self.event_rx
            .lock()
            .unwrap()
            .take()
            .expect("subscribe() can only be called once")
    }

    fn is_healthy(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_exit() {
        let host = LinuxHost::new();
        let _rx = host.subscribe();

        let session_id = SessionId::new();
        let entry = EntryKind::Process {
            command: "true".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
        };

        let handle = host
            .spawn(session_id, &entry, SpawnOptions::default())
            .await
            .unwrap();

        // Give it time to exit
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Process should have exited
        match handle.payload() {
            HostHandlePayload::Linux { pid: _, .. } => {
                let _procs = host.processes.lock().unwrap();
                // Process may or may not still be tracked depending on monitor timing
            }
            _ => panic!("Expected Linux handle"),
        }
    }

    #[tokio::test]
    async fn test_spawn_and_kill() {
        let host = LinuxHost::new();
        let _rx = host.subscribe();

        let session_id = SessionId::new();
        let entry = EntryKind::Process {
            command: "sleep".into(),
            args: vec!["60".into()],
            env: HashMap::new(),
            cwd: None,
        };

        let handle = host
            .spawn(session_id, &entry, SpawnOptions::default())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Kill it
        host.stop(
            &handle,
            StopMode::Graceful {
                timeout: Duration::from_secs(1),
            },
        )
        .await
        .unwrap();
    }
}
