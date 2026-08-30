//! shepherdd - The shepherd background service
//!
//! This is the main entry point for the shepherdd service.
//! It wires together all the components:
//! - Configuration loading
//! - Store initialization
//! - Core engine
//! - Host adapter (Linux)
//! - IPC server
//! - Volume control

use anyhow::{Context, Result};
use clap::Parser;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use shepherd_api::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSink, DiagnosticSubject, EntryKind,
    EntryKindTag, ErrorCode, ErrorInfo, Event, EventPayload, Response,
};
use shepherd_ble::{BleServer, BleServerConfig};
use shepherd_config::load_config;
use shepherd_core::{CoreEngine, CoreEvent};
use shepherd_host_api::{
    BrightnessController, DisplayController, HidpiController, HostAdapter, HostEvent, LightSensor,
    NoOpDisplayController, StopMode as HostStopMode, VolumeController,
};
use shepherd_host_linux::{
    LinuxBrightnessController, LinuxHost, LinuxLightSensor, LinuxVolumeController,
    PipeWireAudioRouter, SwayIpcBackend,
};
use shepherd_http::{AppState as HttpAppState, HttpServer};
use shepherd_ipc::{IpcServer, ServerMessage};
use shepherd_management::{
    AUTO_BRIGHTNESS_SETTING_KEY, AutoBrightnessState, DefaultManagementService, ManagementService,
};
use shepherd_store::{AuditEvent, AuditEventType, SqliteStore, Store};
use shepherd_util::{MonotonicInstant, RateLimiter, default_config_path};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, broadcast, mpsc};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

/// How often to recompute the probed diagnostics (issue #143). Hourly: these
/// are conditions somebody has to go and fix rather than fast-moving state, and
/// the firewall probe execs a subprocess. A config reload sweeps too, so an
/// admin editing the file does not wait for the timer.
const DIAGNOSTIC_SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// How often to check that the management socket is still the one we bound
/// (issue #144). A minute: this is a deliberate act, not a hot path.
const SOCKET_WATCH_INTERVAL: Duration = Duration::from_secs(60);

mod diagnostics;
mod display;
mod display_watch;
mod hidpi;
mod input_devices;
mod internet;
mod media;
mod pairing_display;
mod system_events;

use display::{DisplayManager, WlMirrorLauncher};
use hidpi::XwaylandHidpi;

/// How often to re-read the PipeWire audio topology (issue #124).
///
/// Fast enough that plugging in headphones updates the HUD before the user
/// reaches for the volume slider, slow enough that an idle machine spends
/// nothing noticeable on it — a `pw-dump` costs a few milliseconds and the tick
/// broadcasts only when something actually changed.
const AUDIO_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// shepherdd - Policy enforcement service for child-focused computing
#[derive(Parser, Debug)]
#[command(name = "shepherdd")]
#[command(about = "Policy enforcement service for child-focused computing", long_about = None)]
struct Args {
    /// Configuration file path (default: ~/.config/shepherd/config.toml)
    #[arg(short, long, default_value_os_t = default_config_path())]
    config: PathBuf,

    /// Socket path override (or set SHEPHERD_SOCKET env var)
    #[arg(short, long, env = "SHEPHERD_SOCKET")]
    socket: Option<PathBuf>,

    /// Data directory override (or set SHEPHERD_DATA_DIR env var)
    #[arg(short, long, env = "SHEPHERD_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Give sway's IPC socket a second name at this path before hardening
    /// removes the first (or set SHEPHERD_SWAY_IPC_ALIAS).
    ///
    /// Must be on the same filesystem as the socket — i.e. inside
    /// `$XDG_RUNTIME_DIR` — because the alias is a hard link. Without this,
    /// hardening leaves nothing able to reach the compositor except shepherdd
    /// itself, which is the point in production and unusable in dev.
    #[arg(long, env = "SHEPHERD_SWAY_IPC_ALIAS")]
    sway_ipc_alias: Option<PathBuf>,

    /// Leave sway's IPC socket reachable by every process at this uid, instead
    /// of unlinking it once shepherdd has connected (or set
    /// SHEPHERD_NO_HARDEN_SWAY_IPC).
    ///
    /// Hardening is the default because sway's IPC hands any process running as
    /// this uid `exec`, which starts a process outside shepherd's supervision
    /// *and* outside the cgroup the per-entry firewall is attached to
    /// (issue #144) — a device that ships unhardened is a device where
    /// `default_deny` means nothing.
    ///
    /// The escape hatch exists because the unlink is destructive to whatever
    /// sway session shepherdd happens to be inside: run by hand in a
    /// developer's own desktop, it would take that desktop's socket away from
    /// every other client. Every development entry point in this repo passes
    /// it — `sway.conf`, the headless harness, and the e2e stack — so the flag
    /// is what a dev session opts *out* with, not what a device opts in with.
    #[arg(long = "no-harden-sway-ipc", env = "SHEPHERD_NO_HARDEN_SWAY_IPC")]
    no_harden_sway_ipc: bool,

    /// Accept a client on shepherdd's own management socket from any process
    /// at this uid, instead of only from the session shepherdd is part of (or
    /// set SHEPHERD_NO_RESTRICT_IPC_PEERS).
    ///
    /// Restricting is the default because every activity runs as this uid, so
    /// the socket's file permissions separate nothing: without the check, a
    /// game can call `logout`, `stop_current` or `launch` (issue #144).
    /// Accepted peers are those in shepherdd's own cgroup — the launcher, the
    /// HUD and the compositor's one-shot keybinding clients — plus root, so
    /// `sudo` still reaches the daemon from an operator's own shell.
    ///
    /// The escape hatch exists because the check only means something where
    /// shepherdd's cgroup is one an activity cannot join, which is true of a
    /// device's display-manager session and false of a stack started from a
    /// shell. In dev the whole stack shares the launching terminal's cgroup,
    /// so a client run from any *other* terminal would be refused. Every
    /// development entry point in this repo passes this — `sway.conf`, the
    /// headless harness, `run-dev` and the e2e stack — so the flag is what a
    /// dev session opts *out* with, not what a device opts in with.
    #[arg(long = "no-restrict-ipc-peers", env = "SHEPHERD_NO_RESTRICT_IPC_PEERS")]
    no_restrict_ipc_peers: bool,
}

/// Main service state
struct Service {
    config_path: PathBuf,
    engine: CoreEngine,
    host: Arc<LinuxHost>,
    volume: Arc<LinuxVolumeController>,
    brightness: Arc<LinuxBrightnessController>,
    light_sensor: Arc<LinuxLightSensor>,
    ipc: Arc<IpcServer>,
    store: Arc<dyn Store>,
    rate_limiter: RateLimiter,
    internet_monitor: Option<internet::InternetMonitor>,
    input_monitor: Option<input_devices::InputMonitor>,
    media_prefetcher: media::MediaPrefetcher,
    /// What is currently wrong with this device, for an administrator (issue
    /// #143). Swept periodically and on config reload.
    diagnostics: Arc<diagnostics::DiagnosticRegistry>,
    /// Where to give sway's IPC socket a second name, if anywhere.
    sway_ipc_alias: Option<PathBuf>,
    /// Whether to take sway's IPC socket away from everything else once we
    /// have connected (issue #144).
    harden_sway_ipc: bool,
    /// How the peer allow-list on our own socket ended up (issue #144).
    /// Carried so `run()` can report a downgrade once the diagnostics channel
    /// exists — the IPC server is started well before it.
    ipc_peer_hardening: IpcPeerHardening,
}

/// What arming the management socket's peer allow-list actually achieved.
///
/// Three outcomes rather than a bool, because "the operator turned it off" and
/// "it is on but cannot be a boundary here" look identical from the socket and
/// mean opposite things to whoever is responsible for the device.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IpcPeerHardening {
    /// Armed, and in a cgroup an activity cannot join.
    Enforced,
    /// Deliberately off (`--no-restrict-ipc-peers`).
    OptedOut,
    /// Armed, but shepherdd is somewhere the check cannot hold, or could not
    /// be armed at all. Carries the reason for the diagnostic.
    Degraded(String),
}

impl Service {
    async fn new(args: &Args) -> Result<Self> {
        // Load configuration
        let policy = load_config(&args.config)
            .with_context(|| format!("Failed to load config from {:?}", args.config))?;

        info!(
            config_path = %args.config.display(),
            entry_count = policy.entries.len(),
            "Configuration loaded"
        );

        // Determine paths
        let socket_path = args
            .socket
            .clone()
            .unwrap_or_else(|| policy.service.socket_path.clone());

        let data_dir = args
            .data_dir
            .clone()
            .unwrap_or_else(|| policy.service.data_dir.clone());

        // Create data directory
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data directory {:?}", data_dir))?;

        // Initialize store
        let db_path = data_dir.join("shepherdd.db");
        let store: Arc<dyn Store> = Arc::new(
            SqliteStore::open(&db_path)
                .with_context(|| format!("Failed to open database {:?}", db_path))?,
        );

        info!(db_path = %db_path.display(), "Store initialized");

        // Log service start
        store.append_audit(AuditEvent::new(AuditEventType::ServiceStarted))?;

        // Decide whether the environment may name binaries, before anything is
        // resolved or spawned (issue #144). On a device it may not: GDM's PAM
        // stack reads `~/.pam_environment`, so the kiosk user — and therefore
        // every activity — chooses the session's environment. The same flag
        // governs this and the peer allow-list because they mean the same
        // thing: whether this stack is a device or a developer's.
        shepherd_host_linux::helpers::set_trust_environment(args.no_restrict_ipc_peers);

        // Initialize host adapter
        let host = Arc::new(LinuxHost::new());

        // Give `yt-dlp` a cgroup of its own, like an activity (issue #144). The
        // media cache cannot build this wrapper itself — it is shared with the
        // player and the Android build, neither of which has a user manager —
        // so the daemon injects the one from the Linux host here, once, before
        // any prefetch can start.
        shepherd_media_cache::set_scope_prefix_fn(shepherd_host_linux::helper_scope_argv_prefix);
        // ...and resolve it from a trusted directory rather than `$PATH`, for
        // the same reason (issue #144). The scope contains a hijacked yt-dlp,
        // but `ytdlp_available`'s `--version` probe runs unscoped, so the
        // lookup has to be safe on its own.
        shepherd_media_cache::set_program_resolver_fn(shepherd_host_linux::resolve_helper_arg);

        // Initialize volume controller
        let volume = Arc::new(LinuxVolumeController::new());
        if volume.capabilities().available {
            info!(
                backend = ?volume.capabilities().backend,
                "Volume controller initialized"
            );
        } else {
            warn!("No sound backend detected, volume control unavailable");
        }

        // Initialize brightness controller. Logged at debug level on hosts
        // without a backlight (most desktops) so it doesn't spam warnings;
        // the controller itself already logs an info line when one is found.
        let brightness = Arc::new(LinuxBrightnessController::new());
        if !brightness.capabilities().available {
            debug!("No backlight detected, brightness control unavailable");
        }

        // Initialize ambient light sensor (for automatic brightness). Absent
        // on most hardware; the controller logs an info line when one is
        // found and stays quiet otherwise.
        let light_sensor = Arc::new(LinuxLightSensor::new());

        // Initialize core engine
        let engine = CoreEngine::new(policy, store.clone(), host.capabilities().clone());

        // Apply Steam config to the host before any preload so the CEF debug
        // flag is created (only) when interstitial auto-dismiss is enabled.
        host.configure_steam(
            engine.policy().service.steam.auto_dismiss.clone(),
            engine.policy().service.steam.launch_timeout,
        );

        // Initialize internet connectivity monitor (if configured)
        let internet_monitor = internet::InternetMonitor::from_policy(engine.policy());

        // Initialize input-device dependency monitor (issue #96). Only runs when
        // some entry declares `requires_input`.
        let input_monitor = input_devices::InputMonitor::from_policy(engine.policy());
        // Background media prefetch (issue #127). Constructed unconditionally,
        // including with no media entries configured: it re-reads policy each
        // sweep, so a reload that adds a media entry has something to reach.
        // Also where a missing yt-dlp is reported, since this is the only place
        // that knows both what the policy references and what is installed.
        let media_prefetcher = media::MediaPrefetcher::from_policy(engine.policy());

        // Initialize IPC server
        let mut ipc = IpcServer::new(&socket_path);
        let ipc_peer_hardening = Self::arm_ipc_peer_policy(&mut ipc, !args.no_restrict_ipc_peers);
        ipc.start().await?;

        info!(socket_path = %socket_path.display(), "IPC server started");

        // Rate limiter: 30 requests per second per client
        let rate_limiter = RateLimiter::new(30, Duration::from_secs(1));

        Ok(Self {
            config_path: args.config.clone(),
            engine,
            host,
            volume,
            brightness,
            light_sensor,
            ipc: Arc::new(ipc),
            store,
            rate_limiter,
            internet_monitor,
            input_monitor,
            media_prefetcher,
            diagnostics: Arc::new(diagnostics::DiagnosticRegistry::new()),
            sway_ipc_alias: args.sway_ipc_alias.clone(),
            harden_sway_ipc: !args.no_harden_sway_ipc,
            ipc_peer_hardening,
        })
    }

    /// Take sway's IPC socket away from everything except this daemon.
    ///
    /// Sway's IPC grants any process running as shepherdd's own uid — which is
    /// every activity — the whole compositor: `exec` starts a process outside
    /// shepherd's supervision *and* outside the cgroup the per-entry firewall
    /// is attached to, `exit` ends the kiosk session, and `kill` closes the HUD.
    /// Sway has no access control to turn on (its `ipc` permission blocks went
    /// away in 1.0), and no permission or path scheme can help while everything
    /// shares a uid: `/proc/net/unix` lists every bound socket path, and
    /// `/proc/<pid>/environ` is readable at the same uid.
    ///
    /// What does work is removing the name. Sway keeps its listening socket
    /// open and connections already established keep working, but nothing can
    /// connect by path afterwards. That is only possible because the adapter
    /// holds a persistent connection now (issue #147) — while every call
    /// re-exec'd `swaymsg`, the path had to exist forever.
    ///
    /// Order matters and is load-bearing: connect, then alias, then unlink. An
    /// alias that was asked for and could not be made aborts the unlink, because
    /// a session that is still drivable beats one that is hardened and inert.
    ///
    /// `harden` is true unless `--no-harden-sway-ipc` was passed, so this runs
    /// on any stack that did not explicitly ask to stay reachable.
    ///
    /// Every failure leaves the session running and the socket reachable. That
    /// is the right trade — an unhardened kiosk beats a child staring at a dead
    /// screen — but it means nothing else about the device looks wrong, so a
    /// failure has to be said out loud or it ships as a silent downgrade
    /// (`CompositorNotHardened`, issue #144).
    async fn harden_compositor_socket(
        alias: Option<&Path>,
        harden: bool,
        diagnostics: &dyn DiagnosticSink,
    ) {
        if !harden && alias.is_none() {
            return;
        }

        // Only a stack that asked to be hardened can be *un*-hardened. With
        // `--sway-ipc-alias` alone there is nothing to fail to do, so the
        // failures below are logged and not reported.
        let report = |reason: String| {
            if harden {
                diagnostics.raise(Self::not_hardened_diagnostic(&reason));
            }
        };

        // The subscriptions are already up, but the request connection is lazy.
        // Unlinking before it exists would leave it permanently unable to
        // connect.
        if let Err(e) = shepherd_host_linux::sway_ipc::client().connect_now().await {
            warn!(error = %e, "Not hardening the sway IPC socket: no connection to keep alive");
            report(format!("shepherd could not reach the compositor: {e}"));
            return;
        }

        if let Some(alias) = alias {
            match shepherd_host_linux::sway_ipc::alias_socket(alias) {
                Ok(()) => info!(alias = %alias.display(), "Sway IPC socket aliased"),
                Err(e) => {
                    warn!(error = %e, alias = %alias.display(),
                        "Could not alias the sway IPC socket; leaving it reachable rather than \
                         stranding the session");
                    report(format!(
                        "the socket alias at {} could not be made: {e}",
                        alias.display()
                    ));
                    return;
                }
            }
        }

        if !harden {
            return;
        }

        match shepherd_host_linux::sway_ipc::unlink_socket() {
            Ok(()) => info!(
                "Sway IPC socket unlinked; the compositor is no longer reachable by anything else"
            ),
            Err(e) => {
                warn!(error = %e, "Could not unlink the sway IPC socket");
                report(format!("the socket's name could not be removed: {e}"));
            }
        }
    }

    /// Arm the peer allow-list on shepherdd's own management socket (#144).
    ///
    /// Every activity runs as shepherdd's uid, so the socket's mode separates
    /// nothing: the check that does is the peer's cgroup, which the kernel
    /// maintains, which every descendant inherits, and which an unprivileged
    /// process can neither forge nor leave.
    ///
    /// Returns what was actually achieved rather than reporting it here,
    /// because the IPC server is built long before the diagnostics channel
    /// exists — the ordering constraint that already bit once on this branch.
    fn arm_ipc_peer_policy(ipc: &mut IpcServer, restrict: bool) -> IpcPeerHardening {
        if !restrict {
            info!(
                "Management socket peer restriction is off; every process at this uid can \
                 drive the daemon"
            );
            return IpcPeerHardening::OptedOut;
        }

        let policy = match shepherd_ipc::PeerPolicy::restricted() {
            Ok(policy) => policy,
            Err(e) => {
                // A policy that cannot name what it accepts would refuse every
                // client, including the launcher. Staying open is the same
                // trade the compositor hardening makes: an unhardened kiosk
                // beats a dead one, as long as it is said out loud.
                warn!(error = %e, "Could not read shepherd's own cgroup; leaving the management socket open to this uid");
                return IpcPeerHardening::Degraded(format!(
                    "shepherd could not read its own cgroup, so it cannot tell its own \
                     clients from an activity: {e}"
                ));
            }
        };
        ipc.set_peer_policy(policy);

        // Armed — but only a boundary where shepherd's cgroup is one an
        // activity cannot get into. Inside the user manager's delegated
        // subtree every cgroup is owned by this uid, so any process at this
        // uid can move itself into any other: the allow-list still refuses a
        // peer that has not bothered, and stops being a boundary against one
        // that has. A device's session (started by the display manager, in a
        // root-owned logind scope) is outside it; a stack started from a shell
        // or as a `systemd --user` unit is inside it.
        match shepherd_ipc::own_cgroup_path() {
            Ok(path) if shepherd_ipc::is_delegated_user_cgroup(&path) => {
                warn!(
                    cgroup = %path,
                    "shepherd is running inside the user manager's delegated cgroup subtree, \
                     where a process at this uid can join any cgroup; the management socket's \
                     peer check is not a boundary here"
                );
                IpcPeerHardening::Degraded(format!(
                    "shepherd is running inside the user manager's delegated cgroups \
                     ({path}), where any process at this uid can join any cgroup — including \
                     shepherd's own"
                ))
            }
            Ok(path) => {
                // Armed and in a cgroup nothing at this uid can join — but the
                // allow-list only separates anything if activities are put
                // somewhere else, and one that shares this cgroup is accepted
                // by it. That makes a failure to isolate activities the same
                // downgrade, reported the same way.
                match shepherd_host_linux::activity_isolation_status() {
                    shepherd_host_linux::ActivityIsolationStatus::Supported => {
                        info!(cgroup = %path, "Management socket accepts only this session and root");
                        IpcPeerHardening::Enforced
                    }
                    shepherd_host_linux::ActivityIsolationStatus::Unsupported { reason } => {
                        IpcPeerHardening::Degraded(format!(
                            "activities cannot be given a cgroup of their own, so they share \
                         shepherd's and the check cannot tell them from the launcher: {reason}"
                        ))
                    }
                }
            }
            Err(e) => IpcPeerHardening::Degraded(format!(
                "shepherd could not read its own cgroup path, so it cannot tell whether the \
                 peer check is a boundary on this host: {e}"
            )),
        }
    }

    /// Report the peer allow-list ending up as anything other than a boundary.
    ///
    /// Separate from arming it because the two happen at opposite ends of
    /// startup: the IPC server is built before there is anywhere to report to,
    /// which is the ordering constraint that already bit once on this branch.
    fn degraded_ipc_hardening_is_reported(
        state: &IpcPeerHardening,
        diagnostics: &dyn DiagnosticSink,
    ) {
        if let IpcPeerHardening::Degraded(reason) = state {
            diagnostics.raise(Self::ipc_not_hardened_diagnostic(reason));
        }
    }

    /// The administrator-facing form of "the management socket is open".
    ///
    /// Split out for the same reason as [`Self::not_hardened_diagnostic`]: the
    /// severity and subject are the part worth asserting, and asserting them
    /// needs no socket. `Critical` because a device that ships this way lets
    /// any activity drive the daemon; `Service` because it is true of the
    /// device, not of one activity.
    fn ipc_not_hardened_diagnostic(reason: &str) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::IpcSocketNotHardened,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Critical,
            message: format!(
                "shepherd's own management socket can be driven by any process running as \
                 this user, so an activity can stop itself, launch another, or log the \
                 session out — {reason}"
            ),
            remedy: Some(
                "Start the session from the installed \"Shepherd Kiosk\" desktop entry, \
                 which puts it in a session cgroup no activity can join, and do not pass \
                 --no-restrict-ipc-peers on a device."
                    .to_string(),
            ),
            since: shepherd_util::now(),
        }
    }

    /// Watch for the management socket being replaced under us (issue #144).
    ///
    /// An activity shares this uid, so it can `unlink()` the socket and bind
    /// its own listener at the same path. Clients refuse to talk to the
    /// impostor — they check the daemon's cgroup the same way the daemon checks
    /// theirs — so nothing is breached; what is lost is reachability, silently.
    /// This turns that into a `Critical` an administrator can see.
    ///
    /// Polled rather than watched with inotify: this is a rare, deliberate act
    /// rather than a hot path, one `stat` a minute costs nothing, and an inotify
    /// watch on a path an activity can delete has its own edge cases.
    fn spawn_socket_watch(ipc: Arc<IpcServer>, diagnostics: Arc<diagnostics::DiagnosticPublisher>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SOCKET_WATCH_INTERVAL);
            ticker.tick().await; // the first tick is immediate
            loop {
                ticker.tick().await;
                if ipc.socket_was_replaced() {
                    warn!(
                        "The management socket is no longer the one this daemon bound; \
                         something at this uid replaced or removed it"
                    );
                    diagnostics.raise(Diagnostic {
                        code: DiagnosticCode::IpcSocketReplaced,
                        subject: DiagnosticSubject::Service,
                        severity: DiagnosticSeverity::Critical,
                        message: "Something replaced shepherd's management socket, so the \
                                  launcher, the HUD and the screen-blank timer can no longer \
                                  reach the daemon. They refuse to talk to whatever bound it \
                                  instead, so nothing has been given away — but this session \
                                  needs restarting."
                            .to_string(),
                        remedy: Some(
                            "Log out and back in. If it recurs, an activity is doing it: the \
                             daemon's log names the cgroup of anything that also tried to \
                             connect."
                                .to_string(),
                        ),
                        since: shepherd_util::now(),
                    });
                    // Once is enough; the condition does not clear by itself and
                    // the session has to be restarted either way.
                    return;
                }
            }
        });
    }

    /// The administrator-facing form of "something tried to drive the daemon".
    ///
    /// Names the peer's cgroup when it could be read: an activity's scope is
    /// named after its session id, so this usually identifies which activity
    /// went looking. Best-effort — the refusal has already happened, and
    /// nothing here influenced it.
    fn ipc_peer_rejected_diagnostic(rejection: &shepherd_ipc::Rejection) -> Diagnostic {
        let who = match (&rejection.peer_cgroup, rejection.peer_pid) {
            (Some(cgroup), _) => format!(" (from {cgroup})"),
            (None, Some(pid)) => format!(" (pid {pid})"),
            (None, None) => String::new(),
        };
        Diagnostic {
            code: DiagnosticCode::IpcPeerRejected,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "a process outside this session tried to drive shepherd and was refused{who}"
            ),
            remedy: Some(
                "Nothing is broken: the request was denied. If it repeats, check what that \
                 activity is doing — reaching for the management socket is not accidental."
                    .to_string(),
            ),
            since: shepherd_util::now(),
        }
    }

    /// The administrator-facing form of "this device is not hardened".
    ///
    /// Split out so the severity and subject can be asserted without a
    /// compositor: `Critical` because the config promises a protection the
    /// device is not providing, and `Service` because it is true of the device
    /// rather than of any one activity.
    fn not_hardened_diagnostic(reason: &str) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::CompositorNotHardened,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Critical,
            message: format!(
                "the compositor's IPC socket is still reachable by every process on this \
                 device, so an activity can drive sway directly — {reason}"
            ),
            remedy: Some(
                "Restart the session. If it recurs, check this daemon's log for the \
                 \"Could not unlink the sway IPC socket\" line, which carries the reason."
                    .to_string(),
            ),
            since: shepherd_util::now(),
        }
    }

    async fn run(mut self) -> Result<()> {
        // Copied out before anything moves out of `self`; the hardening step
        // runs late, once every sway connection is established.
        let sway_ipc_alias = self.sway_ipc_alias.clone();
        let harden_sway_ipc = self.harden_sway_ipc;
        let ipc_peer_hardening = self.ipc_peer_hardening.clone();

        let config_path = self.config_path.clone();

        // Broadcast channel shared by IPC and HTTP SSE
        let (event_tx, _event_rx) = broadcast::channel::<Event>(256);

        // Shutdown signal: any path that should bring down shepherdd flips this
        // to `true`. The main loop, the HTTP server's `with_graceful_shutdown`,
        // and the OS-signal listener task all observe it.
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        // Observed diagnostics (issue #143) are raised on workers and in crates
        // with no access to the engine or the IPC server, so they signal here
        // and the main loop republishes. Created before every raise site: the
        // host adapter's monitor below, and the management service and BLE
        // server further down.
        let (diagnostics_changed_tx, mut diagnostics_changed_rx) = mpsc::unbounded_channel();
        let diagnostic_publisher =
            diagnostics::DiagnosticPublisher::new(self.diagnostics.clone(), diagnostics_changed_tx);

        // Notice if the management socket is replaced under us (issue #144).
        // Started here because this is the first point where there is somewhere
        // to report to — the same ordering constraint the peer allow-list has.
        Self::spawn_socket_watch(self.ipc.clone(), Arc::new(diagnostic_publisher.clone()));

        // The host's sweep raises `CompositorUnreachable` when it cannot read
        // the window list, which would otherwise look exactly like an empty
        // screen (issue #147).
        self.host
            .set_diagnostics(Arc::new(diagnostic_publisher.clone()));

        // Watch sway's window events, so an escaped or orphaned surface is
        // noticed when it maps rather than up to two seconds later — or not at
        // all, if it maps and unmaps inside one sweep (issue #147).
        let _window_watch_handle = match self.host.start_window_watch().await {
            Ok(handle) => Some(handle),
            Err(e) => {
                // Not fatal: the monitor's safety-net sweep still runs, just
                // slowly. `windows_for_sweep` will raise the diagnostic.
                warn!(error = %e, "Could not watch sway window events; escape detection falls back to the slow sweep");
                None
            }
        };

        // Start host process monitor
        let _monitor_handle = self.host.start_monitor();

        // Preload Steam if any Steam entries are configured so it is ready
        // when a user launches a game (skips Steam's startup sequence)
        let has_steam = self
            .engine
            .policy()
            .entries
            .iter()
            .any(|e| matches!(e.kind, EntryKind::Steam { .. }));
        if has_steam {
            info!("Steam entries detected, preloading Steam in background");
            // Hide Steam activities until the preloaded client finishes its
            // initial load (issue #76). Seeded here so the very first served
            // snapshot already gates Steam; the host's readiness watcher flips
            // it to ready (see HostEvent::KindReadinessChanged).
            self.engine.set_kind_readiness(EntryKindTag::Steam, false);
            self.host.preload_steam();
        }

        // Get channels
        let mut host_events = self.host.subscribe();
        let ipc_ref = self.ipc.clone();
        let mut ipc_messages = ipc_ref
            .take_message_receiver()
            .await
            .expect("Message receiver should be available");

        // Wrap mutable state
        let engine = Arc::new(Mutex::new(self.engine));
        let rate_limiter = Arc::new(Mutex::new(self.rate_limiter));
        let host = self.host.clone();
        let volume = self.volume.clone();
        let brightness = self.brightness.clone();
        let light_sensor = self.light_sensor.clone();
        let store = self.store.clone();
        // External monitor / docking controller (issue #87). When docking is
        // disabled in config, a no-op controller is used so the management RPCs
        // still resolve. When enabled, the real `DisplayManager` is also handed
        // to a hotplug watcher and initialized below, and to the HiDPI workaround
        // so the two output-mutating controllers coordinate (the HiDPI apply /
        // restore re-asserts the mirror).
        let display_cfg = { engine.lock().await.policy().service.display.clone() };
        let (display_svc, display_manager): (
            Arc<dyn DisplayController>,
            Option<Arc<DisplayManager>>,
        ) = if display_cfg.docking_enabled {
            let mgr = Arc::new(DisplayManager::new(
                Arc::new(SwayIpcBackend),
                Arc::new(WlMirrorLauncher::new()),
                Arc::new(PipeWireAudioRouter::new()),
                display_cfg.mirror_audio,
                ipc_ref.clone(),
                event_tx.clone(),
            ));
            (mgr.clone() as Arc<dyn DisplayController>, Some(mgr))
        } else {
            (Arc::new(NoOpDisplayController), None)
        };

        // The hidpi manager owns both the IPC server handle and the SSE
        // broadcast channel so it can fan `HudScaleChanged` events out to
        // both subscriber populations without being passed them at each
        // call site (the IPC and HTTP handlers can share the same
        // controller via `Arc<dyn HidpiController>`). It also holds the docking
        // controller so it can re-assert the mirror after changing scales.
        let hidpi = Arc::new(XwaylandHidpi::new(
            ipc_ref.clone(),
            event_tx.clone(),
            display_manager.clone(),
        ));

        // Start management transports (HTTP and/or BLE). Both speak the
        // same shepherd_management::ManagementService, so the service is
        // constructed once and shared.
        let (management_api_config, ble_management_config, auto_brightness_policy) = {
            let eng = engine.lock().await;
            (
                eng.policy().service.management_api.clone(),
                eng.policy().service.ble_management.clone(),
                eng.policy().auto_brightness.clone(),
            )
        };

        // Automatic brightness. Offered only when the host actually exposes a
        // light sensor. The runtime on/off state persists in the store; fall
        // back to the config default the first time (or if the store read
        // fails). Enabling is meaningless without a sensor, so force it off.
        let light_sensor_opt: Option<Arc<dyn LightSensor>> =
            if light_sensor.capabilities().available {
                Some(light_sensor.clone() as Arc<dyn LightSensor>)
            } else {
                None
            };
        let initial_auto_enabled = light_sensor_opt.is_some()
            && match store.get_setting(AUTO_BRIGHTNESS_SETTING_KEY) {
                Ok(Some(v)) => v == "true",
                Ok(None) => auto_brightness_policy.enabled,
                Err(e) => {
                    warn!(error = %e, "Failed to read auto-brightness setting; using config default");
                    auto_brightness_policy.enabled
                }
            };
        let auto_brightness_state =
            Arc::new(Mutex::new(AutoBrightnessState::new(initial_auto_enabled)));
        if light_sensor_opt.is_some() {
            info!(
                enabled = initial_auto_enabled,
                poll_secs = auto_brightness_policy.poll_interval.as_secs(),
                "Automatic brightness available",
            );
        }

        // Construct the management service unconditionally: IPC is
        // always on, and now that IPC dispatches through
        // `dispatch_json` it needs `svc` even when HTTP and BLE are
        // both disabled. The service is cheap to construct — it only
        // holds Arcs of already-live objects.
        // Built as a concrete `Arc<DefaultManagementService>` so the
        // auto-brightness poll loop can call the inherent
        // `auto_brightness_tick`, then shared with the transports as
        // `Arc<dyn ManagementService>`.
        let svc_concrete = {
            let ipc_for_broadcast = ipc_ref.clone();
            let event_tx_for_broadcast = event_tx.clone();
            Arc::new(DefaultManagementService {
                engine: engine.clone(),
                store: store.clone(),
                host: host.clone() as Arc<dyn HostAdapter>,
                volume: volume.clone() as Arc<dyn VolumeController>,
                brightness: brightness.clone() as Arc<dyn BrightnessController>,
                light_sensor: light_sensor_opt.clone(),
                auto_brightness: auto_brightness_state.clone(),
                event_tx: event_tx.clone(),
                broadcast_fn: Arc::new(move |event: Event| {
                    ipc_for_broadcast.broadcast_event(event.clone());
                    let _ = event_tx_for_broadcast.send(event);
                }),
                config_path: config_path.clone(),
                shutdown_tx: shutdown_tx.clone(),
                hidpi: hidpi.clone() as Arc<dyn HidpiController>,
                display: display_svc.clone(),
                last_audio_state: Arc::new(tokio::sync::Mutex::new(None)),
                diagnostics: Some(
                    Arc::new(diagnostic_publisher.clone()) as Arc<dyn shepherd_api::DiagnosticSink>
                ),
            })
        };
        let svc: Arc<dyn ManagementService> = svc_concrete.clone();

        // Audio-output watch loop (issue #124). PipeWire can change the default
        // sink with no involvement from us — a headset is plugged in, a
        // higher-priority USB device appears, the dock router diverts to HDMI —
        // and since volume is remembered per route, the reading changes with it.
        // Polling is enough here and reuses the existing `pw-dump` parser; a
        // `pw-mon` subscription would only buy lower latency.
        //
        // Only runs where outputs can be enumerated at all: on PulseAudio/ALSA
        // hosts `current_output` is always `None`, so a tick could still notice
        // an external volume change, but the sink-switch case it exists for
        // cannot arise.
        if volume.capabilities().backend.as_deref() == Some("pipewire") {
            let svc_for_audio = svc_concrete.clone();
            let mut audio_shutdown_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(AUDIO_WATCH_INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = ticker.tick() => svc_for_audio.audio_watch_tick().await,
                        _ = audio_shutdown_rx.changed() => {
                            if *audio_shutdown_rx.borrow() {
                                break;
                            }
                        }
                    }
                }
            });
            info!(
                poll_secs = AUDIO_WATCH_INTERVAL.as_secs_f32(),
                "Audio-output watch loop started"
            );
        }

        // Automatic-brightness poll loop: sample the light sensor on a timer
        // and let the service decide whether to nudge the backlight. Runs only
        // when a sensor exists; ticks are cheap no-ops while auto is off.
        if light_sensor_opt.is_some() {
            let svc_for_auto = svc_concrete.clone();
            let mut auto_shutdown_rx = shutdown_rx.clone();
            let poll_interval = auto_brightness_policy.poll_interval;
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(poll_interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = ticker.tick() => svc_for_auto.auto_brightness_tick().await,
                        _ = auto_shutdown_rx.changed() => {
                            if *auto_shutdown_rx.borrow() {
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Construct the BLE server first so its ClaimMachine can be
        // handed to HttpServer as the source of unified admin bearer
        // tokens. If BLE isn't configured, HTTP falls back to its
        // static-token-only auth.
        let (ble_handle, admin_authority): (
            Option<tokio::task::JoinHandle<()>>,
            Option<Arc<dyn shepherd_management::AdminAuthority>>,
        ) = match ble_management_config {
            Some(ble_cfg) => {
                let bsc = BleServerConfig {
                    device_name: ble_cfg.device_name,
                    firmware_version: env!("CARGO_PKG_VERSION").to_string(),
                    admin_record_path: ble_cfg.admin_record_path,
                    reset_sentinel_path: ble_cfg.reset_sentinel_path,
                    adapter: ble_cfg.adapter,
                };
                // `shepherd-pairing-display` is spawned per pairing
                // attempt to render the Numeric Comparison passkey on
                // the TV. If the binary is missing the pairing path
                // still completes — the user just won't have an
                // on-device visual to compare against.
                let display = Arc::new(pairing_display::SwayPairingDisplay::new());
                match BleServer::new(bsc, svc.clone(), display) {
                    Ok(server) => {
                        let server = server
                            .with_diagnostics(Arc::new(diagnostic_publisher.clone())
                                as Arc<dyn shepherd_api::DiagnosticSink>);
                        let authority =
                            server.claim_machine() as Arc<dyn shepherd_management::AdminAuthority>;
                        let rx = shutdown_rx.clone();
                        let handle = tokio::spawn(async move {
                            if let Err(e) = server.run(rx).await {
                                error!(error = %e, "BLE management server error");
                            }
                        });
                        (Some(handle), Some(authority))
                    }
                    Err(e) => {
                        error!(error = %e, "BLE management server failed to initialize");
                        (None, None)
                    }
                }
            }
            None => (None, None),
        };

        let http_handle = match management_api_config {
            Some(api_cfg) => {
                let http_state = HttpAppState { svc: svc.clone() };
                let http_server =
                    HttpServer::new(http_state, api_cfg).with_admin_authority(admin_authority);
                let http_shutdown_rx = shutdown_rx.clone();
                Some(tokio::spawn(async move {
                    if let Err(e) = http_server.run(http_shutdown_rx).await {
                        error!(error = %e, "HTTP management API error");
                    }
                }))
            }
            None => None,
        };

        // System event watcher (logind + NetworkManager). Always running so the
        // suspend cover (issue #73) works regardless of internet gating: it
        // broadcasts SystemSuspending/SystemResumed and asks for a fresh state
        // snapshot on resume via `resume_rx`. When an internet monitor is
        // configured it also nudges it to re-check immediately on resume /
        // network change instead of waiting for the next poll interval.
        let (resume_tx, mut resume_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let recheck_tx = if let Some(monitor) = self.internet_monitor {
            let engine_ref = engine.clone();
            let ipc_for_monitor = ipc_ref.clone();
            let event_tx_for_monitor = event_tx.clone();
            let (recheck_tx, recheck_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                monitor
                    .run(
                        engine_ref,
                        ipc_for_monitor,
                        event_tx_for_monitor,
                        recheck_rx,
                    )
                    .await;
            });
            Some(recheck_tx)
        } else {
            None
        };

        // Input-device dependency monitor (issue #96): tracks which input device
        // types are connected and re-broadcasts availability on hotplug so
        // input-gated entries (e.g. a typing tutor requiring a keyboard) show and
        // hide as hardware is attached/removed.
        if let Some(monitor) = self.input_monitor {
            let engine_ref = engine.clone();
            let ipc_for_monitor = ipc_ref.clone();
            let event_tx_for_monitor = event_tx.clone();
            tokio::spawn(async move {
                monitor
                    .run(engine_ref, ipc_for_monitor, event_tx_for_monitor)
                    .await;
            });
        }

        // Background media prefetch (issue #127). Session and connectivity
        // state come off the event bus; it takes the engine to re-read the
        // media settings and the library list before each sweep, so a config
        // reload reaches both — including the eviction grace, which the launch
        // path is already handing to activities from the live policy.
        {
            let prefetcher = self.media_prefetcher;
            let events = event_tx.subscribe();
            let engine_for_prefetch = engine.clone();
            let publisher = diagnostic_publisher.clone();
            tokio::spawn(
                async move { prefetcher.run(engine_for_prefetch, events, publisher).await },
            );
        }

        {
            let ipc_for_sys = ipc_ref.clone();
            let event_tx_for_sys = event_tx.clone();
            let broadcast: system_events::BroadcastFn = Arc::new(move |event: Event| {
                ipc_for_sys.broadcast_event(event.clone());
                let _ = event_tx_for_sys.send(event);
            });
            system_events::spawn_system_event_watchers(broadcast, recheck_tx, resume_tx);
        }

        // Spawn IPC accept task
        let ipc_accept = ipc_ref.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc_accept.run().await {
                error!(error = %e, "IPC server error");
            }
        });

        // Initialize the display arrangement (detect primary, mirror any already
        // connected external) and watch for hotplug events (issue #87).
        if let Some(mgr) = display_manager {
            let init_mgr = mgr.clone();
            tokio::spawn(async move { init_mgr.initialize().await });
            display_watch::spawn(mgr, shutdown_rx.clone()).await;
        }

        // The peer allow-list was decided at construction, before there was
        // anywhere to report to; say so now if it did not end up a boundary
        // (issue #144).
        Self::degraded_ipc_hardening_is_reported(&ipc_peer_hardening, &diagnostic_publisher);

        // Every sway connection this daemon needs is now open, so the socket's
        // name in the filesystem has done its job (issue #144).
        Self::harden_compositor_socket(
            sway_ipc_alias.as_deref(),
            harden_sway_ipc,
            &diagnostic_publisher,
        )
        .await;

        // Set up config file watcher
        let (config_change_tx, mut config_change_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watched_path = config_path.clone();
        let _config_watcher: Option<RecommendedWatcher> = {
            let tx = config_change_tx;
            match RecommendedWatcher::new(
                move |result: notify::Result<notify::Event>| {
                    if let Ok(event) = result {
                        let is_relevant = matches!(
                            event.kind,
                            notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                        );
                        if is_relevant && event.paths.iter().any(|p| p == &watched_path) {
                            let _ = tx.send(());
                        }
                    }
                },
                notify::Config::default(),
            ) {
                Ok(mut watcher) => {
                    if let Some(dir) = config_path.parent() {
                        match watcher.watch(dir, RecursiveMode::NonRecursive) {
                            Ok(()) => {
                                info!(
                                    config_path = %config_path.display(),
                                    "Watching config file for changes"
                                );
                                Some(watcher)
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to watch config directory, auto-reload disabled");
                                None
                            }
                        }
                    } else {
                        warn!("Config path has no parent directory, auto-reload disabled");
                        None
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to create config watcher, auto-reload disabled");
                    None
                }
            }
        };

        // Set up signal handlers as a spawned listener that flips the shared
        // shutdown signal. This unifies the OS-signal path with the
        // logout-handler path so the main loop only watches one source.
        let mut sigterm =
            signal(SignalKind::terminate()).context("Failed to create SIGTERM handler")?;
        let mut sigint =
            signal(SignalKind::interrupt()).context("Failed to create SIGINT handler")?;
        let mut sighup = signal(SignalKind::hangup()).context("Failed to create SIGHUP handler")?;
        let signal_shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = sigterm.recv() => info!("Received SIGTERM, shutting down gracefully"),
                _ = sigint.recv() => info!("Received SIGINT, shutting down gracefully"),
                _ = sighup.recv() => info!("Received SIGHUP, shutting down gracefully"),
            }
            let _ = signal_shutdown_tx.send(true);
        });

        // Main event loop
        let tick_interval = Duration::from_millis(100);
        let mut tick_timer = tokio::time::interval(tick_interval);

        // Diagnostic sweep (issue #143). `interval` fires immediately, so the
        // first tick is the startup sweep and there is no separate call for it.
        // Hourly afterwards: these are conditions somebody has to go and fix,
        // not fast-moving state, and the firewall probe execs `pkcheck`.
        let mut diagnostic_timer = tokio::time::interval(DIAGNOSTIC_SWEEP_INTERVAL);
        let diagnostics = self.diagnostics.clone();
        let sound_backend_available = volume.capabilities().available;

        info!("Service running");

        loop {
            tokio::select! {
                // Shutdown requested (signal, HTTP logout, or IPC logout)
                Ok(()) = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }

                // Tick timer - check warnings and expiry
                _ = tick_timer.tick() => {
                    let now_mono = MonotonicInstant::now();
                    let now = shepherd_util::now();

                    let events = {
                        let mut engine = engine.lock().await;
                        engine.tick(now_mono, now)
                    };

                    for event in events {
                        Self::handle_core_event(&engine, &host, &ipc_ref, &event_tx, &hidpi, event, now_mono, now).await;
                    }
                }

                // Diagnostic sweep: recompute every probed condition (issue
                // #143). This is what makes an installed dependency or a freed
                // disk clear itself without a daemon restart.
                _ = diagnostic_timer.tick() => {
                    Self::sweep_diagnostics(
                        &engine, &diagnostics, &ipc_ref, &event_tx, sound_backend_available,
                    ).await;
                }

                // An observed diagnostic was raised or cleared on a worker.
                Some(()) = diagnostics_changed_rx.recv() => {
                    // Coalesce a burst — one prefetch sweep can raise several —
                    // so a run of changes publishes once.
                    while diagnostics_changed_rx.try_recv().is_ok() {}
                    Self::publish_diagnostics(&engine, &diagnostics, &ipc_ref, &event_tx).await;
                }

                // Host events (process exit)
                Some(host_event) = host_events.recv() => {
                    Self::handle_host_event(&engine, &ipc_ref, &event_tx, &hidpi, host_event).await;
                }

                // Resumed from suspend - push a fresh state snapshot so clients
                // can drop the suspend cover with up-to-date content.
                Some(()) = resume_rx.recv() => {
                    let state = {
                        let engine = engine.lock().await;
                        engine.get_state()
                    };
                    Self::broadcast(&ipc_ref, &event_tx, Event::new(EventPayload::StateChanged(state)));
                }

                // Config file changed on disk
                Some(()) = config_change_rx.recv() => {
                    // Drain any additional buffered events to debounce rapid saves
                    while config_change_rx.try_recv().is_ok() {}
                    Self::handle_config_reload(&engine, &ipc_ref, &event_tx, &config_path).await;
                    // Re-probe against the new policy. Without this an admin who
                    // adds a YouTube entry sees no missing-yt-dlp diagnostic
                    // until the next restart, and one who removes the entry
                    // keeps a diagnostic about an activity that is gone.
                    Self::sweep_diagnostics(
                        &engine, &diagnostics, &ipc_ref, &event_tx, sound_backend_available,
                    ).await;
                }

                // IPC messages
                Some(msg) = ipc_messages.recv() => {
                    Self::handle_ipc_message(
                        &svc, &ipc_ref, &store, &rate_limiter, &diagnostic_publisher, msg,
                    )
                    .await;
                }
            }
        }

        // Graceful shutdown
        info!("Shutting down shepherdd");

        // Stop all running sessions
        {
            let engine = engine.lock().await;
            if let Some(session) = engine.current_session() {
                info!(session_id = %session.plan.session_id, "Stopping active session");
                if let Some(handle) = &session.host_handle
                    && let Err(e) = host
                        .stop(
                            handle,
                            HostStopMode::Graceful {
                                timeout: Duration::from_secs(5),
                            },
                        )
                        .await
                {
                    warn!(error = %e, "Failed to stop session gracefully");
                }
            }
        }

        // Restore sway output scales if the XWayland HiDPI workaround was
        // active for the session we just stopped. host.logout() below tears
        // down sway anyway, but this keeps us tidy if logout fails.
        hidpi.restore().await;

        // Stop preloaded Steam (if any) after active sessions are terminated
        host.stop_steam_preload();

        // Exit the desktop session (e.g. `swaymsg exit`). Doing this here, after
        // sessions are stopped and after the HTTP server has begun graceful
        // shutdown, ensures the in-flight logout response is flushed before the
        // browser is torn down with sway.
        if let Err(e) = host.logout().await {
            warn!(error = %e, "Logout (host exit) failed");
        }

        // Wait for the HTTP server to drain. SSE clients will disconnect when
        // sway exits, but we cap the wait so a stuck client cannot block
        // shutdown indefinitely.
        if let Some(mut handle) = http_handle {
            match tokio::time::timeout(Duration::from_secs(3), &mut handle).await {
                Ok(Ok(())) => info!("HTTP server drained"),
                Ok(Err(e)) => warn!(error = %e, "HTTP server task failed during shutdown"),
                Err(_) => {
                    warn!("HTTP server did not drain within 3s; aborting");
                    handle.abort();
                }
            }
        }

        // Same drain treatment for the BLE server. The bluer adapter
        // release happens in `BleServer::run`'s drop guards on
        // ApplicationHandle / AdvertisementHandle / AgentHandle.
        if let Some(mut handle) = ble_handle {
            match tokio::time::timeout(Duration::from_secs(3), &mut handle).await {
                Ok(Ok(())) => info!("BLE server drained"),
                Ok(Err(e)) => warn!(error = %e, "BLE server task failed during shutdown"),
                Err(_) => {
                    warn!("BLE server did not drain within 3s; aborting");
                    handle.abort();
                }
            }
        }

        // Log shutdown
        if let Err(e) = store.append_audit(AuditEvent::new(AuditEventType::ServiceStopped)) {
            warn!(error = %e, "Failed to log service shutdown");
        }

        info!("Shutdown complete");
        Ok(())
    }

    /// Broadcast an event to both IPC subscribers and HTTP SSE subscribers
    fn broadcast(ipc: &Arc<IpcServer>, tx: &broadcast::Sender<Event>, event: Event) {
        ipc.broadcast_event(event.clone());
        let _ = tx.send(event);
    }

    async fn handle_config_reload(
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        config_path: &Path,
    ) {
        match load_config(config_path) {
            Ok(policy) => {
                let entry_count = {
                    let event = engine.lock().await.reload_policy(policy);
                    if let CoreEvent::PolicyReloaded { entry_count } = event {
                        entry_count
                    } else {
                        0
                    }
                };
                info!(
                    entry_count,
                    config_path = %config_path.display(),
                    "Config reloaded"
                );
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::PolicyReloaded { entry_count }),
                );
                let state = engine.lock().await.get_state();
                Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
            }
            Err(e) => {
                warn!(error = %e, "Failed to reload config, keeping existing policy");
            }
        }
    }

    /// Recompute every probed diagnostic and publish the result (issue #143).
    ///
    /// Broadcasts only when the set actually changed, so an hourly sweep over a
    /// healthy device is silent rather than pushing an identical snapshot to
    /// every connected client once an hour.
    async fn sweep_diagnostics(
        engine: &Arc<Mutex<CoreEngine>>,
        diagnostics: &Arc<diagnostics::DiagnosticRegistry>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        sound_backend_available: bool,
    ) {
        let policy = { engine.lock().await.policy().clone() };
        let facts = diagnostics::gather_facts(&policy, sound_backend_available).await;
        let fresh = diagnostics::evaluate(&facts, shepherd_util::now());

        // Feed the firewall answer to the availability gate before publishing.
        // An entry whose configured firewall cannot be applied stops launching,
        // and the child sees `ReasonCode::ProtectionUnavailable` rather than
        // getting an activity the config promised would be filtered.
        let gate_changed = match &facts.firewall {
            Some(diagnostics::FirewallFact::Enforceable) => {
                engine.lock().await.set_firewall_enforceable(true)
            }
            Some(diagnostics::FirewallFact::Unenforceable { .. }) => {
                engine.lock().await.set_firewall_enforceable(false)
            }
            // Probe failed to run at all: leave the gate as it was rather than
            // guessing, so a transient failure cannot blank the grid.
            None => false,
        };

        if !diagnostics.replace_probed(fresh) && !gate_changed {
            return;
        }

        Self::publish_diagnostics(engine, diagnostics, ipc, event_tx).await;
    }

    /// Push the current diagnostic set onto the snapshot and out to clients.
    ///
    /// Shared by the probed sweep and the observed-change path, so the two
    /// cannot drift into publishing differently.
    async fn publish_diagnostics(
        engine: &Arc<Mutex<CoreEngine>>,
        diagnostics: &Arc<diagnostics::DiagnosticRegistry>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
    ) {
        let set = diagnostics.current();
        debug!(
            count = set.items.len(),
            critical = set.has_critical(),
            "Diagnostics changed"
        );

        // Both: the snapshot so a client connecting later is correct, the event
        // so one already connected does not wait for the next state change.
        let state = {
            let mut engine = engine.lock().await;
            engine.set_diagnostics(set.clone());
            engine.get_state()
        };
        Self::broadcast(
            ipc,
            event_tx,
            Event::new(EventPayload::DiagnosticsChanged(set)),
        );
        Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_core_event(
        engine: &Arc<Mutex<CoreEngine>>,
        host: &Arc<LinuxHost>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        hidpi: &Arc<XwaylandHidpi>,
        event: CoreEvent,
        _now_mono: MonotonicInstant,
        _now: chrono::DateTime<chrono::Local>,
    ) {
        match &event {
            CoreEvent::Warning {
                session_id,
                threshold_seconds,
                time_remaining,
                severity,
                message,
            } => {
                info!(
                    session_id = %session_id,
                    threshold = threshold_seconds,
                    remaining = ?time_remaining,
                    "Warning issued"
                );

                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::WarningIssued {
                        session_id: session_id.clone(),
                        threshold_seconds: *threshold_seconds,
                        time_remaining: *time_remaining,
                        severity: *severity,
                        message: message.clone(),
                    }),
                );
            }

            CoreEvent::ExpireDue { session_id } => {
                info!(session_id = %session_id, "Session expired, stopping");

                // Get the host handle and stop it
                let handle = {
                    let engine = engine.lock().await;
                    engine.current_session().and_then(|s| s.host_handle.clone())
                };

                if let Some(handle) = handle
                    && let Err(e) = host
                        .stop(
                            &handle,
                            HostStopMode::Graceful {
                                timeout: Duration::from_secs(5),
                            },
                        )
                        .await
                {
                    warn!(error = %e, "Failed to stop session gracefully, forcing");
                    let _ = host.stop(&handle, HostStopMode::Force).await;
                }

                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::SessionExpiring {
                        session_id: session_id.clone(),
                    }),
                );
            }

            CoreEvent::SessionStarted {
                session_id,
                entry_id,
                label,
                deadline,
                confirm_on_close,
                can_reset,
            } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::SessionStarted {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        label: label.clone(),
                        deadline: *deadline,
                        confirm_on_close: *confirm_on_close,
                        can_reset: *can_reset,
                    }),
                );
            }

            CoreEvent::SessionEnded {
                session_id,
                entry_id,
                reason,
                duration,
            } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::SessionEnded {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        reason: reason.clone(),
                        duration: *duration,
                    }),
                );

                // Restore the compositor scale if an XWayland HiDPI workaround
                // was in effect (no-op otherwise).
                hidpi.restore().await;

                // Broadcast state change
                let state = {
                    let engine = engine.lock().await;
                    engine.get_state()
                };
                Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
            }

            CoreEvent::PolicyReloaded { entry_count } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::PolicyReloaded {
                        entry_count: *entry_count,
                    }),
                );
            }

            CoreEvent::EntryAvailabilityChanged { entry_id, enabled } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::EntryAvailabilityChanged {
                        entry_id: entry_id.clone(),
                        enabled: *enabled,
                    }),
                );
            }

            CoreEvent::AvailabilitySetChanged => {
                // Time-based availability change - broadcast updated state
                let state = {
                    let engine = engine.lock().await;
                    engine.get_state()
                };
                Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
            }
        }
    }

    async fn handle_host_event(
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        hidpi: &Arc<XwaylandHidpi>,
        event: HostEvent,
    ) {
        match event {
            HostEvent::Exited { handle, status } => {
                let now_mono = MonotonicInstant::now();
                let now = shepherd_util::now();

                info!(
                    session_id = %handle.session_id,
                    status = ?status,
                    "Host process exited - will end session"
                );

                // Matched against the current session by handle payload: the
                // monitor cannot know the session id, so it reports a
                // fabricated one. An unmatched exit belongs to a previous
                // activity whose reap is only surfacing now, and must not end
                // whatever session replaced it (issue #136).
                let core_event = {
                    let mut engine = engine.lock().await;
                    engine.notify_activity_exited(&handle, status.code, now_mono, now)
                };

                info!(
                    has_event = core_event.is_some(),
                    "notify_activity_exited result"
                );

                if let Some(CoreEvent::SessionEnded {
                    session_id,
                    entry_id,
                    reason,
                    duration,
                }) = core_event
                {
                    info!(
                        session_id = %session_id,
                        entry_id = %entry_id,
                        reason = ?reason,
                        duration_secs = duration.as_secs(),
                        "Broadcasting SessionEnded"
                    );
                    Self::broadcast(
                        ipc,
                        event_tx,
                        Event::new(EventPayload::SessionEnded {
                            session_id,
                            entry_id,
                            reason,
                            duration,
                        }),
                    );

                    // Restore the compositor scale (and HUD factor) if an
                    // XWayland HiDPI workaround was in effect for this session.
                    hidpi.restore().await;

                    // Broadcast state change
                    let state = {
                        let engine = engine.lock().await;
                        engine.get_state()
                    };
                    info!("Broadcasting StateChanged");
                    Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
                }
            }

            HostEvent::WindowReady { handle } => {
                debug!(session_id = %handle.session_id, "Window ready");
                // Usage is billed from here rather than from approval: until
                // now the child was looking at a spinner (issue #135).
                let mut engine = engine.lock().await;
                engine.notify_window_ready(&handle, MonotonicInstant::now());
            }

            HostEvent::KindReadinessChanged { kind, ready } => {
                let changed = {
                    let mut engine = engine.lock().await;
                    engine.set_kind_readiness(kind, ready)
                };
                if changed {
                    info!(?kind, ready, "Activity kind readiness changed");
                    // Re-broadcast state so the launcher shows/hides the now
                    // (un)gated entries of this kind.
                    let state = {
                        let engine = engine.lock().await;
                        engine.get_state()
                    };
                    Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
                }
            }

            HostEvent::SpawnFailed { session_id, error } => {
                error!(session_id = %session_id, error = %error, "Spawn failed");
            }

            HostEvent::LaunchFailed { handle, error } => {
                let now_mono = MonotonicInstant::now();
                let now = shepherd_util::now();
                warn!(session_id = %handle.session_id, error = %error, "Launch never started");

                let core_event = {
                    let mut engine = engine.lock().await;
                    engine.notify_launch_failed(Some(&handle), error, now_mono, now)
                };

                if let Some(CoreEvent::SessionEnded {
                    session_id,
                    entry_id,
                    reason,
                    duration,
                }) = core_event
                {
                    Self::broadcast(
                        ipc,
                        event_tx,
                        Event::new(EventPayload::SessionEnded {
                            session_id,
                            entry_id,
                            reason,
                            duration,
                        }),
                    );
                    hidpi.restore().await;
                    let state = {
                        let engine = engine.lock().await;
                        engine.get_state()
                    };
                    Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
                }
            }

            HostEvent::ActivityEscaped {
                session_id,
                pid,
                command,
                resolved,
            } => {
                if resolved {
                    info!(
                        session_id = %session_id,
                        pid, command = %command,
                        "Escaped activity has been cleaned up"
                    );
                } else {
                    error!(
                        session_id = %session_id,
                        pid, command = %command,
                        "Activity outlived its session and every kill; supervision lost"
                    );
                }
                // Audit it either way: a caregiver reading the log should be
                // able to see that supervision was lost and when it came back.
                let audited = {
                    let engine = engine.lock().await;
                    engine
                        .store()
                        .append_audit(AuditEvent::new(AuditEventType::ActivityEscaped {
                            session_id,
                            pid,
                            command,
                            resolved,
                        }))
                };
                if let Err(e) = audited {
                    warn!(error = %e, "Failed to audit escaped activity");
                }
            }
        }
    }

    /// Handle one incoming message from the IPC socket.
    ///
    /// Requests are dispatched through `shepherd_management::dispatch_json`
    /// — the generated JSON-RPC router keeps this method thin. The two
    /// wire-method names that don't go through the trait are the
    /// subscribe / unsubscribe pair: they flip a per-client
    /// subscription flag on the writer task *after* the response
    /// frame is on the wire, preventing broadcast events from
    /// arriving before the subscribe acknowledgement.
    async fn handle_ipc_message(
        svc: &Arc<dyn ManagementService>,
        ipc: &Arc<IpcServer>,
        store: &Arc<dyn Store>,
        rate_limiter: &Arc<Mutex<RateLimiter>>,
        diagnostics: &dyn DiagnosticSink,
        msg: ServerMessage,
    ) {
        match msg {
            ServerMessage::Request { client_id, request } => {
                if !rate_limiter.lock().await.check(&client_id) {
                    let resp = Response::error(
                        request.request_id,
                        ErrorInfo::new(ErrorCode::RateLimited, "Too many requests"),
                    );
                    let _ = ipc.send_response(&client_id, resp).await;
                    return;
                }

                if request.api_version != shepherd_api::API_VERSION {
                    let resp = Response::error(
                        request.request_id,
                        ErrorInfo::new(
                            ErrorCode::InvalidRequest,
                            format!(
                                "unsupported api_version {} (server speaks {})",
                                request.api_version,
                                shepherd_api::API_VERSION
                            ),
                        ),
                    );
                    let _ = ipc.send_response(&client_id, resp).await;
                    return;
                }

                match request.method.as_str() {
                    "subscribe_events" => {
                        let resp = Response::success(request.request_id, serde_json::Value::Null);
                        let _ = ipc.send_subscribe_response(&client_id, resp).await;
                    }
                    "unsubscribe_events" => {
                        let resp = Response::success(request.request_id, serde_json::Value::Null);
                        let _ = ipc.send_unsubscribe_response(&client_id, resp).await;
                    }
                    _ => {
                        let resp = dispatch_ipc(
                            svc.as_ref(),
                            &request.method,
                            request.params,
                            request.request_id,
                        )
                        .await;
                        let _ = ipc.send_response(&client_id, resp).await;
                    }
                }
            }

            ServerMessage::ClientConnected { client_id, info } => {
                info!(
                    client_id = %client_id,
                    role = ?info.role,
                    uid = ?info.uid,
                    "Client connected"
                );
                let _ = store.append_audit(AuditEvent::new(AuditEventType::ClientConnected {
                    client_id: client_id.to_string(),
                    role: format!("{:?}", info.role),
                    uid: info.uid,
                }));
            }

            ServerMessage::ClientRejected { rejection } => {
                // Already logged with full detail by the IPC layer; here it
                // becomes something an administrator can see (issue #143).
                diagnostics.raise(Self::ipc_peer_rejected_diagnostic(&rejection));
            }

            ServerMessage::ClientDisconnected { client_id } => {
                debug!(client_id = %client_id, "Client disconnected");
                let _ = store.append_audit(AuditEvent::new(AuditEventType::ClientDisconnected {
                    client_id: client_id.to_string(),
                }));
                rate_limiter.lock().await.remove_client(&client_id);
            }
        }
    }
}

/// Route an RPC to the trait via `dispatch_json` and translate its
/// error shape onto the IPC wire's `ErrorCode`. Kept as a free
/// function (not a `Service` method) so it doesn't drag the full
/// `Service` fixture into the small set of ManagementError → ErrorCode
/// mappings.
async fn dispatch_ipc(
    svc: &dyn ManagementService,
    method: &str,
    params: serde_json::Value,
    request_id: u64,
) -> Response {
    match shepherd_management::dispatch_json(svc, method, params).await {
        Ok(value) => Response::success(request_id, value),
        Err(shepherd_management::RpcDispatchError::MethodNotFound(m)) => Response::error(
            request_id,
            ErrorInfo::new(ErrorCode::MethodNotFound, format!("unknown method '{m}'")),
        ),
        Err(shepherd_management::RpcDispatchError::InvalidParams(msg)) => {
            Response::error(request_id, ErrorInfo::new(ErrorCode::InvalidParams, msg))
        }
        Err(shepherd_management::RpcDispatchError::Serialization(msg)) => {
            Response::error(request_id, ErrorInfo::new(ErrorCode::Internal, msg))
        }
        Err(shepherd_management::RpcDispatchError::Management(e)) => {
            let (code, msg) = match e {
                shepherd_management::ManagementError::NotFound(m) => (ErrorCode::NotFound, m),
                shepherd_management::ManagementError::BadRequest(m) => (ErrorCode::BadRequest, m),
                shepherd_management::ManagementError::Forbidden(m) => (ErrorCode::Forbidden, m),
                shepherd_management::ManagementError::Conflict(m) => (ErrorCode::Conflict, m),
                shepherd_management::ManagementError::Unprocessable(m) => {
                    (ErrorCode::Unprocessable, m)
                }
                shepherd_management::ManagementError::Internal(m) => (ErrorCode::Internal, m),
            };
            Response::error(request_id, ErrorInfo::new(code, msg))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();

    info!(version = env!("CARGO_PKG_VERSION"), "shepherdd starting");

    // Create and run the service
    let service = Service::new(&args).await?;
    service.run().await
}

#[cfg(test)]
mod harden_diagnostic_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Captures what the hardening step reports, so the failure paths can be
    /// exercised without a compositor.
    #[derive(Default)]
    struct RecordingSink {
        raised: StdMutex<Vec<DiagnosticCode>>,
    }

    impl DiagnosticSink for RecordingSink {
        fn raise(&self, diagnostic: Diagnostic) {
            self.raised.lock().unwrap().push(diagnostic.code);
        }
        fn clear(&self, _code: DiagnosticCode, _subject: &DiagnosticSubject) {}
    }

    /// Opting out has to leave the socket exactly as open as it was before
    /// #144, and say so without reporting a problem — a developer who asked
    /// for this is not looking at a broken device.
    #[test]
    fn opting_out_of_the_peer_check_reports_nothing() {
        let mut ipc = IpcServer::new("/nonexistent-dir-for-shepherd-tests/ipc.sock");
        assert_eq!(
            Service::arm_ipc_peer_policy(&mut ipc, false),
            IpcPeerHardening::OptedOut
        );
    }

    /// The check being armed is not the same as the check being a boundary.
    /// Where shepherd sits in a cgroup any process at this uid can join —
    /// a stack started from a shell, which is every dev session — arming it
    /// changes nothing, and a device configured that way has to say so rather
    /// than look protected.
    #[test]
    fn a_degraded_peer_check_is_reported_as_such() {
        let sink = RecordingSink::default();
        Service::degraded_ipc_hardening_is_reported(
            &IpcPeerHardening::Degraded("test reason".into()),
            &sink,
        );
        assert_eq!(
            *sink.raised.lock().unwrap(),
            vec![DiagnosticCode::IpcSocketNotHardened],
            "a socket that is not a boundary has to say so"
        );
    }

    /// The other two outcomes are silent: one is working as intended, the
    /// other is a deliberate choice.
    #[test]
    fn an_enforced_or_opted_out_peer_check_reports_nothing() {
        for state in [IpcPeerHardening::Enforced, IpcPeerHardening::OptedOut] {
            let sink = RecordingSink::default();
            Service::degraded_ipc_hardening_is_reported(&state, &sink);
            assert!(
                sink.raised.lock().unwrap().is_empty(),
                "{state:?} should not raise a diagnostic"
            );
        }
    }

    /// A refused peer is an administrator-facing condition, and the message
    /// has to name who it was — an activity's scope carries its session id, so
    /// this is what turns "something probed the socket" into "this activity
    /// did". Dropping the cgroup would leave a report nobody can act on.
    #[test]
    fn a_refused_peer_names_where_it_came_from() {
        let d = Service::ipc_peer_rejected_diagnostic(&shepherd_ipc::Rejection {
            reason: "not shepherd's own".into(),
            peer_cgroup: Some("/system.slice/shepherd-abc-123.scope".into()),
            peer_pid: Some(4242),
        });
        assert_eq!(d.code, DiagnosticCode::IpcPeerRejected);
        assert_eq!(d.subject, DiagnosticSubject::Service);
        assert!(
            d.message.contains("shepherd-abc-123.scope"),
            "the report has to name the activity: {}",
            d.message
        );
    }

    /// An alias that cannot be created, so hardening always fails somewhere:
    /// with no compositor reachable it fails at `connect_now`, and inside a
    /// live sway session it gets as far as `alias_socket` and fails there.
    /// Either way a stack that asked to be hardened is not hardened, which is
    /// the only thing these tests care about — and it keeps them from
    /// depending on whether the developer runs `cargo test` inside sway.
    fn impossible_alias() -> &'static Path {
        Path::new("/nonexistent-dir-for-shepherd-tests/alias.sock")
    }

    /// The regression this guards: every failure path returns early, leaves
    /// the session running, and leaves the socket reachable. Nothing else
    /// about the device looks wrong, so if the report is ever dropped the
    /// downgrade becomes invisible (issue #144).
    #[tokio::test]
    async fn a_hardening_failure_is_reported() {
        let sink = RecordingSink::default();
        Service::harden_compositor_socket(Some(impossible_alias()), true, &sink).await;

        assert_eq!(
            *sink.raised.lock().unwrap(),
            vec![DiagnosticCode::CompositorNotHardened],
            "a stack that asked to be hardened, and was not, has to say so"
        );
    }

    /// The same failure on a stack that never asked to be hardened is not a
    /// downgrade — there was nothing to fail to do. Reporting it would put a
    /// `Critical` on every developer session, which is how a channel stops
    /// being read.
    #[tokio::test]
    async fn the_same_failure_is_silent_when_hardening_was_not_asked_for() {
        let sink = RecordingSink::default();
        Service::harden_compositor_socket(Some(impossible_alias()), false, &sink).await;

        assert!(
            sink.raised.lock().unwrap().is_empty(),
            "an unhardened dev session is not an administrator-facing condition"
        );
    }

    /// The severity is the whole point: this is the config promising a
    /// protection the device is not providing, which is what `Critical` means
    /// here — and it is about the device, not any one activity.
    ///
    /// Pinned because every failure path deliberately leaves the session
    /// running and looking healthy (issue #144). If this stops being reported,
    /// a device ships unhardened with nothing to show for it, which is the
    /// exact silent downgrade the diagnostic exists to prevent.
    #[test]
    fn a_failed_hardening_is_a_critical_service_condition() {
        let d = Service::not_hardened_diagnostic("the socket's name could not be removed: EACCES");

        assert_eq!(d.code, DiagnosticCode::CompositorNotHardened);
        assert_eq!(d.severity, DiagnosticSeverity::Critical);
        assert!(matches!(d.subject, DiagnosticSubject::Service));
        assert!(
            d.message.contains("EACCES"),
            "the underlying reason has to survive into the message, or the \
             administrator sees a condition with no cause: {}",
            d.message
        );
        assert!(d.remedy.is_some(), "a Critical condition needs an answer");
    }
}
