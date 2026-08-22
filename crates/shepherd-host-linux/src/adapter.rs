//! Linux host adapter implementation

use async_trait::async_trait;
use shepherd_api::{
    EntryKind, EntryKindTag, InputCompatMode, InterstitialKind, WindowAction, WindowInfo,
    WindowOwner,
};
use shepherd_host_api::{
    ExitStatus, HostAdapter, HostCapabilities, HostError, HostEvent, HostHandlePayload, HostResult,
    HostSessionHandle, SpawnOptions, StopMode,
};
use shepherd_util::SessionId;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::process::{
    FirewallEnforcementStatus, ManagedProcess, apply_firewall_to_existing_scope,
    build_inherited_env, find_steam_game_pids, firewall_enforcement_status,
    firewall_helper_argv_prefix, init, kill_by_command, kill_flatpak_cgroup, kill_snap_cgroup,
    kill_steam_game_processes, make_scope_name, pgid_is_live, pid_in_group, pid_is_live,
    signal_group, steam_webhelper_running, stop_firewall_scope,
};
use crate::sidecar::{
    GamepadPreset, spawn_disable_touch, spawn_gamepad_bridge, spawn_tablet_bridge,
    spawn_touch_bridge, terminate_sidecar,
};
use crate::steam_interstitial::{self, DEFAULT_CEF_PORT, DismissOutcome};

/// How long to wait for the preloaded Steam client to report ready before
/// un-gating Steam activities anyway (issue #76). A safety net so a missed
/// readiness signal never leaves Steam games permanently hidden.
const STEAM_READY_FALLBACK: Duration = Duration::from_secs(120);

/// How long to keep checking that a SIGKILL actually took before declaring the
/// activity a survivor. Long enough for the kernel to reap and the monitor to
/// notice; short enough that a genuinely stuck activity is reported promptly
/// rather than leaving the launcher held indefinitely (issue #136).
const KILL_CONFIRM_WINDOW: Duration = Duration::from_millis(1500);

/// How long to keep watching for a Steam game to appear *after* its launch
/// timed out and the session was ended (issue #135). Steam honours a
/// `steam://rungameid` request on its own schedule and there is no way to
/// cancel one; on `copernicus` the game arrived 17s late. Generous enough to
/// cover a slow shader precompile, bounded so we don't kill a game the child
/// deliberately started much later.
const STEAM_ORPHAN_WATCH: Duration = Duration::from_secs(180);

/// How long to keep looking for an activity's first window before giving up.
/// Generous: a Steam cold start with a shader precompile took over two minutes
/// on `copernicus`. Giving up only means billing falls back to the whole
/// session, so erring long is the safe direction.
const WINDOW_READY_WATCH: Duration = Duration::from_secs(300);

/// How often to look for that window. Polls `list_windows` rather than
/// subscribing to sway, because the poll only runs while an activity is
/// starting and reuses a code path that is already covered by tests.
const WINDOW_READY_POLL: Duration = Duration::from_millis(500);

/// Monitor ticks (100ms each) between reconciliation sweeps for escaped
/// activities. The sweep talks to the compositor, so it is deliberately much
/// slower than the process poll it rides on.
const RECONCILE_EVERY_TICKS: u64 = 20;

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

/// Resolve the base directory under which browser policy/profile dirs are
/// materialized. Honors `SHEPHERD_BROWSER_ROOT` (used by tests to redirect
/// writes away from the real `~/.var/app/...`), otherwise the user's home.
fn resolve_browser_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SHEPHERD_BROWSER_ROOT") {
        return PathBuf::from(root);
    }
    dirs::home_dir().unwrap_or_default()
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
    /// Transient systemd scope the activity runs in, when it was launched
    /// through the privileged firewall helper. That scope lives in the
    /// *system* manager, so `systemctl stop` on it (via the helper) reaches
    /// processes our own signals may not.
    firewall_scope: Option<String>,
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
    /// Ephemeral browser profile dirs to delete when their activity exits
    /// (`wipe_on_exit`), keyed by the activity's pid. The monitor (or `stop`)
    /// removes the entry and wipes the dir exactly once.
    profile_wipes: Arc<Mutex<HashMap<u32, PathBuf>>>,
    /// Base dir for browser policy/profile materialization (the user's home in
    /// production; redirected in tests). Empty if no home could be resolved.
    browser_root: PathBuf,
    event_tx: mpsc::UnboundedSender<HostEvent>,
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<HostEvent>>>>,
    /// Steam launch interstitials we're allowed to auto-dismiss via CEF (see
    /// [`steam_interstitial`]). Empty disables the launch watchdog entirely.
    steam_auto_dismiss: Arc<Mutex<HashSet<InterstitialKind>>>,
    /// Watchdog deadline for a Steam game to appear, in milliseconds.
    steam_launch_timeout_ms: Arc<AtomicU64>,
    /// Activities that outlived every kill we know how to send. Their session
    /// is already over, so nothing else is watching them — the monitor keeps
    /// working on these rather than letting them run unsupervised (issue #136).
    escaped: Arc<Mutex<HashMap<u32, EscapedActivity>>>,
}

/// An activity that survived teardown and is still on the machine.
#[derive(Clone, Debug)]
struct EscapedActivity {
    session_id: SessionId,
    pgid: u32,
    info: Option<SessionInfo>,
    /// Kill attempts made by the reconciliation sweep so far.
    attempts: u32,
    /// Whether we have already told the daemon about this one.
    reported: bool,
}

/// Every pid shepherd is accountable for, snapshotted so a window can be
/// attributed to whatever is (or is not) supervising it.
///
/// The compositor only knows which process drew a surface. Turning that into
/// "the child's game" or "nothing we know about" needs the host's own books,
/// and it needs all of them at once — an activity's window is very often owned
/// by a descendant, and a Steam game is not in our process tree at all.
#[derive(Debug, Default)]
struct SupervisedPids {
    /// Processes we spawned for a live session, as `(pid, pgid)`.
    activities: Vec<(u32, u32)>,
    /// Steam game pids for live Steam sessions, found by app id because they
    /// are children of the Steam client rather than of anything we spawned.
    activity_steam: Vec<u32>,
    /// Per-activity input sidecars (touch bridge and friends).
    sidecars: HashSet<u32>,
    /// Shepherd's own background processes — today, the preloaded Steam
    /// client that sits on the scratchpad between launches.
    shepherd: HashSet<u32>,
    /// Activities that outlived teardown, as `(pid, pgid)`.
    escaped: Vec<(u32, u32)>,
    /// Steam game pids belonging to an escaped Steam session.
    escaped_steam: Vec<u32>,
}

impl SupervisedPids {
    /// Who a window belongs to, as far as the host can tell.
    ///
    /// Escape is checked before ordinary supervision because the two overlap:
    /// a stop that fails leaves the activity in `processes` *and* in
    /// `escaped`, and "this got away from us" is the more urgent truth.
    fn owner_of(&self, w: &WindowInfo) -> WindowOwner {
        if LinuxHost::is_infrastructure(w) {
            return WindowOwner::Shepherd;
        }
        // Nothing to match on. Reported as unowned rather than assumed
        // harmless: a surface we cannot attribute is exactly what this field
        // exists to surface.
        let Some(pid) = w.pid else {
            return WindowOwner::Unowned;
        };
        if in_any_group(pid, &self.escaped) || self.escaped_steam.contains(&pid) {
            return WindowOwner::Escaped;
        }
        if in_any_group(pid, &self.activities)
            || self.activity_steam.contains(&pid)
            || self.sidecars.contains(&pid)
        {
            return WindowOwner::Activity;
        }
        if self.shepherd.contains(&pid) {
            return WindowOwner::Shepherd;
        }
        WindowOwner::Unowned
    }

    /// Stamp [`WindowInfo::owner`] on a freshly parsed window list.
    fn attribute(&self, windows: &mut [WindowInfo]) {
        for w in windows.iter_mut() {
            w.owner = self.owner_of(w);
        }
    }
}

/// Whether `pid` is one of `group`'s pids or shares one of their process
/// groups.
fn in_any_group(pid: u32, group: &[(u32, u32)]) -> bool {
    group
        .iter()
        .any(|&(p, pgid)| p == pid || pid_in_group(pid, pgid))
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
            profile_wipes: Arc::new(Mutex::new(HashMap::new())),
            browser_root: resolve_browser_root(),
            event_tx: tx,
            event_rx: Arc::new(Mutex::new(Some(rx))),
            steam_auto_dismiss: Arc::new(Mutex::new(HashSet::new())),
            steam_launch_timeout_ms: Arc::new(AtomicU64::new(30_000)),
            escaped: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Apply `[service.steam]` config. Call before [`preload_steam`] so the CEF
    /// debug flag is created (only) when at least one interstitial is enabled.
    pub fn configure_steam(
        &self,
        auto_dismiss: HashSet<InterstitialKind>,
        launch_timeout: Duration,
    ) {
        *self.steam_auto_dismiss.lock().unwrap() = auto_dismiss;
        self.steam_launch_timeout_ms
            .store(launch_timeout.as_millis() as u64, Ordering::Relaxed);
    }

    /// Watch a Steam launch: poll for the game process, auto-dismiss any enabled
    /// interstitial via CEF, and — if no game appears before the deadline — tear
    /// the launch down and emit an error exit so the session ends (back to the
    /// launcher) instead of hanging on the spinner. The Steam window is never
    /// surfaced; a blocker we're not allowed to dismiss simply times out.
    /// Timing out does **not** cancel the launch, because nothing can:
    /// `steam://rungameid` is a request to the long-lived preloaded client, and
    /// that client will honour it on its own schedule. On `copernicus`
    /// (2026-08-20) Stray came up 17 seconds after a 60s deadline had already
    /// expired — twice — and because the watchdog had deleted its
    /// `steam_sessions` entry, nothing was tracking the game when it appeared.
    /// It ran unsupervised over the launcher until the child happened to launch
    /// the same entry again, and 110s of that play was never metered.
    ///
    /// So after the deadline the watchdog keeps watching (see
    /// [`Self::spawn_steam_orphan_watcher`]) instead of forgetting the launch.
    fn spawn_steam_launch_watchdog(
        &self,
        handle: HostSessionHandle,
        pid: u32,
        app_id: u32,
        auto_dismiss: HashSet<InterstitialKind>,
    ) {
        let steam_sessions = self.steam_sessions.clone();
        let processes = self.processes.clone();
        let sidecars = self.sidecars.clone();
        let event_tx = self.event_tx.clone();
        let timeout_ms = self.steam_launch_timeout_ms.load(Ordering::Relaxed);

        tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);

            loop {
                tokio::time::sleep(Duration::from_millis(750)).await;

                // The game launched — the regular monitor owns it now.
                if !find_steam_game_pids(app_id).is_empty() {
                    return;
                }
                // The session was stopped/removed out from under us.
                if !steam_sessions.lock().unwrap().contains_key(&pid) {
                    return;
                }

                match tokio::time::timeout(
                    Duration::from_secs(5),
                    steam_interstitial::try_dismiss_interstitials(DEFAULT_CEF_PORT, &auto_dismiss),
                )
                .await
                {
                    Ok(Ok(DismissOutcome::Dismissed(kind))) => {
                        info!(
                            app_id,
                            kind = kind.slug(),
                            "Dismissed Steam launch interstitial"
                        );
                        // Keep waiting for the game to actually come up.
                    }
                    Ok(Ok(DismissOutcome::NoModal)) => {}
                    Ok(Err(e)) => debug!(error = %e, "Steam CEF interstitial check failed"),
                    Err(_) => debug!("Steam CEF interstitial check timed out"),
                }

                if Instant::now() >= deadline {
                    // Re-check first: the interstitial probe above can burn up
                    // to 5s, so the game may have come up since the check at
                    // the top of this iteration.
                    if !find_steam_game_pids(app_id).is_empty() {
                        info!(app_id, "Steam game appeared just before the deadline");
                        return;
                    }

                    warn!(
                        app_id,
                        pid, "Steam game did not launch within timeout; ending session with error"
                    );
                    kill_steam_game_processes(app_id, nix::sys::signal::Signal::SIGKILL);
                    steam_sessions.lock().unwrap().remove(&pid);
                    processes.lock().unwrap().remove(&pid);
                    reap_sidecars(&sidecars, pid);
                    let _ = event_tx.send(HostEvent::LaunchFailed {
                        handle,
                        error: format!(
                            "Steam did not start app {app_id} within {}s",
                            timeout_ms / 1000
                        ),
                    });

                    // The session is over, but Steam's request is not: keep
                    // watching so a game that turns up later is killed rather
                    // than left running unsupervised (issue #135).
                    Self::watch_for_orphaned_steam_game(steam_sessions, app_id, pid);
                    return;
                }
            }
        });
    }

    /// After a Steam launch has been given up on, keep watching for the game to
    /// turn up anyway and kill it if it does (issue #135).
    ///
    /// The alternative — adopting a late game back into a session — would mean
    /// resurrecting a session the child has already been told ended, and would
    /// bill them for however long the launch dragged on. Killing it is the
    /// honest outcome: the launch failed, so nothing should be running. The
    /// child can simply launch again, which now works because the entry is
    /// no longer shadowed by an untracked copy of itself.
    ///
    /// Stands down the moment a *new* session for the same app id exists —
    /// otherwise the child relaunching the entry (exactly what they did on
    /// `copernicus`) would have their legitimate game killed by the previous
    /// attempt's watcher. Gives up after [`STEAM_ORPHAN_WATCH`]; past that, a
    /// game appearing is no longer plausibly this request.
    fn watch_for_orphaned_steam_game(
        steam_sessions: Arc<Mutex<HashMap<u32, SteamSession>>>,
        app_id: u32,
        session_pid: u32,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let deadline = Instant::now() + STEAM_ORPHAN_WATCH;
            loop {
                tokio::time::sleep(Duration::from_millis(750)).await;

                // Someone launched this app again: it is their game now.
                let relaunched = steam_sessions
                    .lock()
                    .unwrap()
                    .values()
                    .any(|s| s.app_id == app_id && s.pid != session_pid);
                if relaunched {
                    info!(
                        app_id,
                        "Entry was launched again; standing down the orphan watch"
                    );
                    return;
                }

                if !find_steam_game_pids(app_id).is_empty() {
                    warn!(
                        app_id,
                        "Steam launched a game for a request we already gave up on; killing it \
                         rather than leaving it unsupervised"
                    );
                    kill_steam_game_processes(app_id, nix::sys::signal::Signal::SIGKILL);
                    return;
                }

                if Instant::now() >= deadline {
                    debug!(
                        app_id,
                        watched_secs = STEAM_ORPHAN_WATCH.as_secs(),
                        "No late Steam game appeared; stopping orphan watch"
                    );
                    return;
                }
            }
        })
    }

    /// Watch the preloaded Steam client and report when it has finished its
    /// initial load, so Steam activities can be un-gated (issue #76).
    ///
    /// Emits `KindReadinessChanged { Steam, false }` immediately (Steam is not
    /// ready right after preload) and then `{ Steam, true }` once
    /// `steamwebhelper` appears — or after a generous fallback deadline, so a
    /// missed signal (or a failed preload) never hides Steam games forever.
    fn spawn_steam_readiness_watcher(&self) {
        let event_tx = self.event_tx.clone();

        // Gate Steam until the watcher confirms readiness. Harmless if the
        // engine was already seeded not-ready at startup (a no-op there).
        let _ = event_tx.send(HostEvent::KindReadinessChanged {
            kind: EntryKindTag::Steam,
            ready: false,
        });

        tokio::spawn(async move {
            let deadline = Instant::now() + STEAM_READY_FALLBACK;
            loop {
                tokio::time::sleep(Duration::from_millis(1000)).await;

                let detected = steam_webhelper_running();
                let timed_out = Instant::now() >= deadline;
                if detected {
                    info!("Steam finished initial load (steamwebhelper detected)");
                } else if timed_out {
                    warn!(
                        "Steam readiness not detected within {}s; un-gating Steam anyway",
                        STEAM_READY_FALLBACK.as_secs()
                    );
                } else {
                    continue;
                }

                let _ = event_tx.send(HostEvent::KindReadinessChanged {
                    kind: EntryKindTag::Steam,
                    ready: true,
                });
                return;
            }
        });
    }

    /// Spawn Steam in the background so it is ready when a game is launched.
    ///
    /// Steam performs several startup steps (update, auth, cloud sync) before it
    /// can run a game. By starting Steam at daemon startup, these steps complete
    /// in the background and game launches feel nearly instant.
    pub fn preload_steam(&self) {
        // Watch for Steam finishing its initial load so Steam activities stay
        // hidden until then (issue #76). Started before the spawn (and
        // unconditionally, even if the spawn below fails) so the fallback
        // deadline always un-gates eventually.
        self.spawn_steam_readiness_watcher();

        // Enable the loopback CEF debug endpoint so the launch watchdog can
        // dismiss interstitials. Only do this when at least one interstitial is
        // enabled — the endpoint is a control surface we don't open otherwise.
        // The flag is read by Steam at startup, hence before launch.
        if !self.steam_auto_dismiss.lock().unwrap().is_empty() {
            steam_interstitial::ensure_cef_debug_enabled();
        }

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
            None,
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

    /// `app_id`s that are shepherd's own furniture rather than an activity.
    /// A window matching one of these is expected to outlive every session.
    fn is_infrastructure(window: &WindowInfo) -> bool {
        const INFRA: &[&str] = &[
            "org.shepherd.launcher",
            "org.shepherd.hud",
            "org.shepherd.pairing",
            "at.yrlf.wl_mirror",
        ];
        window
            .app_id
            .as_deref()
            .is_some_and(|id| INFRA.contains(&id))
    }

    /// Report windows that belong to no activity we are tracking.
    ///
    /// This is the backstop for an orphan nobody predicted — an activity whose
    /// direct child exited while a `setsid`'d descendant kept the window, or a
    /// late Steam game arriving after we stopped watching. Neither shows up in
    /// `processes`, so only the compositor knows they exist.
    ///
    /// Deliberately **report-only** for pids shepherd did not spawn. Closing an
    /// unrecognized window is a policy call with real blast radius (a system
    /// dialog, something an admin started deliberately), and getting it wrong
    /// on a kiosk a child depends on is worse than the visibility gap. Windows
    /// belonging to a *known* escaped activity are closed — see
    /// [`Self::reconcile_escaped`].
    /// Returns the pids reported for the first time by this call, so the
    /// caller (and tests) can see what changed. `reported` carries the set
    /// across sweeps so a persistent orphan is logged once rather than every
    /// two seconds, and is pruned of pids whose windows have gone.
    fn report_unowned_windows(
        windows: &[WindowInfo],
        known: &HashSet<u32>,
        reported: &mut HashSet<u32>,
    ) -> Vec<u32> {
        let mut fresh = Vec::new();
        for w in windows {
            let Some(pid) = w.pid else { continue };
            if w.in_scratchpad || Self::is_infrastructure(w) || known.contains(&pid) {
                continue;
            }
            if reported.insert(pid) {
                fresh.push(pid);
                warn!(
                    pid,
                    app_id = ?w.app_id,
                    class = ?w.window_class,
                    name = ?w.name,
                    "Window on screen belongs to no tracked activity"
                );
            }
        }
        // Forget windows that have gone, so if one comes back it is reported
        // again rather than silently ignored forever.
        reported.retain(|pid| windows.iter().any(|w| w.pid == Some(*pid)));
        fresh
    }

    /// Snapshot everything the host is supervising, so windows can be
    /// attributed to it.
    ///
    /// Built per [`HostAdapter::list_windows`] call — i.e. only when a client
    /// is actually asking — because resolving Steam game pids means walking
    /// `/proc` and reading every process's environment. That is fine on an
    /// admin screen someone has open; it is not something to put on the
    /// monitor's reconciliation sweep, which runs every two seconds whether
    /// anyone is looking or not.
    fn supervised_pids(
        processes: &Arc<Mutex<HashMap<u32, ManagedProcess>>>,
        sidecars: &Arc<Mutex<HashMap<u32, Vec<Child>>>>,
        session_info: &Arc<Mutex<HashMap<SessionId, SessionInfo>>>,
        steam_preload_pids: &Arc<Mutex<HashSet<u32>>>,
        escaped: &Arc<Mutex<HashMap<u32, EscapedActivity>>>,
    ) -> SupervisedPids {
        let escaped_snapshot: Vec<(u32, EscapedActivity)> = escaped
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, a)| (*pid, a.clone()))
            .collect();
        let gone: HashSet<SessionId> = escaped_snapshot
            .iter()
            .map(|(_, a)| a.session_id.clone())
            .collect();

        let mut pids = SupervisedPids {
            activities: processes
                .lock()
                .unwrap()
                .values()
                .map(|p| (p.pid, p.pgid))
                .collect(),
            sidecars: sidecars
                .lock()
                .unwrap()
                .values()
                .flatten()
                .map(|c| c.id())
                .collect(),
            shepherd: steam_preload_pids.lock().unwrap().clone(),
            escaped: escaped_snapshot
                .iter()
                .map(|(pid, a)| (*pid, a.pgid))
                .collect(),
            ..SupervisedPids::default()
        };

        // A Steam game is a child of the long-lived Steam client, so neither
        // its pid nor its group is anything we spawned. Find it by app id.
        for (_, a) in &escaped_snapshot {
            if let Some(app_id) = a.info.as_ref().and_then(|i| i.steam_app_id) {
                pids.escaped_steam
                    .extend(find_steam_game_pids(app_id).into_iter().map(|p| p as u32));
            }
        }
        for (session_id, info) in session_info.lock().unwrap().iter() {
            if gone.contains(session_id) {
                continue;
            }
            if let Some(app_id) = info.steam_app_id {
                pids.activity_steam
                    .extend(find_steam_game_pids(app_id).into_iter().map(|p| p as u32));
            }
        }

        pids
    }

    /// Send every kill we have at an activity, hardest first. Shared by the
    /// stop paths and by the reconciliation sweep.
    fn kill_activity(pid: u32, pgid: u32, info: &Option<SessionInfo>) {
        use nix::sys::signal::Signal::SIGKILL;

        signal_group(pgid, SIGKILL);
        if let Some(info) = info {
            if let Some(ref snap) = info.snap_name {
                kill_snap_cgroup(snap, SIGKILL);
            } else if let Some(app_id) = info.steam_app_id {
                let _ = kill_steam_game_processes(app_id, SIGKILL);
            } else if let Some(ref app_id) = info.flatpak_app_id {
                kill_flatpak_cgroup(app_id, SIGKILL);
            } else {
                kill_by_command(&info.command_name, SIGKILL);
            }
        }
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), SIGKILL);
    }

    /// Keep working on activities that survived teardown, and close any window
    /// they still have.
    ///
    /// Reporting a survivor is not enough: its session is over, so nothing else
    /// is watching it and — as on `copernicus` — it stays on screen with no way
    /// to close it. This retries the kill, and if the activity is still holding
    /// a surface it asks the compositor to close that too, which reaches an
    /// activity whose process we cannot signal.
    async fn reconcile_escaped(
        escaped: &Arc<Mutex<HashMap<u32, EscapedActivity>>>,
        session_info: &Arc<Mutex<HashMap<SessionId, SessionInfo>>>,
        processes: &Arc<Mutex<HashMap<u32, ManagedProcess>>>,
        sidecars: &Arc<Mutex<HashMap<u32, Vec<Child>>>>,
        unowned_reported: &mut HashSet<u32>,
        event_tx: &mpsc::UnboundedSender<HostEvent>,
    ) {
        let snapshot: Vec<(u32, EscapedActivity)> = {
            let map = escaped.lock().unwrap();
            map.iter().map(|(pid, a)| (*pid, a.clone())).collect()
        };

        // Windows still on screen: both to close activities we cannot kill,
        // and to notice surfaces that belong to nothing we know about.
        let windows = crate::sway::list_windows().await.unwrap_or_default();

        let known: HashSet<u32> = {
            let mut k: HashSet<u32> = processes.lock().unwrap().keys().copied().collect();
            k.extend(sidecars.lock().unwrap().keys().copied());
            k.extend(snapshot.iter().map(|(pid, _)| *pid));
            k
        };
        let _ = Self::report_unowned_windows(&windows, &known, unowned_reported);

        if snapshot.is_empty() {
            return;
        }

        for (pid, activity) in snapshot {
            let steam = activity
                .info
                .as_ref()
                .and_then(|i| i.steam_app_id)
                .is_some();

            if !Self::activity_is_running(pid, activity.pgid, &activity.info, steam) {
                escaped.lock().unwrap().remove(&pid);
                // Only now is it safe to drop the kill recipe we were using.
                session_info.lock().unwrap().remove(&activity.session_id);
                info!(pid, session_id = %activity.session_id, "Escaped activity is finally gone");
                let _ = event_tx.send(HostEvent::ActivityEscaped {
                    session_id: activity.session_id.clone(),
                    pid,
                    command: activity
                        .info
                        .as_ref()
                        .map(|i| i.command_name.clone())
                        .unwrap_or_default(),
                    resolved: true,
                });
                continue;
            }

            if !activity.reported {
                let _ = event_tx.send(HostEvent::ActivityEscaped {
                    session_id: activity.session_id.clone(),
                    pid,
                    command: activity
                        .info
                        .as_ref()
                        .map(|i| i.command_name.clone())
                        .unwrap_or_default(),
                    resolved: false,
                });
            }

            Self::kill_activity(pid, activity.pgid, &activity.info);

            // Close any surface it is still showing. A window we can close is
            // the difference between "unsupervised activity on the child's
            // screen" and "gone from view while we keep killing it".
            for w in windows.iter().filter(|w| w.pid == Some(pid)) {
                if let Err(e) = crate::sway::act_on_window(w.id, WindowAction::Close).await {
                    debug!(pid, window = w.id, error = %e, "Could not close escaped window");
                }
            }

            if let Some(entry) = escaped.lock().unwrap().get_mut(&pid) {
                entry.attempts += 1;
                entry.reported = true;
                if entry.attempts % 10 == 1 {
                    warn!(
                        pid,
                        attempts = entry.attempts,
                        session_id = %entry.session_id,
                        "Activity is still running after its session ended; retrying"
                    );
                }
            }
        }
    }
    /// Watch for the activity's first window and report it.
    ///
    /// `HostEvent::WindowReady` was declared and handled but never actually
    /// emitted by anything, so the engine had no way to tell "the child is
    /// looking at a spinner" from "the child is playing" — and billed both the
    /// same. On `copernicus` that charged 60s of Steam shader precompile as
    /// play time (issue #135).
    ///
    /// The window is often owned by a descendant rather than the process we
    /// spawned, and for Steam by a process that is not in our tree at all, so
    /// match on the group and on the game's own pids as well as the pid.
    fn spawn_window_watch(
        &self,
        handle: HostSessionHandle,
        pid: u32,
        pgid: u32,
        steam_app_id: Option<u32>,
    ) {
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            let deadline = Instant::now() + WINDOW_READY_WATCH;
            loop {
                tokio::time::sleep(WINDOW_READY_POLL).await;

                // A non-Steam activity that is already gone will never map a
                // window. Steam's launch process exits immediately, so it has
                // only the deadline to bound it.
                if steam_app_id.is_none() && !pid_is_live(pid) && !pgid_is_live(pgid) {
                    return;
                }

                let steam_pids: Vec<i32> =
                    steam_app_id.map(find_steam_game_pids).unwrap_or_default();
                let windows = crate::sway::list_windows().await.unwrap_or_default();
                let found = windows.iter().find(|w| {
                    let Some(wpid) = w.pid else { return false };
                    if w.in_scratchpad || Self::is_infrastructure(w) {
                        return false;
                    }
                    wpid == pid || pid_in_group(wpid, pgid) || steam_pids.contains(&(wpid as i32))
                });

                if let Some(w) = found {
                    info!(
                        pid,
                        window_pid = w.pid,
                        app_id = ?w.app_id,
                        "Activity window appeared"
                    );
                    let _ = event_tx.send(HostEvent::WindowReady { handle });
                    return;
                }

                if Instant::now() >= deadline {
                    debug!(
                        pid,
                        watched_secs = WINDOW_READY_WATCH.as_secs(),
                        "No window appeared for this activity; billing the whole session"
                    );
                    return;
                }
            }
        });
    }

    /// Whether the activity behind `pid` is still alive.
    ///
    /// For Steam the process we spawned is only the `rungameid` request, which
    /// exits immediately, so liveness is the game's own pids instead.
    fn activity_is_running(
        pid: u32,
        pgid: u32,
        session_info: &Option<SessionInfo>,
        is_steam: bool,
    ) -> bool {
        if is_steam {
            return session_info
                .as_ref()
                .and_then(|info| info.steam_app_id)
                .map(|id| !find_steam_game_pids(id).is_empty())
                .unwrap_or(false);
        }
        // The group as well as the pid: a launcher script that execs and exits
        // leaves the real activity behind in the same group, and calling that
        // a clean exit is how an activity ends up unsupervised.
        pid_is_live(pid) || pgid_is_live(pgid)
    }

    /// Wait for a kill to actually take, escalating once to the activity's
    /// systemd scope if it has one.
    ///
    /// A SIGKILL is not a guarantee: the pid may have escaped the process
    /// group, and the command-name fallback may not match it. A firewalled
    /// Process-kind entry has one more lever — its transient *system* scope,
    /// whose cgroup `systemctl stop` empties regardless of what escaped where.
    ///
    /// Returns `StopFailed` if the activity outlives all of it, and registers
    /// it as escaped so the reconciliation sweep keeps working on it rather
    /// than letting it run unsupervised (issue #136).
    async fn confirm_stopped(
        &self,
        session_id: &SessionId,
        pid: u32,
        pgid: u32,
        session_info: &Option<SessionInfo>,
        is_steam: bool,
    ) -> HostResult<()> {
        let mut escalated = false;
        let mut deadline = std::time::Instant::now() + KILL_CONFIRM_WINDOW;
        loop {
            if !Self::activity_is_running(pid, pgid, session_info, is_steam) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                // One escalation left: empty the systemd scope's cgroup.
                if let Some(scope) = session_info
                    .as_ref()
                    .and_then(|i| i.firewall_scope.as_ref())
                    && !escalated
                {
                    warn!(
                        session_id = %session_id,
                        pid,
                        scope = %scope,
                        "Activity survived SIGKILL; stopping its firewall scope"
                    );
                    let scope = scope.clone();
                    let _ = tokio::task::spawn_blocking(move || stop_firewall_scope(&scope)).await;
                    escalated = true;
                    deadline = std::time::Instant::now() + KILL_CONFIRM_WINDOW;
                    continue;
                }

                warn!(session_id = %session_id, pid, "Activity still running after force kill");
                self.register_escaped(session_id, pid, pgid, session_info);
                return Err(HostError::StopFailed(format!(
                    "activity (pid {pid}) still running after force kill"
                )));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Record an activity that survived teardown, so the reconciliation sweep
    /// keeps trying. Deliberately does *not* drop `session_info`: that is the
    /// recipe the sweep needs to keep killing by cgroup/scope/command name.
    /// `reconcile_escaped` removes it once the activity is actually gone.
    ///
    /// Per-activity sidecars *are* torn down here. They are input shims, not
    /// the activity, and leaving them running strands a uinput virtual
    /// pointer+keyboard across sessions — which is what happened to Bitwig's
    /// gamepad bridge on 2026-08-20 (pid 93535 was still logging 64s after
    /// shepherdd exited).
    fn register_escaped(
        &self,
        session_id: &SessionId,
        pid: u32,
        pgid: u32,
        info: &Option<SessionInfo>,
    ) {
        reap_sidecars(&self.sidecars, pid);
        self.escaped.lock().unwrap().insert(
            pid,
            EscapedActivity {
                session_id: session_id.clone(),
                pgid,
                info: info.clone(),
                attempts: 0,
                reported: false,
            },
        );
    }

    /// Finish an activity's exit: reap its sidecars, wipe an ephemeral browser
    /// profile, and tell the engine.
    fn finish_exit(
        pid: u32,
        pgid: u32,
        status: ExitStatus,
        sidecars: &Arc<Mutex<HashMap<u32, Vec<Child>>>>,
        profile_wipes: &Arc<Mutex<HashMap<u32, PathBuf>>>,
        event_tx: &mpsc::UnboundedSender<HostEvent>,
    ) {
        info!(pid, pgid, status = ?status, "Activity exited - sending HostEvent::Exited");

        reap_sidecars(sidecars, pid);

        // Wipe an ephemeral browser profile now that the activity (and, for
        // flatpak, its Chrome instance) is fully gone.
        if let Some(dir) = profile_wipes.lock().unwrap().remove(&pid) {
            crate::browser::wipe_profile_dir(&dir);
        }

        // The session id is unknown here; the engine matches on the payload.
        let handle =
            HostSessionHandle::new(SessionId::new(), HostHandlePayload::Linux { pid, pgid });
        let _ = event_tx.send(HostEvent::Exited { handle, status });
    }

    /// Start the background process monitor
    pub fn start_monitor(&self) -> tokio::task::JoinHandle<()> {
        let processes = self.processes.clone();
        let steam_sessions = self.steam_sessions.clone();
        let steam_preload_pids = self.steam_preload_pids.clone();
        let sidecars = self.sidecars.clone();
        let profile_wipes = self.profile_wipes.clone();
        let event_tx = self.event_tx.clone();
        let escaped = self.escaped.clone();
        let session_info = self.session_info.clone();

        tokio::spawn(async move {
            let mut ticks: u64 = 0;
            let mut unowned_reported: HashSet<u32> = HashSet::new();
            // Activities whose spawned process is reaped but whose process
            // group still has members. Keyed by the spawned pid.
            let mut winding_down: HashMap<u32, (u32, ExitStatus)> = HashMap::new();
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                ticks += 1;

                // Reconciliation is a rescue path, not a hot loop: it shells
                // out to the compositor, so run it every ~2s rather than on
                // every poll. Cheap no-op when nothing has escaped.
                if ticks.is_multiple_of(RECONCILE_EVERY_TICKS) {
                    Self::reconcile_escaped(
                        &escaped,
                        &session_info,
                        &processes,
                        &sidecars,
                        &mut unowned_reported,
                        &event_tx,
                    )
                    .await;
                }

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

                    // The process we spawned is not always the activity. A
                    // launcher script that backgrounds the real program and
                    // exits is reaped here within milliseconds while the
                    // window stays on screen — and calling that "exited" ends
                    // the session under a running activity, which is the
                    // supervision escape from the other direction (issue #136).
                    // Wait for the whole process group instead.
                    if pgid_is_live(pgid) {
                        info!(
                            pid = pid,
                            pgid = pgid,
                            status = ?status,
                            "Spawned process exited but its process group is still alive; \
                             holding the session"
                        );
                        winding_down.insert(pid, (pgid, status));
                        continue;
                    }

                    Self::finish_exit(pid, pgid, status, &sidecars, &profile_wipes, &event_tx);
                }

                // Sessions whose direct child was reaped but whose group had
                // survivors: end them only once the group is genuinely empty.
                let settled: Vec<(u32, u32, ExitStatus)> = winding_down
                    .iter()
                    .filter(|(_, (pgid, _))| !pgid_is_live(*pgid))
                    .map(|(pid, (pgid, status))| (*pid, *pgid, status.clone()))
                    .collect();
                for (pid, pgid, status) in settled {
                    winding_down.remove(&pid);
                    Self::finish_exit(pid, pgid, status, &sidecars, &profile_wipes, &event_tx);
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

        // Materialize browser policy before spawning. Supported only for the
        // com.google.Chrome flatpak: write the managed-policy JSON to the app's
        // per-user config tree and rebuild the argv so Chrome is launched
        // through a shim that injects that policy into the sandbox's own /etc
        // (per-user, never the machine-wide host /etc). See `crate::browser`.
        let mut argv = argv;
        let mut pending_wipe: Option<PathBuf> = None;
        if let Some(ref browser) = options.browser {
            match entry_kind {
                EntryKind::Flatpak { app_id, args, env }
                    if crate::browser::is_supported_browser_flatpak(app_id) =>
                {
                    if self.browser_root.as_os_str().is_empty() {
                        warn!("Browser policy set but no home directory resolved; ignoring");
                    } else {
                        match crate::browser::write_policy_file(&self.browser_root, browser) {
                            Ok(policy_file) => {
                                // Per-profile user-data-dir: isolates the profile
                                // on disk and is what we wipe on exit.
                                let udd =
                                    crate::browser::user_data_dir(&self.browser_root, browser);
                                let mut flags = crate::browser::chrome_flags(browser, Some(&udd));
                                flags.extend(expand_args(args));
                                argv = crate::browser::chrome_flatpak_argv(
                                    app_id,
                                    &policy_file,
                                    env,
                                    &flags,
                                );
                                info!(policy = %policy_file.display(), "Materialized Chrome browser policy");
                                if browser.wipe_on_exit {
                                    pending_wipe = Some(udd);
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to write browser policy; launching Chrome without it")
                            }
                        }
                    }
                }
                _ => warn!(
                    "Browser policy is only supported for the {} flatpak; ignoring",
                    crate::browser::SUPPORTED_BROWSER_FLATPAK
                ),
            }
        }

        // Apply firewall: for Process kind, hand the launch to the privileged
        // helper via pkexec, which runs `systemd-run --scope` against the
        // *system* manager (the one that can attach BPF cgroup programs).
        // Snap/flatpak go through `apply_firewall_to_existing_scope` below;
        // Steam isn't supported. If the helper isn't installed or polkit
        // doesn't grant us, skip the wrapper rather than spawning under a
        // silent no-op.
        let mut firewall_scope: Option<String> = None;
        let final_argv = if let Some(ref spec) = options.firewall {
            if sandboxed_app_name.is_none() && steam_app_id.is_none() {
                match firewall_enforcement_status() {
                    FirewallEnforcementStatus::Supported => {
                        let scope_name = make_scope_name(&session_id.to_string());
                        firewall_scope = Some(scope_name.clone());
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
                InputCompatMode::TouchToMouse => match spawn_touch_bridge() {
                    Ok(child) => session_sidecars.push(child),
                    Err(e) => {
                        warn!(error = %e, "Failed to spawn touch-to-mouse bridge; continuing without it")
                    }
                },
                InputCompatMode::TabletToTouch => match spawn_tablet_bridge() {
                    Ok(child) => session_sidecars.push(child),
                    Err(e) => {
                        warn!(error = %e, "Failed to spawn tablet-to-touch bridge; continuing without it")
                    }
                },
                InputCompatMode::DisableTouch => match spawn_disable_touch() {
                    Ok(child) => session_sidecars.push(child),
                    Err(e) => {
                        warn!(error = %e, "Failed to spawn touchscreen-disable grab; continuing without it")
                    }
                },
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
            // Not `final_argv[0]`: under firewall enforcement that is `pkexec`.
            Some(&command_name),
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

        // Register the ephemeral browser profile for wiping when this pid exits.
        if let Some(dir) = pending_wipe {
            self.profile_wipes.lock().unwrap().insert(pid, dir);
        }

        // Store the session info so we can use it for killing even after process exits
        let session_info_entry = SessionInfo {
            command_name: command_name.clone(),
            snap_name: snap_name.clone(),
            flatpak_app_id: flatpak_app_id.clone(),
            steam_app_id,
            firewall_scope: firewall_scope.clone(),
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

            // Steam may block a launch behind an interstitial (cloud-sync
            // warning, "connect a controller" advisory, …) that is invisible in
            // the kiosk. When any interstitial is enabled, arm a watchdog that
            // auto-dismisses it and, failing that, ends the session with an
            // error instead of hanging forever. We never surface Steam itself.
            let auto_dismiss = self.steam_auto_dismiss.lock().unwrap().clone();
            if !auto_dismiss.is_empty() {
                self.spawn_steam_launch_watchdog(handle.clone(), pid, app_id, auto_dismiss);
            }
        }

        self.spawn_window_watch(handle.clone(), pid, pgid, steam_app_id);

        info!(pid = pid, pgid = pgid, "Spawned process");

        Ok(handle)
    }

    async fn stop(&self, handle: &HostSessionHandle, mode: StopMode) -> HostResult<()> {
        let session_id = handle.session_id.clone();
        let (pid, pgid) = match handle.payload() {
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
                    // Signal the group from the handle rather than only through
                    // `ManagedProcess`: once the spawned process is reaped its
                    // entry is gone, and with it the only path that reached a
                    // descendant still holding the screen.
                    signal_group(pgid, nix::sys::signal::Signal::SIGTERM);
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
                            signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
                            let procs = self.processes.lock().unwrap();
                            if let Some(p) = procs.get(&pid) {
                                let _ = p.kill();
                            }
                        }
                        self.confirm_stopped(&session_id, pid, pgid, &session_info, is_steam)
                            .await?;
                        break;
                    }

                    if !Self::activity_is_running(pid, pgid, &session_info, is_steam) {
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
                    signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
                    let procs = self.processes.lock().unwrap();
                    if let Some(p) = procs.get(&pid) {
                        let _ = p.kill();
                    }
                }

                // Same confirmation as the graceful path: a force stop that
                // left the activity running must say so.
                self.confirm_stopped(&session_id, pid, pgid, &session_info, is_steam)
                    .await?;
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
        let mut windows = crate::sway::list_windows().await?;
        // Attribute before handing the list out: an admin UI's whole job here
        // is to separate the child's activity from a window nothing owns, and
        // only the host knows which is which.
        Self::supervised_pids(
            &self.processes,
            &self.sidecars,
            &self.session_info,
            &self.steam_preload_pids,
            &self.escaped,
        )
        .attribute(&mut windows);
        Ok(windows)
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
            HostHandlePayload::Linux { .. } => {
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

    // -----------------------------------------------------------------------
    // Reconciliation sweep (issue #136)
    //
    // The rescue path for an activity that outlived teardown. It is the last
    // thing standing between "supervision was lost" and "supervision was lost
    // and nobody noticed", so it gets tested against real processes rather
    // than a mock.
    // -----------------------------------------------------------------------

    fn window(pid: u32, app_id: &str) -> WindowInfo {
        WindowInfo {
            id: pid as u64,
            name: Some(format!("window {pid}")),
            app_id: Some(app_id.to_string()),
            window_class: None,
            pid: Some(pid),
            workspace: Some("1".into()),
            in_scratchpad: false,
            visible: true,
            focused: false,
            owner: WindowOwner::Unowned,
        }
    }

    /// Spawn a process in its own group that will not exit on its own.
    ///
    /// `token` ends up in its command line so a `pkill -f` in the code under
    /// test matches this process and nothing else. The caller keeps the
    /// returned `Child` and reaps it once the code under test has killed it.
    fn spawn_survivor(token: &str) -> (std::process::Child, u32, u32) {
        let child = std::process::Command::new("setsid")
            .args(["sh", "-c", &format!("exec tail -f /dev/null # {token}")])
            .spawn()
            .expect("spawn survivor");
        let pid = child.id();
        // `setsid` forks when it is already a group leader, so the process
        // holding the token may be a child of the one we spawned. Wait for the
        // group to exist and use it as the identity.
        for _ in 0..200 {
            if pgid_is_live(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        (child, pid, pid)
    }

    fn escaped_events(rx: &mut mpsc::UnboundedReceiver<HostEvent>) -> Vec<(u32, bool)> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let HostEvent::ActivityEscaped { pid, resolved, .. } = ev {
                out.push((pid, resolved));
            }
        }
        out
    }

    /// The whole rescue arc: an activity that outlived teardown is reported,
    /// killed by the sweep, and then reported resolved — and only at that
    /// point is its bookkeeping dropped.
    #[tokio::test]
    async fn reconcile_kills_an_escaped_activity_and_reports_when_it_is_gone() {
        let host = LinuxHost::new();
        let mut rx = host.subscribe();
        let mut unowned = HashSet::new();

        let token = "shepherd-reconcile-test-alpha";
        let (mut survivor, pid, pgid) = spawn_survivor(token);
        let session_id = SessionId::new();
        let info = Some(SessionInfo {
            command_name: token.to_string(),
            snap_name: None,
            flatpak_app_id: None,
            steam_app_id: None,
            firewall_scope: None,
        });
        host.session_info
            .lock()
            .unwrap()
            .insert(session_id.clone(), info.clone().unwrap());
        host.register_escaped(&session_id, pid, pgid, &info);

        // First sweep: still alive, so it is announced and attacked.
        LinuxHost::reconcile_escaped(
            &host.escaped,
            &host.session_info,
            &host.processes,
            &host.sidecars,
            &mut unowned,
            &host.event_tx,
        )
        .await;

        assert_eq!(
            escaped_events(&mut rx),
            vec![(pid, false)],
            "an escape must be announced so it reaches the audit log"
        );

        // The sweep's kill lands asynchronously; give the group time to die.
        for _ in 0..200 {
            if !pgid_is_live(pgid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !pgid_is_live(pgid),
            "the sweep must actually kill the activity, not just log about it"
        );

        // Bookkeeping is still held until a sweep confirms the kill.
        assert!(host.escaped.lock().unwrap().contains_key(&pid));
        assert!(host.session_info.lock().unwrap().contains_key(&session_id));

        // Second sweep: gone, so it resolves and the bookkeeping is released.
        LinuxHost::reconcile_escaped(
            &host.escaped,
            &host.session_info,
            &host.processes,
            &host.sidecars,
            &mut unowned,
            &host.event_tx,
        )
        .await;

        assert_eq!(
            escaped_events(&mut rx),
            vec![(pid, true)],
            "resolving must be announced too, so the log shows supervision came back"
        );
        assert!(
            host.escaped.lock().unwrap().is_empty(),
            "a resolved escape must leave the registry"
        );
        assert!(
            !host.session_info.lock().unwrap().contains_key(&session_id),
            "and only then is its kill recipe dropped"
        );

        let _ = survivor.wait();
    }

    /// A survivor that is still there next time round must not be re-announced
    /// on every sweep, but must keep being attacked.
    ///
    /// Needs a target that shrugs off SIGKILL, which no child of ours can do.
    /// `kthreadd` (pid 2) can: the kernel discards signals to kernel threads
    /// outright, so this is inert even when run as root — and it is exactly
    /// the shape of the real case, an activity that is demonstrably alive and
    /// does not die when we signal it.
    #[tokio::test]
    async fn reconcile_announces_an_escape_once_but_keeps_retrying() {
        const KTHREADD: u32 = 2;
        if !pid_is_live(KTHREADD) {
            eprintln!("no kernel thread at pid 2; skipping");
            return;
        }

        let host = LinuxHost::new();
        let mut rx = host.subscribe();
        let mut unowned = HashSet::new();
        let session_id = SessionId::new();

        host.escaped.lock().unwrap().insert(
            KTHREADD,
            EscapedActivity {
                session_id: session_id.clone(),
                // Must not be a real group: `kill(-pgid)` with pgid 1 would
                // signal everything we own.
                pgid: u32::MAX / 2,
                // No SessionInfo, so no `pkill -f` runs from a unit test.
                info: None,
                attempts: 0,
                reported: false,
            },
        );

        for _ in 0..3 {
            LinuxHost::reconcile_escaped(
                &host.escaped,
                &host.session_info,
                &host.processes,
                &host.sidecars,
                &mut unowned,
                &host.event_tx,
            )
            .await;
        }

        assert_eq!(
            escaped_events(&mut rx),
            vec![(KTHREADD, false)],
            "three sweeps, one announcement — otherwise a stuck activity spams \
             the log and the audit trail every two seconds"
        );
        assert_eq!(
            host.escaped.lock().unwrap()[&KTHREADD].attempts,
            3,
            "but every sweep must try again rather than giving up"
        );
    }

    /// The attribution the admin UIs render, and the log warns on.
    ///
    /// Pure: `pid_in_group` fails closed on pids that do not exist, so the
    /// fabricated pids here only ever match by identity.
    #[test]
    fn windows_are_attributed_to_what_is_supervising_them() {
        let pids = SupervisedPids {
            activities: vec![(100, 100)],
            sidecars: [103].into_iter().collect(),
            shepherd: [104].into_iter().collect(),
            escaped: vec![(105, 105)],
            activity_steam: vec![106],
            escaped_steam: vec![107],
        };

        let owners = |w: WindowInfo| pids.owner_of(&w);

        assert_eq!(
            owners(window(100, "org.example.Game")),
            WindowOwner::Activity
        );
        assert_eq!(
            owners(window(101, "org.shepherd.launcher")),
            WindowOwner::Shepherd,
            "our own furniture is recognised by app_id, whatever its pid"
        );
        assert_eq!(
            owners(window(102, "org.example.Orphan")),
            WindowOwner::Unowned
        );
        assert_eq!(
            owners(window(103, "org.example.Bridge")),
            WindowOwner::Activity
        );
        assert_eq!(owners(window(104, "steam")), WindowOwner::Shepherd);
        assert_eq!(
            owners(window(105, "org.example.Stubborn")),
            WindowOwner::Escaped
        );
        assert_eq!(
            owners(window(106, "steam_app_504230")),
            WindowOwner::Activity
        );
        assert_eq!(
            owners(window(107, "steam_app_504230")),
            WindowOwner::Escaped
        );

        // A surface the compositor reported no pid for cannot be tied to
        // anything, and saying so is the point of the field.
        let mut anonymous = window(108, "org.example.Nameless");
        anonymous.pid = None;
        assert_eq!(pids.owner_of(&anonymous), WindowOwner::Unowned);
    }

    /// An activity that escapes stays in `processes` — the failed stop never
    /// got far enough to drop it — so the two overlap, and the escape has to
    /// win or the UI would show a loose activity as normally supervised.
    #[test]
    fn an_escaped_activity_outranks_its_stale_process_entry() {
        let pids = SupervisedPids {
            activities: vec![(200, 200)],
            escaped: vec![(200, 200)],
            ..SupervisedPids::default()
        };
        assert_eq!(
            pids.owner_of(&window(200, "org.example.Stubborn")),
            WindowOwner::Escaped
        );
    }

    #[test]
    fn unowned_windows_are_reported_once_and_forgotten_when_they_close() {
        let known: HashSet<u32> = [100].into_iter().collect();
        let mut reported = HashSet::new();

        let windows = vec![
            window(100, "org.example.TrackedActivity"), // a tracked activity
            window(101, "org.shepherd.launcher"),       // our own furniture
            window(102, "org.example.Orphan"),          // the one that matters
        ];

        assert_eq!(
            LinuxHost::report_unowned_windows(&windows, &known, &mut reported),
            vec![102],
            "only the surface belonging to nothing we know about"
        );
        assert!(
            LinuxHost::report_unowned_windows(&windows, &known, &mut reported).is_empty(),
            "a persistent orphan must not be re-announced on every sweep"
        );

        // It closes, then something takes its pid slot later: report again.
        assert!(LinuxHost::report_unowned_windows(&[], &known, &mut reported).is_empty());
        assert_eq!(
            LinuxHost::report_unowned_windows(&windows, &known, &mut reported),
            vec![102],
            "a window that comes back must be reported again"
        );
    }

    #[test]
    fn scratchpad_windows_are_not_orphans() {
        // Steam's own client is moved to the scratchpad by sway.conf rather
        // than being an activity; it is hidden, not loose on the child's
        // screen.
        let mut hidden = window(200, "steam");
        hidden.in_scratchpad = true;
        let mut reported = HashSet::new();
        assert!(
            LinuxHost::report_unowned_windows(&[hidden], &HashSet::new(), &mut reported).is_empty()
        );
    }
}
