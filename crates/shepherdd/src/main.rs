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
use shepherd_config::{LockMode, load_config};
use shepherd_core::{CoreEngine, CoreEvent};
use shepherd_host_api::{
    BrightnessController, DisplayController, HidpiController, HostAdapter, HostEvent,
    HudLayoutController, LightSensor, NetworkInfoProvider, NoOpDisplayController,
    StopMode as HostStopMode, VolumeController,
};
use shepherd_host_linux::{
    LinuxBrightnessController, LinuxHost, LinuxLightSensor, LinuxNetworkInfo,
    LinuxVolumeController, PipeWireAudioRouter, SwayIpcBackend, WaydroidLockMode,
};
use shepherd_http::{AppState as HttpAppState, HttpServer};
use shepherd_ipc::{IpcServer, ServerMessage};
use shepherd_management::{
    AUTO_BRIGHTNESS_SETTING_KEY, AutoBrightnessState, DefaultManagementService, ManagementService,
    WebListenerHandle,
};
use shepherd_state_proto::{RemoteFiles, RemoteStore, Supervision};
use shepherd_store::{AuditEvent, AuditEventType, SqliteStore, Store};
use shepherd_util::{
    LocalProtectedFiles, MonotonicInstant, ProtectedFile, ProtectedFiles, RateLimiter,
    default_config_path,
};
use std::net::SocketAddr;
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

/// How often the web credential store expires what has gone stale (issue #156).
///
/// Session expiry is enforced on every request anyway; this only decides how
/// long a dead session lingers in the list a parent is looking at.
const WEB_AUTH_SWEEP: Duration = Duration::from_secs(30);

/// How often the on-screen setup card re-checks what it should be saying.
///
/// Faster than the sweep because the card goes up during startup, before the
/// HTTP listener has bound — and until it has, `management_urls` is empty and
/// the card can only offer a port. Five seconds is how long a parent spends
/// looking at the weaker message on a cold boot, and a NetworkManager read is
/// only made while a setup code exists at all.
const SETUP_CARD_POLL: Duration = Duration::from_secs(5);

/// What the setup card is currently showing (issue #156).
///
/// Compared rather than blindly re-spawned: the poll is fast, and restarting
/// the overlay subprocess on every tick would flash the card in the face of
/// the person reading the code off it.
/// What starting BLE management leaves behind for the rest of the daemon.
///
/// One claim machine wearing two hats — it verifies the bearer tokens the HTTP
/// middleware is presented with, and it is the administrator roster both
/// transports manage (issue #149) — plus the task serving GATT. `Default` is
/// the shape of a device with Bluetooth management switched off, or one whose
/// BLE server failed to start: degraded, and everything else still runs.
#[derive(Default)]
struct BleManagement {
    handle: Option<tokio::task::JoinHandle<()>>,
    authority: Option<Arc<dyn shepherd_management::AdminAuthority>>,
    roster: Option<Arc<dyn shepherd_management::AdminRoster>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupCardContent {
    code: String,
    /// Every URL the device is reachable at, from the live network status —
    /// so a wildcard bind names its wifi and VPN addresses rather than
    /// nothing (issue #182).
    urls: Vec<String>,
    /// The port on its own, for the window before the listener has bound.
    port: Option<u16>,
}

mod diagnostics;
mod display;
mod display_watch;
mod hidpi;
mod hud_layout;
mod input_devices;
mod internet;
mod media;
mod pairing_display;
mod system_events;

use display::{DisplayManager, WlMirrorLauncher};
use hidpi::XwaylandHidpi;
use hud_layout::HudLayout;

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
    /// removes the first.
    ///
    /// Must be on the same filesystem as the socket — i.e. inside
    /// `$XDG_RUNTIME_DIR` — because the alias is a hard link. Without this,
    /// hardening leaves nothing able to reach the compositor except shepherdd
    /// itself, which is the point in production and unusable in dev.
    // Deliberately no `env =` (issue #144). This is the most dangerous of the
    // development switches to leave environment-settable: the others disarm a
    // check, this one *hands out a working compositor socket* at a path the
    // caller picks, and sway's IPC grants `exec` — a process outside shepherd's
    // supervision and outside the cgroup the firewall is attached to. On a
    // device the environment belongs to the kiosk user, so a flag it is.
    #[arg(long)]
    sway_ipc_alias: Option<PathBuf>,

    /// Leave sway's IPC socket reachable by every process at this uid, instead
    /// of unlinking it once shepherdd has connected.
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
    #[arg(long = "no-harden-sway-ipc")]
    no_harden_sway_ipc: bool,

    /// Accept a client on shepherdd's own management socket from any process
    /// at this uid, instead of only from the session shepherdd is part of.
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
    #[arg(long = "no-restrict-ipc-peers")]
    no_restrict_ipc_peers: bool,

    /// Trust the environment: honour `SHEPHERD_*_BIN`,
    /// `SHEPHERD_FIREWALL_HELPER` and `SHEPHERD_BROWSER_ROOT`, and search
    /// `$PATH` ahead of the compiled-in trusted directories.
    ///
    /// Off by default, because on a device the environment is not shepherd's to
    /// trust: GDM's PAM stack reads `~/.pam_environment`, a file the kiosk user
    /// owns, so every activity can choose what `$PATH` says (issue #144). A
    /// substituted `systemd-run` would run as a direct child of the daemon, in
    /// the daemon's own cgroup, which the management socket accepts as `Admin`.
    ///
    /// Separate from `--no-restrict-ipc-peers` although both are development
    /// opt-outs, because they are not the same risk and are not wanted at the
    /// same times. That one decides who may *drive* the daemon; this one
    /// decides which code the daemon *runs*. Only the e2e suite needs it — it
    /// stubs `flatpak`, `pkcheck` and `pkexec` on `$PATH` — so an ordinary dev
    /// session leaves it off and exercises the same resolution a device does.
    #[arg(long = "trust-environment")]
    trust_environment: bool,

    /// Keep policy and state in this user's home instead of asking the state
    /// custodian for them (issue #157).
    ///
    /// On a device the files an activity must not reach — `shepherdd.db`, and
    /// later `config.toml` and the BLE admin record — are owned by
    /// `shepherd-state` and served over a socket that admits only this
    /// session's cgroup. This flag keeps them where they used to be: in the
    /// home directory of the uid every activity runs as, where an activity can
    /// reset today's usage and rewrite the policy. Measured, not theoretical —
    /// `docs/ai/history/2026-08-29 005`.
    ///
    /// A third flag rather than a meaning bolted onto one of the two above,
    /// for the reason those two were split: they are different risks wanted at
    /// different times. One decides who may *drive* the daemon, one decides
    /// which code it *runs*, and this one decides whether its state is
    /// reachable by the software it supervises. `shepherd install sway-config`
    /// strips all three and refuses to finish if a strip did not take.
    #[arg(long = "no-state-custodian")]
    no_state_custodian: bool,
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
    /// Where policy and state ended up living (issue #157). Carried for the
    /// same reason, and decided at the same end of startup: the store is opened
    /// before there is anywhere to report to.
    state_protection: StateProtection,
    /// The custodian's files when the policy came from there, which decides
    /// both how it is watched (shepherdd cannot inotify a directory it cannot
    /// open) and where a reload reads from.
    policy_files: Option<Arc<dyn ProtectedFiles>>,
    /// Where the BLE admin record, unbond queue and reset sentinel live.
    /// Always present, unlike `policy_files` — see [`StateParts`].
    protected_files: Arc<dyn ProtectedFiles>,
    /// The connection the custodian watches to know this daemon is still
    /// supervising the session (issue #172).
    ///
    /// Held for its `Drop` as much as for [`Supervision::beat`]: closing it is
    /// how an orderly exit tells the watchdog, and a killed process closes it
    /// without being asked. `None` where there is no custodian to tell.
    supervision: Option<Supervision>,
    /// Whether anything outside the session would notice this daemon dying, and
    /// what to say if not. Carried for the same reason [`StateProtection`] is:
    /// it is settled before there is a diagnostics channel to report it on.
    session_guard: SessionGuard,
}

/// Whether the session survives this daemon being killed (issue #172).
///
/// Four outcomes rather than a bool, for the reason [`IpcPeerHardening`] has
/// three: "there is no custodian on this device" and "there is one and it
/// cannot end a session" look identical from here and mean entirely different
/// things to whoever is responsible for the device.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionGuard {
    /// No custodian answered, so nothing outside the session is watching. The
    /// device already says so through [`DiagnosticCode::StateNotProtected`];
    /// saying it twice would be two alarms for one fact.
    NoCustodian,
    /// The custodian is watching and can end the session.
    Armed,
    /// Watching, with something worth reporting — it could not check whether it
    /// is allowed to end a session, so it will find out when it tries.
    Caveat(String),
    /// Watching and unable to act, or not watching at all. The worst shape
    /// available, because it is the one that looks like protection.
    Inert(String),
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

/// Where shepherd's state ended up living, and why.
///
/// Three outcomes for the same reason [`IpcPeerHardening`] has three: "the
/// operator asked for the old behaviour" and "the protection was wanted and
/// could not be had" look identical from the database and mean opposite things
/// to whoever is responsible for the device.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StateProtection {
    /// Served by the custodian, at a uid no activity has.
    Custodian,
    /// Deliberately local (`--no-state-custodian`).
    OptedOut,
    /// Wanted, but not had; the store is a file in the home directory of the
    /// uid activities run as. Carries the reason.
    ///
    /// Only ever a device that has no custodian to reach: one whose custodian
    /// *is* installed and unreachable does not get here, because
    /// [`Service::unreachable_or_local`] refuses to start it.
    Degraded(String),
}

/// What [`StateSource::into_parts`] settles into: the store, the policy source
/// when it is the custodian's, and how protected that combination is.
type StateParts = (
    Arc<dyn Store>,
    // The policy handle: `Some` only when the custodian actually holds a
    // policy, because that decides where a *reload* reads from.
    Option<Arc<dyn ProtectedFiles>>,
    // Where the BLE admin record, unbond queue and reset sentinel live --
    // always something, custodian or local. Separate from the policy handle on
    // purpose: a custodian that holds the admin record but no policy still
    // serves the record, and gating one on the other would send the token back
    // to the home directory over an unrelated migration gap.
    Arc<dyn ProtectedFiles>,
    StateProtection,
);

/// Whatever has to stay alive for policy auto-reload to keep working.
///
/// Both arms are just ownership: a dropped `notify` watcher stops watching, and
/// a dropped [`shepherd_state_proto::ConfigWatch`] closes its connection.
enum PolicyWatch {
    // Both fields are held for their `Drop` and never read, which is the whole
    // job: a dropped watcher stops watching. Named rather than `_`-prefixed so
    // the variants still say what is keeping the watch alive.
    #[allow(dead_code)]
    Local(RecommendedWatcher),
    #[allow(dead_code)]
    Custodian(shepherd_state_proto::ConfigWatch),
    None,
}

/// Where this daemon's policy and state come from, decided once at startup.
///
/// Holds the connections rather than re-making them, and answers both questions
/// — the policy file and the database — from the same decision, so a device
/// cannot end up half protected.
enum StateSource {
    /// The custodian answered. Policy and database both come from it.
    Custodian {
        files: Arc<dyn ProtectedFiles>,
        store: Arc<RemoteStore>,
    },
    /// Deliberately local (`--no-state-custodian`), or the custodian could not
    /// be reached. `reason` is `None` for the first and the failure for the
    /// second — which is the difference between "the operator asked" and "the
    /// protection was wanted and could not be had".
    Local { reason: Option<String> },
}

impl StateSource {
    /// Whether the custodian answered *and* holds a policy.
    ///
    /// Both halves matter: a custodian with no policy file means the database
    /// is protected and the policy is not, which is the half that decides what
    /// a child may do.
    fn holds_policy(&self) -> bool {
        match self {
            StateSource::Custodian { files, .. } => {
                matches!(files.read(ProtectedFile::Config), Ok(Some(_)))
            }
            StateSource::Local { .. } => false,
        }
    }

    /// The policy, from wherever this device keeps it.
    ///
    /// A custodian that answers but holds no policy falls back to the file at
    /// `local` rather than refusing to start: a device whose migration has not
    /// run should boot with its old policy and say so, not present a child with
    /// a dead screen.
    fn load_policy(&self, local: &Path) -> Result<shepherd_config::Policy> {
        if let StateSource::Custodian { files, .. } = self
            && let Some(text) = files
                .read(ProtectedFile::Config)
                .context("reading the policy from the state custodian")?
        {
            return shepherd_config::parse_config(&text)
                .context("parsing the policy the state custodian returned");
        }
        load_config(local).with_context(|| format!("Failed to load config from {local:?}"))
    }

    /// Where [`Self::load_policy`] read from, for the log line.
    fn policy_source(&self, local: &Path) -> String {
        if self.holds_policy() {
            "the state custodian".to_string()
        } else {
            local.display().to_string()
        }
    }

    /// Settle into a store and a policy source, reporting how protected the
    /// result is.
    ///
    /// The files handle comes back rather than being dropped because the policy
    /// is read again on every reload, and from the same place it was read the
    /// first time — a device that reloaded from a different source than it
    /// booted from would be the worst kind of surprise.
    fn into_parts(self, data_dir: &Path) -> Result<StateParts> {
        let holds_policy = self.holds_policy();
        match self {
            StateSource::Custodian { store, files } => {
                let protection = if holds_policy {
                    StateProtection::Custodian
                } else {
                    // Database protected, policy not. Degraded rather than
                    // Custodian, because a policy an activity can rewrite is
                    // exactly what this issue is about.
                    StateProtection::Degraded(
                        "the custodian holds no policy file, so the policy is still read from \
                         this user's home where every activity can rewrite it; migrate it with \
                         `shepherd install state --user <user>`, or `shepherd-admin setup-user \
                         <user>` on a packaged system"
                            .to_string(),
                    )
                };
                // `None` when the custodian holds no policy: the policy is
                // then a local file, and the reload has to read it from where
                // it was actually read at boot.
                let policy_files = holds_policy.then_some(Arc::clone(&files));
                Ok((store, policy_files, files, protection))
            }
            StateSource::Local { reason } => {
                let store = Service::open_local_store(data_dir)?;
                let protection = match reason {
                    Some(reason) => StateProtection::Degraded(reason),
                    None => StateProtection::OptedOut,
                };
                // The same `LocalProtectedFiles` the custodian uses on its own
                // side, rooted at this user's data directory: one
                // implementation, so "protected" and "not protected" cannot
                // drift into two behaviours.
                let files: Arc<dyn ProtectedFiles> =
                    Arc::new(LocalProtectedFiles::new(data_dir.to_path_buf()));
                Ok((store, None, files, protection))
            }
        }
    }
}

impl Service {
    /// Connect to the custodian, or decide not to.
    ///
    /// **The choice is made once, here, and never revisited.** That is a
    /// security property, not tidiness: a daemon that could fall back
    /// *mid-session* would be one an activity could push into falling back, by
    /// making the custodian unreachable — handing back everything this is for.
    /// After startup a broken connection is an error the caller sees, and the
    /// client reconnects; it never becomes a local file.
    ///
    /// Falling back at all follows the trade #144 already made twice: an
    /// unprotected kiosk beats a child staring at a dead screen. What makes
    /// that honest is that it is never silent — see
    /// [`Self::state_not_protected_diagnostic`].
    fn open_state(args: &Args) -> Result<StateSource> {
        if args.no_state_custodian {
            warn!(
                "Policy and state are in this user's home; every activity runs as this uid \
                 and can read and rewrite them (issue #157)"
            );
            return Ok(StateSource::Local { reason: None });
        }

        // Whose state to ask for is *this process's* user, not a name from the
        // environment: on a device the environment belongs to the kiosk user,
        // and so to every activity (issue #144, finding 1).
        let user = match Self::this_user() {
            Some(user) => user,
            None => {
                let reason =
                    "this process's own uid has no user entry, so there is no custodian to ask"
                        .to_string();
                warn!(%reason, "Falling back to local policy and state");
                // No name to look a state directory up by, so this cannot be
                // told apart from a device that never had a custodian.
                return Ok(StateSource::Local {
                    reason: Some(reason),
                });
            }
        };

        // Retry briefly before giving up. The custodian is socket-activated, so
        // the first connection is also what starts it, and it waits for the
        // graphical session before it answers — a few seconds here covers the
        // boot race rather than reporting one as a failure.
        let store = match Self::connect_with_retries(&user) {
            Ok(store) => store,
            Err(e) => return Self::unreachable_or_local(&user, e),
        };
        let files = match RemoteFiles::connect(&user) {
            Ok(files) => files,
            Err(e) => return Self::unreachable_or_local(&user, e),
        };

        info!(
            socket = %store.socket_path().display(),
            "Policy and state served by the custodian; not reachable by activities"
        );
        Ok(StateSource::Custodian {
            files: Arc::new(files),
            store: Arc::new(store),
        })
    }

    /// This process's own user name, which is whose state the custodian serves.
    ///
    /// Read from the uid rather than from `$USER` for the reason the whole of
    /// #144 turns on: on a device the environment belongs to the kiosk user, so
    /// it belongs to every activity too.
    fn this_user() -> Option<String> {
        nix::unistd::User::from_uid(nix::unistd::getuid())
            .ok()
            .flatten()
            .map(|user| user.name)
    }

    /// Whether the custodian holds state for `user`, asked of the filesystem
    /// rather than of the connection that just failed (issue #157).
    ///
    /// `/var/lib/shepherdd/state/<user>/` is `0700` and owned by
    /// `shepherd-state`, so nothing here can read it — but its parents are
    /// root-owned and world-executable, so this `stat` is allowed, and an
    /// activity can neither create the directory nor remove it.
    ///
    /// It answers the one question a failed connection cannot: whether this is
    /// a device that never had a custodian, or one whose protection broke this
    /// boot. Both fall back, but only the second must stop presenting itself as
    /// unclaimed — after migration the fallback location is empty, so an absent
    /// `admin.toml` there means "looking in the wrong place", not "nobody has
    /// claimed this device".
    fn custodian_holds_state_for(user: &str) -> bool {
        shepherd_state_proto::state_dir(user).is_dir()
    }

    /// What to do when the custodian did not answer.
    ///
    /// Two situations wear the same error, and they want opposite responses.
    ///
    /// A device that never had a custodian — a packaged install where
    /// `setup-user` has not run, or one deliberately left without — has its
    /// state in the kiosk user's home, where it always was. Nothing has moved,
    /// nothing is missing, and refusing to start would be refusing over a
    /// protection this device was never given. It boots, and says so.
    ///
    /// A device that *has* one and cannot reach it is a different thing
    /// entirely. Its state was moved: the database and the admin record are in
    /// a directory this process cannot read, and the home directory holds a
    /// signpost saying so. Carrying on would mean opening a fresh empty
    /// database, presenting a claimed device as unclaimed, and offering a
    /// launcher with no activities on it — which reads to a child exactly like
    /// bedtime, and to an adult like the device is merely slow. So it exits,
    /// and sway's fallback ends the session.
    ///
    /// Landing back at the greeter is a worse-looking failure and a better one:
    /// it says *something is wrong* rather than impersonating a working device
    /// with nothing configured. The message is written for the journal, because
    /// that is where whoever hits this will be looking.
    fn unreachable_or_local(user: &str, error: impl std::fmt::Display) -> Result<StateSource> {
        let reason = format!("{error}");
        if Self::custodian_holds_state_for(user) {
            anyhow::bail!(
                "the state custodian is installed for {user} and did not answer ({reason}); \
                 refusing to start, because this device's policy, usage history and BLE admin \
                 record are in /var/lib/shepherdd, not where an unprotected run would look. \
                 Check `systemctl status shepherd-stated@{user}.service` and \
                 `journalctl -u shepherd-stated@{user}.service`"
            );
        }
        warn!(
            error = %reason,
            "No state custodian for this user; policy and state stay in the home directory"
        );
        Ok(StateSource::Local {
            reason: Some(reason),
        })
    }

    /// Connect to the custodian, retrying a transient failure.
    ///
    /// Bounded and short: this runs before the session is up, so every second
    /// here is a second the child waits at a blank screen. Long enough to cover
    /// the custodian starting under socket activation and resolving the session
    /// logind may only just have registered.
    fn connect_with_retries(user: &str) -> Result<RemoteStore, shepherd_store::StoreError> {
        const ATTEMPTS: u32 = 5;
        const GAP: std::time::Duration = std::time::Duration::from_millis(600);

        let mut last = None;
        for attempt in 1..=ATTEMPTS {
            match RemoteStore::connect(user) {
                Ok(store) => return Ok(store),
                Err(e) => {
                    if attempt < ATTEMPTS {
                        debug!(attempt, error = %e, "The state custodian is not answering yet");
                        std::thread::sleep(GAP);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.expect("at least one attempt"))
    }

    /// Say something when a protected device still has the old database in the
    /// user's home.
    ///
    /// It gets there two ways, and both are worth an operator knowing about: an
    /// upgrade where migration did not run, or a boot where the custodian was
    /// unreachable and the daemon fell back — accruing usage into a file that
    /// is now stale and, unlike the live one, readable and writable by every
    /// activity.
    ///
    /// Deliberately not deleted here. The daemon cannot know whether that file
    /// holds a day of a child's usage that nobody has looked at yet, and
    /// discarding a device's history to tidy up is the same mistake `uninstall`
    /// declines to make. Say where it is and let a person decide.
    fn warn_about_a_superseded_local_store(data_dir: &Path) {
        let stale = data_dir.join("shepherdd.db");
        if stale.exists() {
            warn!(
                path = %stale.display(),
                "State is served by the custodian, but an old database is still in this \
                 user's home. Nothing reads it, and every activity can read and rewrite it. \
                 The protected copy is the live one, so `shepherd install state` will not \
                 migrate over it: if this file holds usage worth keeping, move it \
                 deliberately, then remove it (issue #157)"
            );
        }
    }

    /// Start watching the policy, returning whatever has to stay alive for the
    /// watch to keep running.
    ///
    /// Auto-reload is best-effort on both paths: a device whose watch could not
    /// be established still runs, and still reloads on an explicit
    /// `reload_config`. It says so rather than pretending.
    fn watch_policy(
        custodial: bool,
        config_path: &Path,
        tx: tokio::sync::mpsc::UnboundedSender<()>,
    ) -> PolicyWatch {
        if custodial {
            let user = match nix::unistd::User::from_uid(nix::unistd::getuid()) {
                Ok(Some(user)) => user.name,
                _ => {
                    warn!("Cannot name this user, so the policy watch is disabled");
                    return PolicyWatch::None;
                }
            };
            let notify_tx = tx.clone();
            return match shepherd_state_proto::ConfigWatch::start(
                &user,
                move || {
                    let _ = notify_tx.send(());
                },
                |e| {
                    warn!(error = %e, "The policy watch ended; auto-reload is off until restart");
                },
            ) {
                Ok(watch) => {
                    info!("Watching the custodian's policy file for changes");
                    PolicyWatch::Custodian(watch)
                }
                Err(e) => {
                    warn!(error = %e, "Could not watch the custodian's policy, auto-reload disabled");
                    PolicyWatch::None
                }
            };
        }

        // Match on the *file name*, not the whole path. The watch is on one
        // directory and is not recursive, so a name is already unique within
        // it — and comparing whole paths meant comparing the spelling
        // `--config` was given against the one `notify` reports. A relative
        // `-c ./config.example.toml`, which is how every dev entry point
        // starts the daemon, never matched, so auto-reload was silently off
        // for the entire development stack while the log said "Watching config
        // file for changes".
        let Some(watched_name) = config_path.file_name().map(|n| n.to_os_string()) else {
            warn!("Config path names no file, so auto-reload is disabled");
            return PolicyWatch::None;
        };
        let watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result
                    && policy_event_matches(&event, &watched_name)
                {
                    let _ = tx.send(());
                }
            },
            notify::Config::default(),
        );
        // `Path::parent` of a bare `config.toml` is `Some("")`, which is not a
        // directory anything can watch; that spelling means the working
        // directory.
        let dir = match config_path.parent() {
            Some(p) if p.as_os_str().is_empty() => Some(Path::new(".")),
            other => other,
        };
        match (watcher, dir) {
            (Ok(mut watcher), Some(dir)) => match watcher.watch(dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    info!(config_path = %config_path.display(), "Watching config file for changes");
                    PolicyWatch::Local(watcher)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to watch config directory, auto-reload disabled");
                    PolicyWatch::None
                }
            },
            (Ok(_), None) => {
                warn!("Config path has no parent directory, auto-reload disabled");
                PolicyWatch::None
            }
            (Err(e), _) => {
                warn!(error = %e, "Failed to create config watcher, auto-reload disabled");
                PolicyWatch::None
            }
        }
    }

    /// The pre-#157 store: a SQLite file in the data directory.
    fn open_local_store(data_dir: &Path) -> Result<Arc<dyn Store>> {
        let db_path = data_dir.join("shepherdd.db");
        let store = SqliteStore::open(&db_path)
            .with_context(|| format!("Failed to open database {:?}", db_path))?;
        info!(db_path = %db_path.display(), "Store initialized (local)");
        Ok(Arc::new(store))
    }

    /// The web management interface is configured and not serving (issue
    /// #182).
    ///
    /// A `Warning` rather than a `Critical`: the companion app reaches this
    /// device over BLE and is unaffected, so this is one path lost rather than
    /// a device lost. It is still the path somebody would reach for when
    /// something else has gone wrong, which is why it is said out loud at all.
    fn management_api_unavailable_diagnostic(error: &anyhow::Error) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::ManagementApiUnavailable,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "The web management interface is configured but is not serving, so the \
                 address a browser would open refuses the connection — {error}"
            ),
            remedy: Some(
                "Check `service.management_api.bind` names an address this device actually \
                 has, and that nothing else holds the port. Raise \
                 `service.management_api.bind_retry_seconds` if the address belongs to an \
                 interface that comes up late, such as a VPN."
                    .to_string(),
            ),
            since: shepherd_util::now(),
        }
    }

    /// The administrator-facing form of "this device's state is reachable by
    /// the software it supervises".
    ///
    /// `Critical` and `Service`-scoped for the same reasons
    /// [`Self::ipc_not_hardened_diagnostic`] is: it is true of the device
    /// rather than of one activity, and a device shipped this way has no
    /// working boundary around its quota, usage or audit trail.
    fn state_not_protected_diagnostic(reason: &str) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::StateNotProtected,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Critical,
            message: format!(
                "shepherd's usage, quota and audit state is a file owned by the user every \
                 activity runs as, so an activity can reset today's usage or grant itself \
                 time — {reason}"
            ),
            remedy: Some(
                "Install and enable the state custodian (`shepherd install state --user \
                 <user>`, or `shepherd-admin setup-user <user>` on a packaged system), and \
                 do not pass --no-state-custodian on a device."
                    .to_string(),
            ),
            since: shepherd_util::now(),
        }
    }

    /// Report a degraded state store once diagnostics exist.
    ///
    /// Separate from choosing the store for the same ordering reason
    /// [`Self::degraded_ipc_hardening_is_reported`] is separate: the store is
    /// built before there is anywhere to report to.
    fn degraded_state_protection_is_reported(
        state: &StateProtection,
        diagnostics: &dyn DiagnosticSink,
    ) {
        if let StateProtection::Degraded(reason) = state {
            diagnostics.raise(Self::state_not_protected_diagnostic(reason));
        }
    }

    /// Open the watchdog connection, and settle what it is worth.
    ///
    /// Called only where the custodian answered: it is the one process outside
    /// the session, at a uid nothing inside it can signal, and without it there
    /// is nothing to hold the other end of this.
    ///
    /// Never fatal. A device that could not open this connection is exactly as
    /// supervised as every device was before #172, and refusing to start over
    /// it would trade a defence-in-depth failure for a child staring at a
    /// greeter.
    fn start_supervision(user: &str) -> (Option<Supervision>, SessionGuard) {
        match Supervision::start(user) {
            Ok((supervision, reply)) => {
                let guard = match (reply.armed, reply.reason) {
                    (true, None) => {
                        info!(
                            deadline_secs = reply.deadline.as_secs(),
                            "The custodian will end this session if this daemon stops \
                             supervising it"
                        );
                        SessionGuard::Armed
                    }
                    (true, Some(caveat)) => {
                        warn!(%caveat, "The session watchdog is armed with a caveat");
                        SessionGuard::Caveat(caveat)
                    }
                    (false, reason) => {
                        let reason = reason.unwrap_or_else(|| {
                            "the custodian did not say why it cannot end the session".to_string()
                        });
                        error!(%reason, "The session watchdog cannot end the session");
                        SessionGuard::Inert(reason)
                    }
                };
                (Some(supervision), guard)
            }
            Err(e) => {
                error!(error = %e, "Could not open the supervision channel");
                (
                    None,
                    SessionGuard::Inert(format!(
                        "the supervision channel to the custodian could not be opened ({e})"
                    )),
                )
            }
        }
    }

    /// What to say about a session nothing is guarding.
    fn session_not_guarded_diagnostic(guard: &SessionGuard) -> Option<Diagnostic> {
        let (severity, reason) = match guard {
            // Already reported, once, as `StateNotProtected`.
            SessionGuard::NoCustodian | SessionGuard::Armed => return None,
            SessionGuard::Caveat(reason) => (DiagnosticSeverity::Warning, reason),
            SessionGuard::Inert(reason) => (DiagnosticSeverity::Critical, reason),
        };
        Some(Diagnostic {
            code: DiagnosticCode::SessionNotGuarded,
            subject: DiagnosticSubject::Service,
            severity,
            message: format!(
                "an activity runs as this daemon's own uid and can kill or stop it; if it \
                 does, nothing outside the session will end the session — {reason}"
            ),
            remedy: Some(
                "Install the polkit rule that lets the state custodian end a session \
                 (/etc/polkit-1/rules.d/50-shepherd-session-guard.rules, shipped with the \
                 package and installed by `shepherd install state`), then restart the \
                 session."
                    .to_string(),
            ),
            since: shepherd_util::now(),
        })
    }

    /// Report an unguarded session once diagnostics exist, for the same
    /// ordering reason [`Self::degraded_state_protection_is_reported`] is
    /// separate: this is settled at startup, before there is anywhere to say it.
    fn unguarded_session_is_reported(guard: &SessionGuard, diagnostics: &dyn DiagnosticSink) {
        if let Some(diagnostic) = Self::session_not_guarded_diagnostic(guard) {
            diagnostics.raise(diagnostic);
        }
    }

    async fn new(args: &Args) -> Result<Self> {
        // Where policy and state come from, decided once and for both. Splitting
        // the decision would let a device end up with a protected database and a
        // policy any activity can rewrite, which is the half that matters most:
        // the policy is what says how long a child may play.
        let state = Self::open_state(args)?;

        let policy = state.load_policy(&args.config)?;
        info!(
            entry_count = policy.entries.len(),
            source = state.policy_source(&args.config),
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
        //
        // Asked before `into_parts` consumes it, and asked of the *source*
        // rather than of the protection: a custodian that answers but holds no
        // policy yet is `Degraded`, and it is still the process that can end
        // this session if this daemon stops supervising it (issue #172).
        let custodian_answered = matches!(state, StateSource::Custodian { .. });
        let (store, policy_files, protected_files, state_protection) =
            state.into_parts(&data_dir)?;
        let (supervision, session_guard) = match (custodian_answered, Self::this_user()) {
            (true, Some(user)) => Self::start_supervision(&user),
            (true, None) => (
                None,
                SessionGuard::Inert(
                    "this process's own uid has no user entry, so the custodian serving it \
                     cannot be named"
                        .to_string(),
                ),
            ),
            (false, _) => (None, SessionGuard::NoCustodian),
        };
        if state_protection == StateProtection::Custodian {
            Self::warn_about_a_superseded_local_store(&data_dir);
        }

        // Log service start
        store.append_audit(AuditEvent::new(AuditEventType::ServiceStarted))?;

        // Decide whether the environment may name binaries, before anything is
        // resolved or spawned (issue #144). On a device it may not: GDM's PAM
        // stack reads `~/.pam_environment`, so the kiosk user — and therefore
        // every activity — chooses the session's environment.
        //
        // Its own flag rather than a second meaning for
        // `--no-restrict-ipc-peers`: that one decides who may drive the daemon,
        // this one decides which code the daemon runs, and only the e2e suite
        // wants the second. Keeping them apart is what lets an ordinary dev
        // session resolve binaries the way a device does.
        shepherd_host_linux::helpers::set_trust_environment(args.trust_environment);

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

        // Apply `[service.waydroid]` config before any preboot.
        host.configure_waydroid(
            engine.policy().service.waydroid.multi_window,
            engine.policy().service.waydroid.suspend_when_idle,
            engine.policy().service.waydroid.boot_ready_timeout,
            match engine.policy().service.waydroid.lock_mode {
                LockMode::Off => WaydroidLockMode::Off,
                LockMode::Statusbar => WaydroidLockMode::Statusbar,
                LockMode::Locktask => WaydroidLockMode::Locktask,
            },
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
            state_protection,
            policy_files,
            protected_files,
            supervision,
            session_guard,
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
        let state_protection = self.state_protection.clone();
        let session_guard = self.session_guard.clone();
        // Moved out rather than cloned: what keeps the watchdog quiet is this
        // value existing, so the loop below has to be the thing that owns it.
        // When `run` returns, it drops, the connection closes, and the custodian
        // ends the session — which is what an exiting shepherdd wants anyway.
        let supervision = self.supervision.take();
        let policy_files = self.policy_files.clone();
        let protected_files = Arc::clone(&self.protected_files);

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

        // Pre-boot Android if configured. Default: preboot iff any Android
        // entry exists; `[service.waydroid] preboot` overrides either way.
        let has_android = self
            .engine
            .policy()
            .entries
            .iter()
            .any(|e| matches!(e.kind, EntryKind::Android { .. }));
        if has_android {
            // Hide Android activities until the Waydroid session is up, mirroring
            // Steam (issue #76): a launch hard-errors unless the session is
            // running. Seeded here so the very first served snapshot already gates
            // Android; the host's readiness watcher flips it as the session comes
            // up / goes down (see HostEvent::KindReadinessChanged). Runs
            // independent of pre-boot so a manually-started session un-gates too.
            self.engine.set_kind_readiness(EntryKindTag::Android, false);
            self.host.spawn_waydroid_readiness_watcher();
        }
        let should_preboot =
            should_preboot_waydroid(self.engine.policy().service.waydroid.preboot, has_android);
        if should_preboot {
            info!("Pre-booting Android (Waydroid) in background");
            self.host.preboot_waydroid();
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

        // HUD placement (issue #171). Same shape and the same reasons as the
        // hidpi manager above: it holds both subscriber channels so it can
        // announce a change of edge to IPC and SSE alike, and the global
        // setting it falls back to is fixed at load time.
        let hud_layout = {
            let global = engine.lock().await.policy().hud_orientation;
            Arc::new(HudLayout::new(global, ipc_ref.clone(), event_tx.clone()))
        };

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

        // The web listener's real state (issue #182), created before the
        // service that reads it and before the server that writes it. A
        // configured listener starts out `Binding`: the address it wants may
        // not exist yet, which is what `bind_retry_seconds` is for.
        let web_listener = match &management_api_config {
            Some(cfg) => {
                WebListenerHandle::configured(SocketAddr::new(cfg.bind, cfg.port), cfg.tls.is_tls())
            }
            None => WebListenerHandle::disabled(),
        };

        // The web UI's credential store (issue #156). Built whenever the HTTP
        // API is, and *only* then: it is the thing that makes an unclaimed
        // device closed rather than open, so a management API without one
        // would be the fail-open state this replaced. A store that cannot be
        // loaded takes the API down with it rather than falling back to no
        // authentication.
        let web_auth: Option<Arc<shepherd_management::WebAuth>> = match &management_api_config {
            Some(cfg) => {
                let policy = shepherd_management::WebAuthPolicy {
                    session_idle: cfg.auth.session_idle,
                    session_max_age: cfg.auth.session_max_age,
                    lockout_after: cfg.auth.lockout_after,
                    lockout: cfg.auth.lockout,
                };
                match shepherd_management::WebAuth::load(Arc::clone(&protected_files), policy) {
                    Ok(auth) => Some(Arc::new(auth)),
                    Err(e) => {
                        error!(error = %e, "Could not open the web management credential store");
                        return Err(anyhow::anyhow!("web management credential store: {e}"));
                    }
                }
            }
            None => None,
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
        // Media refresh (issue #165): the management API's end of the nudge
        // that makes the prefetcher re-fetch now instead of at its next tick.
        // Capacity one — the request carries nothing, so a second press while
        // the first is still queued is asking for the same sweep, and dropping
        // it is the correct answer rather than a queue of identical work.
        let (media_refresh_tx, media_refresh_rx) = tokio::sync::mpsc::channel::<()>(1);

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
                policy_files: policy_files.clone(),
                media_refresh_tx: Some(media_refresh_tx),
                // Plugged in below, once the BLE server has a claim machine.
                admins: Default::default(),
                shutdown_tx: shutdown_tx.clone(),
                hidpi: hidpi.clone() as Arc<dyn HidpiController>,
                hud_layout: hud_layout.clone() as Arc<dyn HudLayoutController>,
                display: display_svc.clone(),
                last_audio_state: Arc::new(tokio::sync::Mutex::new(None)),
                diagnostics: Some(
                    Arc::new(diagnostic_publisher.clone()) as Arc<dyn shepherd_api::DiagnosticSink>
                ),
                network: Some(Arc::new(LinuxNetworkInfo::new()) as Arc<dyn NetworkInfoProvider>),
                web_listener: web_listener.clone(),
                web_auth: web_auth.clone(),
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
        let BleManagement {
            handle: ble_handle,
            authority: admin_authority,
            roster: admin_roster,
        } = match ble_management_config {
            Some(ble_cfg) => {
                let bsc = BleServerConfig {
                    device_name: ble_cfg.device_name,
                    firmware_version: env!("CARGO_PKG_VERSION").to_string(),
                    // The admin record carries the minted HTTP token, so when
                    // the custodian is holding shepherd's state it holds this
                    // too — otherwise the credential sits at the uid every
                    // activity runs as (issue #157).
                    //
                    // Not `policy_files`: that is `None` when the custodian
                    // holds no *policy*, and the admin record is a different
                    // file the custodian may well be serving.
                    files: Arc::clone(&protected_files),
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
                        // One claim machine, two roles: it verifies the
                        // bearer tokens the HTTP middleware is presented with,
                        // and it is the administrator roster both transports
                        // manage (issue #149).
                        let claim = server.claim_machine();
                        let authority =
                            claim.clone() as Arc<dyn shepherd_management::AdminAuthority>;
                        let roster = claim as Arc<dyn shepherd_management::AdminRoster>;
                        let rx = shutdown_rx.clone();
                        let handle = tokio::spawn(async move {
                            if let Err(e) = server.run(rx).await {
                                error!(error = %e, "BLE management server error");
                            }
                        });
                        BleManagement {
                            handle: Some(handle),
                            authority: Some(authority),
                            roster: Some(roster),
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "BLE management server failed to initialize");
                        BleManagement::default()
                    }
                }
            }
            None => BleManagement::default(),
        };

        // The store learns about the companion here rather than at
        // construction: the claim machine does not exist until the BLE server
        // is built, and the browser's login page needs to know whether the
        // "approve on my phone" button leads anywhere.
        if let Some(web) = &web_auth {
            web.set_companion(admin_authority.clone());
        }
        // Same handover, for the same reason: this is what lets a browser list
        // the administrators and approve a second phone (issue #149).
        svc_concrete.set_admin_roster(admin_roster.clone());

        // Zipped, not two `if let`s: the store is built above from exactly this
        // `Option`, so pairing them here is what makes "an API always has a
        // credential store" a thing the compiler carries rather than a thing
        // two nearby blocks happen to agree on.
        let http_handle = match management_api_config.zip(web_auth.clone()) {
            Some((api_cfg, web)) => {
                announce_web_auth_state(&web, &api_cfg);
                let http_state = HttpAppState { svc: svc.clone() };
                let http_server = HttpServer::new(http_state, api_cfg)
                    .with_admin_authority(admin_authority)
                    .with_listener_status(web_listener.clone())
                    .with_web_auth(web)
                    .with_protected_files(Arc::clone(&protected_files))
                    .with_hostnames(local_hostnames());
                let http_shutdown_rx = shutdown_rx.clone();
                let listener_status = web_listener.clone();
                let publisher = diagnostic_publisher.clone();
                Some(tokio::spawn(async move {
                    if let Err(e) = http_server.run(http_shutdown_rx).await {
                        error!(error = %e, "HTTP management API error");
                        // Until #182 this was the whole report: one line in a
                        // log on a device whose web interface is precisely how
                        // somebody would have read it. Say it where a parent
                        // can see it, on the phone if nowhere else.
                        listener_status.set_failed(&e);
                        publisher.raise(Self::management_api_unavailable_diagnostic(&e));
                    }
                }))
            }
            None => None,
        };

        // The web credential store's housekeeping (issue #156), and the
        // on-screen setup code that goes with it.
        //
        // One task for both because they are the same question asked on the
        // same clock: is this device set up, and are its sessions still alive.
        // The card is shown while there is a code to show and torn down the
        // moment a password exists — the parent who just finished setting one
        // should not have to look at their setup code any more.
        if let Some(web) = web_auth.clone() {
            let mut sweep_shutdown = shutdown_rx.clone();
            let svc_for_setup = svc.clone();
            tokio::spawn(async move {
                let mut card: Option<pairing_display::SetupCodeDisplay> = None;
                // What the card on screen is currently saying, so a tick that
                // changes nothing does not restart the subprocess and flash the
                // card at whoever is reading it.
                let mut showing: Option<SetupCardContent> = None;
                let mut ticker = tokio::time::interval(SETUP_CARD_POLL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut since_sweep = Duration::ZERO;
                loop {
                    tokio::select! {
                        _ = ticker.tick() => {
                            // The sweep is expiry housekeeping and wants the
                            // slower clock; the card wants the faster one,
                            // because at boot it goes up before the listener
                            // has bound and should stop saying "port 8080 on
                            // this device" as soon as it can say an address.
                            since_sweep += SETUP_CARD_POLL;
                            if since_sweep >= WEB_AUTH_SWEEP {
                                since_sweep = Duration::ZERO;
                                web.sweep();
                            }
                            let wanted = match web.enrolment_code() {
                                // Only asked for while a code exists: this is a
                                // NetworkManager round trip, and a device that
                                // finished setup months ago has no use for one.
                                Some(code) => {
                                    let status = svc_for_setup.network_status().await;
                                    Some(SetupCardContent {
                                        code,
                                        urls: status.management_urls,
                                        port: status.management_api.port,
                                    })
                                }
                                None => None,
                            };
                            if wanted != showing {
                                // Torn down before the replacement goes up:
                                // two overlays anchored to the same corner
                                // would stack rather than replace.
                                drop(card.take());
                                card = wanted.as_ref().map(|c| {
                                    pairing_display::SetupCodeDisplay::show(
                                        &c.code, &c.urls, c.port,
                                    )
                                });
                                showing = wanted;
                            }
                        }
                        _ = sweep_shutdown.changed() => {
                            if *sweep_shutdown.borrow() {
                                break;
                            }
                        }
                    }
                }
                // Explicit: the card is a child process, and shutdown is the
                // one path where leaving it to the end of scope would be easy
                // to lose in a later refactor.
                drop(card);
            });
        }

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
            tokio::spawn(async move {
                prefetcher
                    .run(engine_for_prefetch, events, media_refresh_rx, publisher)
                    .await
            });
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
        Self::degraded_state_protection_is_reported(&state_protection, &diagnostic_publisher);
        Self::unguarded_session_is_reported(&session_guard, &diagnostic_publisher);

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
        // Watch the policy for changes, from whichever side owns it.
        //
        // When the custodian holds it, shepherdd cannot use inotify: it cannot
        // watch a directory it cannot open. The custodian watches instead and
        // pushes on a second connection, which is the same behaviour by a
        // different route — and a strictly better one, because the file being
        // watched is then one no activity can write.
        let _config_watcher =
            Self::watch_policy(policy_files.is_some(), &config_path, config_change_tx);

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
                    // The watchdog's heartbeat rides this loop rather than a
                    // timer of its own (issue #172). A beat sent by an
                    // independent task would attest that *a thread* is alive,
                    // which is not the property anyone wants: this is the loop
                    // that decides whether a child's time is up, and it is the
                    // one whose silence has to end the session.
                    if let Some(supervision) = &supervision {
                        supervision.beat();
                    }

                    let now_mono = MonotonicInstant::now();
                    let now = shepherd_util::now();

                    let events = {
                        let mut engine = engine.lock().await;
                        engine.tick(now_mono, now)
                    };

                    for event in events {
                        Self::handle_core_event(&engine, &host, &ipc_ref, &event_tx, &hidpi, &hud_layout, event, now_mono, now).await;
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
                    Self::handle_host_event(&engine, &ipc_ref, &event_tx, &hidpi, &hud_layout, host_event).await;
                }

                // Resumed from suspend - reconcile the running session with the
                // wall clock (issue #155) and push a fresh state snapshot so
                // clients can drop the suspend cover with up-to-date content.
                Some(()) = resume_rx.recv() => {
                    let now_mono = MonotonicInstant::now();
                    let now = shepherd_util::now();

                    let (events, state) = {
                        let mut engine = engine.lock().await;
                        let events = engine.notify_resumed(now, now_mono);
                        (events, engine.get_state())
                    };

                    // Snapshot first, warning second. Clients rebuild their
                    // countdown from the snapshot, so a warning delivered
                    // before it would be overwritten by the state that follows.
                    Self::broadcast(&ipc_ref, &event_tx, Event::new(EventPayload::StateChanged(state)));

                    for event in events {
                        Self::handle_core_event(&engine, &host, &ipc_ref, &event_tx, &hidpi, &hud_layout, event, now_mono, now).await;
                    }
                }

                // Config file changed on disk
                Some(()) = config_change_rx.recv() => {
                    // Drain any additional buffered events to debounce rapid saves
                    while config_change_rx.try_recv().is_ok() {}
                    Self::handle_config_reload(
                        &engine,
                        &ipc_ref,
                        &event_tx,
                        &config_path,
                        policy_files.as_ref(),
                    )
                    .await;
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
        // active for the session we just stopped, and the HUD's edge with
        // them. host.logout() below tears down sway anyway, but this keeps us
        // tidy if logout fails.
        hidpi.restore().await;
        hud_layout.restore().await;

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
        policy_files: Option<&Arc<dyn ProtectedFiles>>,
    ) {
        // Read from wherever the policy was read at boot. Reloading from a
        // different source than the one that started the session would mean a
        // device silently changing which file decides what a child may do.
        let loaded = match policy_files {
            Some(files) => files
                .read(ProtectedFile::Config)
                .map_err(shepherd_config::ConfigError::ReadError)
                .and_then(|text| match text {
                    Some(text) => shepherd_config::parse_config(&text),
                    None => Err(shepherd_config::ConfigError::ReadError(
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "the state custodian holds no policy file",
                        ),
                    )),
                }),
            None => load_config(config_path),
        };
        match loaded {
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
                    source = match policy_files {
                        // Naming the home path here would be wrong and
                        // confusing in exactly the case that matters: the
                        // custodian's copy is what was read.
                        Some(_) => "the state custodian".to_string(),
                        None => config_path.display().to_string(),
                    },
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
        hud_layout: &Arc<HudLayout>,
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
                can_turn_pages,
                kind_tag,
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
                        can_turn_pages: *can_turn_pages,
                        kind_tag: *kind_tag,
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
                // was in effect, and the HUD's edge if the activity moved it
                // (both no-ops otherwise).
                hidpi.restore().await;
                hud_layout.restore().await;

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
            CoreEvent::LockChanged { .. } => {
                // Announced by the management service, which pairs it with a
                // fresh snapshot (see `DefaultManagementService::set_locked`).
                // The arm exists so a future producer inside the engine is a
                // compile error rather than a silently unannounced lock.
            }
            CoreEvent::AdminModeChanged { .. } => {
                // Announced by the management service itself, which already
                // holds the transition and pairs it with a fresh snapshot (see
                // `DefaultManagementService::broadcast_admin_mode`). Rebroadcast
                // here would double every transition; the arm exists so that a
                // future producer of this event inside the engine is a compile
                // error here rather than a silently unannounced mode change.
            }
        }
    }

    async fn handle_host_event(
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        hidpi: &Arc<XwaylandHidpi>,
        hud_layout: &Arc<HudLayout>,
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
                    // XWayland HiDPI workaround was in effect for this session,
                    // and the HUD's edge if the activity moved it.
                    hidpi.restore().await;
                    hud_layout.restore().await;

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
                    hud_layout.restore().await;
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
                // becomes something an administrator can see (issue #143) and
                // something the audit log keeps (issue #144's acceptance asks
                // for the rejection to be audited, and an acceptance was being
                // recorded while a refusal was not).
                let _ = store.append_audit(AuditEvent::new(AuditEventType::ClientRejected {
                    reason: rejection.reason.clone(),
                    peer_cgroup: rejection.peer_cgroup.clone(),
                    peer_pid: rejection.peer_pid,
                }));
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

/// DNS names to put in a generated TLS certificate.
///
/// The machine's hostname, plus its `.local` form, plus `localhost`. Not an
/// exhaustive answer — a device reached through a name only the router knows
/// will still mismatch — but it covers the two ways a parent actually types a
/// device's address, and the `files` TLS mode is the answer for anything else.
fn local_hostnames() -> Vec<String> {
    let mut names = vec!["localhost".to_string()];
    if let Ok(hostname) = std::fs::read_to_string("/etc/hostname") {
        let hostname = hostname.trim().to_string();
        if !hostname.is_empty() {
            names.push(format!("{hostname}.local"));
            names.push(hostname);
        }
    }
    names.dedup();
    names
}

/// Say, once at startup, what state web management authentication is in.
///
/// An unconfigured device puts its setup code in the journal, because the
/// on-screen card needs a compositor and this needs to work on a device whose
/// display has not come up — or over SSH, where the person reading the journal
/// is the one who will set the password.
fn announce_web_auth_state(
    web: &shepherd_management::WebAuth,
    cfg: &shepherd_config::ManagementApiConfig,
) {
    let scheme = if cfg.tls.is_tls() { "https" } else { "http" };
    let host = if cfg.bind.is_unspecified() {
        "<this device>".to_string()
    } else {
        cfg.bind.to_string()
    };
    match web.enrolment_code() {
        Some(code) => {
            warn!(
                "Management web UI has no password yet. Open {scheme}://{host}:{} and enter \
                 setup code {code} to choose one.",
                cfg.port
            );
        }
        None => {
            info!("Management web UI is password-protected");
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

/// Whether a filesystem event is "the policy file changed".
///
/// A free function so the rule can be tested without a filesystem, a working
/// directory or a timing window — all three of which are what let the bug this
/// replaced live: the old rule compared whole paths, the watched one spelled
/// however `--config` was given and the reported one spelled however `notify`
/// resolved it. Every dev entry point passes `-c ./config.example.toml`, so
/// nothing ever matched and auto-reload was off for the whole development
/// stack while the log said it was on.
///
/// Matching on the file name is exact rather than lax: the watch is on one
/// directory and is not recursive, so a name is unique within what it can see.
fn policy_event_matches(event: &notify::Event, name: &std::ffi::OsStr) -> bool {
    matches!(
        event.kind,
        notify::EventKind::Modify(_) | notify::EventKind::Create(_)
    ) && event.paths.iter().any(|p| p.file_name() == Some(name))
}

#[cfg(test)]
mod policy_watch_tests {
    use super::policy_event_matches;
    use notify::event::{CreateKind, EventKind, ModifyKind, RemoveKind};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn event(kind: EventKind, path: &str) -> notify::Event {
        notify::Event {
            kind,
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        }
    }

    /// The regression. `notify` reports the path it resolved; `--config` is
    /// spelled however the caller typed it, and every dev entry point types
    /// `./config.example.toml`. Comparing the two as paths never matched, so
    /// auto-reload was silently off for the whole development stack — and for
    /// anyone else running shepherdd with a relative `-c`.
    #[test]
    fn a_relative_config_matches_the_absolute_path_the_watcher_reports() {
        assert!(policy_event_matches(
            &event(
                EventKind::Modify(ModifyKind::Any),
                "/home/someone/shepherd/config.example.toml"
            ),
            OsStr::new("config.example.toml"),
        ));
    }

    /// A rename-into-place — which is how `LocalProtectedFiles::write`,
    /// `shepherd install policy` and every careful editor land a new policy —
    /// arrives as a create on the target.
    #[test]
    fn a_rename_into_place_counts() {
        assert!(policy_event_matches(
            &event(
                EventKind::Create(CreateKind::File),
                "/var/lib/x/config.toml"
            ),
            OsStr::new("config.toml"),
        ));
    }

    #[test]
    fn another_file_in_the_same_directory_does_not() {
        // The temp file the atomic write leaves next to the target, most of
        // all: reloading from a half-written policy is the failure the rename
        // exists to prevent.
        assert!(!policy_event_matches(
            &event(
                EventKind::Create(CreateKind::File),
                "/var/lib/x/config.toml.tmp"
            ),
            OsStr::new("config.toml"),
        ));
    }

    #[test]
    fn a_deletion_is_not_a_reason_to_reload() {
        // There would be nothing to read, and the running policy is better
        // than none.
        assert!(!policy_event_matches(
            &event(
                EventKind::Remove(RemoveKind::File),
                "/var/lib/x/config.toml"
            ),
            OsStr::new("config.toml"),
        ));
    }
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

    /// Same contract for #157's downgrade, and for the same reason: falling
    /// back to a store in the kiosk user's home leaves the session running and
    /// looking healthy, so the only trace is this.
    #[test]
    fn an_unprotected_state_store_is_a_critical_service_condition() {
        let d = Service::state_not_protected_diagnostic(
            "connecting to the state custodian at /run/shepherdd/state/kiosk.sock: \
             No such file or directory",
        );

        assert_eq!(d.code, DiagnosticCode::StateNotProtected);
        assert_eq!(d.severity, DiagnosticSeverity::Critical);
        assert!(matches!(d.subject, DiagnosticSubject::Service));
        assert!(
            d.message.contains("No such file or directory"),
            "the underlying reason has to survive into the message: {}",
            d.message
        );
        assert!(d.remedy.is_some(), "a Critical condition needs an answer");
    }

    /// A watchdog that cannot fire is the shape that looks like protection, so
    /// it is `Critical` and it carries the reason (issue #172).
    #[test]
    fn a_watchdog_that_cannot_fire_is_a_critical_service_condition() {
        let d = Service::session_not_guarded_diagnostic(&SessionGuard::Inert(
            "polkit refuses org.freedesktop.login1.manage to this daemon's uid".to_string(),
        ))
        .expect("an inert watchdog is reported");

        assert_eq!(d.code, DiagnosticCode::SessionNotGuarded);
        assert_eq!(d.severity, DiagnosticSeverity::Critical);
        assert!(matches!(d.subject, DiagnosticSubject::Service));
        assert!(
            d.message.contains("polkit refuses"),
            "the underlying reason has to survive into the message: {}",
            d.message
        );
        assert!(
            d.remedy
                .as_deref()
                .is_some_and(|r| r.contains("50-shepherd-session-guard.rules")),
            "the answer is a named file, not advice to investigate"
        );
    }

    /// The four outcomes, and why only two of them say anything.
    ///
    /// A device with no custodian already reports `StateNotProtected`, and one
    /// whose watchdog is armed has nothing to report — so the channel stays
    /// worth reading. The caveat is a `Warning` rather than a `Critical`
    /// because "could not check" is not "will not work": the watchdog still
    /// fires, and finds out then.
    #[test]
    fn only_a_watchdog_worth_worrying_about_reports() {
        for (guard, expected) in [
            (SessionGuard::Armed, None),
            (SessionGuard::NoCustodian, None),
            (
                SessionGuard::Caveat("polkit did not answer".into()),
                Some(DiagnosticSeverity::Warning),
            ),
            (
                SessionGuard::Inert("no polkit rule".into()),
                Some(DiagnosticSeverity::Critical),
            ),
        ] {
            let sink = RecordingSink::default();
            Service::unguarded_session_is_reported(&guard, &sink);
            let raised = sink.raised.lock().expect("lock");
            match expected {
                None => assert!(raised.is_empty(), "{guard:?} should say nothing"),
                Some(_) => assert_eq!(
                    *raised,
                    vec![DiagnosticCode::SessionNotGuarded],
                    "{guard:?} should report an unguarded session"
                ),
            }
            let severity = Service::session_not_guarded_diagnostic(&guard).map(|d| d.severity);
            assert_eq!(severity, expected, "{guard:?} reported the wrong severity");
        }
    }

    /// Only a *degraded* store is reported. An operator who passed
    /// `--no-state-custodian` asked for this and does not need telling, and a
    /// device using the custodian has nothing to report — so a diagnostic in
    /// either case would be noise that trains people to ignore the channel.
    #[test]
    fn only_a_degraded_state_store_reports() {
        for (state, expected) in [
            (StateProtection::Custodian, 0),
            (StateProtection::OptedOut, 0),
            (StateProtection::Degraded("socket missing".into()), 1),
        ] {
            let sink = RecordingSink::default();
            Service::degraded_state_protection_is_reported(&state, &sink);
            assert_eq!(
                sink.raised.lock().expect("lock").len(),
                expected,
                "{state:?} reported the wrong number of diagnostics"
            );
        }
    }

    /// The signal that tells a broken custodian apart from one that was never
    /// installed, and the reason it can be trusted: the directory is one only
    /// root can create, in a tree an activity cannot write.
    ///
    /// Both answers matter. A `true` on a fresh install would refuse claims on
    /// a device nobody has ever paired — unpairable out of the box. A `false`
    /// on a device whose protection broke is the bug this exists to prevent.
    #[test]
    fn a_custodian_state_directory_is_what_says_protection_was_expected() {
        // No custodian was ever installed for a name nothing owns.
        assert!(!Service::custodian_holds_state_for(
            "definitely-not-a-user-on-this-box"
        ));

        // And the path it asks about is the one the custodian actually uses,
        // which is the half that would rot silently if either side moved.
        assert_eq!(
            shepherd_state_proto::state_dir("kiosk"),
            std::path::Path::new("/var/lib/shepherdd/state/kiosk")
        );
    }
}

/// Decide whether to pre-boot Android at startup. An explicit
/// `[service.waydroid] preboot` wins; when unset, pre-boot iff at least one
/// Android entry is configured (so a kiosk with no Android activities pays no
/// Waydroid cost).
fn should_preboot_waydroid(configured: Option<bool>, has_android: bool) -> bool {
    configured.unwrap_or(has_android)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preboot_defaults_to_presence_of_android_entries() {
        assert!(should_preboot_waydroid(None, true));
        assert!(!should_preboot_waydroid(None, false));
    }

    #[test]
    fn explicit_preboot_overrides_either_way() {
        // Forced on even with no Android entries...
        assert!(should_preboot_waydroid(Some(true), false));
        // ...and forced off even when Android entries exist.
        assert!(!should_preboot_waydroid(Some(false), true));
    }
}
