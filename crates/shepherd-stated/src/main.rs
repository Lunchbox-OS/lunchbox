//! `shepherd-stated` — custodian for shepherd's policy and state (issue #157).
//!
//! shepherdd runs inside the kiosk session, as the same uid as every activity
//! it launches. So do its files: `shepherdd.db`, `config.toml`, the BLE admin
//! record. File permissions cannot separate them, and neither can confinement —
//! `docs/ai/history/2026-08-29 005` measured an activity resetting today's usage
//! and adding an unlimited entry to the policy, live, on an installed device.
//!
//! This daemon owns those files at a uid activities do not have, and hands them
//! to shepherdd over a socket that admits exactly one cgroup: the kiosk's logind
//! session scope. Everything an activity can do to get a process somewhere else
//! — `systemd --user`, the firewall helper — lands it outside that scope, and it
//! can neither join the scope nor make logind create another one. Those are
//! measurements, not assumptions; see `2026-08-29 005`.
//!
//! ## What it is not
//!
//! Not root. It opens a database it owns, reads a file it owns, and asks logind
//! a read-only question; nothing there needs privilege, and a privileged process
//! parsing attacker-adjacent input is the thing this design is otherwise free of.
//!
//! Not a policy engine. It never learns what a limit means or whether a child
//! may launch something — that stays in `shepherd-core`, inside shepherdd. This
//! is custody, not judgment.
//!
//! Not a spawner. It runs no subprocess at all, which is what keeps the whole
//! `$PATH`-substitution class (`2026-08-29 004`) away from it. `clippy.toml`
//! denies bare `Command::new` workspace-wide, so that stays true by default.
//!
//! ## Socket activation
//!
//! The listening socket is created by the **system manager**, as root, and
//! handed over as a file descriptor. That is worth more than the convenience:
//! `2026-08-29 004`'s finding 2 showed an activity can `unlink()` and rebind
//! shepherdd's own socket because it lives at the child's uid, and concluded
//! that preventing it "requires the socket to be created by something other than
//! the daemon, i.e. systemd socket activation … which `shepherdd` cannot use
//! while sway `exec`s it". This daemon can, so the name-takeover class does not
//! exist here.

mod session;
mod watch;

use std::os::fd::{AsFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener as StdUnixListener;

use anyhow::{Context, Result, bail};
use clap::Parser;
use shepherd_ipc::{PeerPolicy, kernel_supports_peer_cgroup};
use shepherd_state_proto::StateRequest;
use shepherd_store::{SqliteStore, Store};
use shepherd_util::{LocalProtectedFiles, ProtectedFiles};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, error, info, warn};

/// The fd number systemd passes the first listening socket on, per
/// `sd_listen_fds(3)`. Fixed by the protocol, not chosen here.
const SD_LISTEN_FDS_START: RawFd = 3;

#[derive(Parser, Debug)]
#[command(name = "shepherd-stated", about, version)]
struct Args {
    /// The kiosk user whose session is trusted, and whose state this serves.
    #[arg(long)]
    user: String,

    /// Where the protected state lives. Defaults to what the systemd unit's
    /// `StateDirectory=` creates, which is the only path a device uses.
    ///
    /// Not read from the environment, deliberately: on a device the environment
    /// belongs to the kiosk user, and a state directory an activity could
    /// choose would defeat the point of protecting the one it cannot
    /// (`docs/ai/history/2026-08-29 004`).
    #[arg(long)]
    state_dir: Option<std::path::PathBuf>,

    /// Bind here instead of taking a socket from the service manager. For
    /// running the daemon by hand; the packaged unit uses socket activation,
    /// which is what keeps the socket's *name* out of reach of the kiosk uid.
    #[arg(long)]
    socket: Option<std::path::PathBuf>,

    #[arg(long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&args.log_level)),
        )
        .init();

    let uid = uid_of(&args.user)?;
    info!(user = %args.user, uid, "shepherd-stated starting");

    // Refuse before binding rather than after. A kernel that cannot report a
    // peer's cgroup cannot tell shepherdd from an activity, and a broker that
    // cannot tell them apart has nothing to offer: serving anyway would hand
    // over exactly what this exists to protect. shepherdd's own check makes the
    // opposite trade — it degrades and carries on — because a kiosk that will
    // not start is worse than one that says it is unprotected, and because
    // nothing an activity does can lower the kernel.
    if !kernel_supports_peer_cgroup() {
        bail!(
            "this kernel cannot report a peer's cgroup (needs SO_PEERPIDFD + \
             PIDFD_GET_INFO), so no peer can be identified and nothing may be served"
        );
    }

    let conn = zbus::Connection::system()
        .await
        .context("connecting to the system bus to ask logind about sessions")?;

    let trusted = match session::resolve_waiting(&conn, uid).await? {
        session::Trust::Session(trusted) => trusted,
        // Nobody is logged in, or more than one session is and there is no
        // right one to pick. Exit *cleanly*: with `Restart=on-failure` that
        // consumes no restart budget, so the socket stays armed and the next
        // connection tries again. Failing here is what a device measured
        // taking the socket unit down for a whole boot.
        session::Trust::Nothing(reason) => {
            info!(
                user = %args.user,
                %reason,
                "Nothing to trust; serving nobody and stopping"
            );
            return Ok(());
        }
    };
    info!(
        session = %trusted.id,
        scope = %trusted.scope,
        cgroup_id = trusted.cgroup_id,
        "Trusting the kiosk's graphical session; every other peer will be refused"
    );

    let policy = PeerPolicy::for_cgroup(trusted.cgroup_id);

    let state_dir = args
        .state_dir
        .clone()
        .unwrap_or_else(|| default_state_dir(&args.user));
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("creating the state directory {}", state_dir.display()))?;
    let db_path = state_dir.join("shepherdd.db");
    let store: Arc<dyn Store> = Arc::new(
        SqliteStore::open(&db_path)
            .with_context(|| format!("opening the database {}", db_path.display()))?,
    );
    info!(db = %db_path.display(), "Store opened");

    // The same `LocalProtectedFiles` shepherdd uses for its data directory when
    // the custodian is opted out — one implementation, so "protected" and "not
    // protected" cannot drift into two behaviours.
    let files: Arc<dyn ProtectedFiles> = Arc::new(LocalProtectedFiles::new(state_dir.clone()));
    let config_changes = watch::config_changes(&state_dir)?;
    watch::warn_if_no_policy(&state_dir);

    let listener = listener(&args)?;

    // A logout ends that session and its scope; the next login gets a *new*
    // one. Nothing here would notice, and a daemon still comparing against a
    // cgroup that no longer exists refuses every peer — the safe direction, but
    // a device whose launcher has silently stopped working.
    //
    // So the trusted session's disappearance ends the process, and socket
    // activation starts a fresh one on the next connection, which resolves the
    // session that exists then. Re-resolving in place would work too; exiting
    // is less code and cannot leave a half-updated policy behind.
    let watch = session::watch_for_loss(&conn, trusted.clone()).await?;

    tokio::select! {
        r = serve(listener, policy, store, files, config_changes) => r,
        r = watch => r,
    }
}

/// Where the systemd unit's `StateDirectory=shepherdd/state/%i` puts things.
///
/// Assembled rather than taken from the environment. systemd does pass
/// `$STATE_DIRECTORY`, but honouring it would mean the daemon's idea of where
/// the protected files live came from a variable — and this whole issue is
/// about state that can be pointed somewhere else.
fn default_state_dir(user: &str) -> std::path::PathBuf {
    // The same constant shepherdd stats to tell "the custodian is broken" apart
    // from "this device never had one", so the two cannot drift into disagreeing
    // about where the state lives.
    shepherd_state_proto::state_dir(user)
}

/// Resolve a user name to a uid without shelling out.
fn uid_of(name: &str) -> Result<u32> {
    let user = nix::unistd::User::from_name(name)
        .with_context(|| format!("looking up user {name}"))?
        .with_context(|| format!("no such user: {name}"))?;
    Ok(user.uid.as_raw())
}

/// Take the listening socket from the service manager, or bind one.
fn listener(args: &Args) -> Result<UnixListener> {
    match inherited_listener()? {
        Some(fd) => {
            if args.socket.is_some() {
                warn!(
                    "Both socket activation and --socket were given; using the inherited \
                     socket, which is the one the service manager owns"
                );
            }
            let std_listener = StdUnixListener::from(fd);
            std_listener
                .set_nonblocking(true)
                .context("making the inherited socket non-blocking")?;
            info!("Serving on the socket the service manager passed in");
            UnixListener::from_std(std_listener).context("adopting the inherited socket")
        }
        None => {
            let path = args.socket.as_ref().context(
                "no socket was passed by the service manager and --socket was not given",
            )?;
            // Only for a hand-run daemon: the packaged unit never reaches here,
            // so the name this binds is not the one a device depends on.
            if path.exists() {
                std::fs::remove_file(path)
                    .with_context(|| format!("removing the stale socket {}", path.display()))?;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let l =
                UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
            // Connecting to a Unix socket needs *write* permission on the
            // inode, so the mode has to be permissive for shepherdd — at a
            // different uid — to reach it at all. That is deliberate and not a
            // weakness: the peer check is the gate, and the file mode is not.
            // Anything stricter would have to be either owner-only (locking
            // shepherdd out) or group-shared with the kiosk uid, which every
            // activity also has — decorative, and the same mistake as the
            // pre-#144 uid check. The packaged unit says `SocketMode=0666` for
            // this reason; a hand-run daemon has to match it or nothing can
            // connect.
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o666))
                .with_context(|| format!("setting the mode on {}", path.display()))?;
            warn!(
                path = %path.display(),
                "Bound the socket directly; on a device the service manager should own this \
                 name, so an activity cannot take it (issue #144, finding 2)"
            );
            Ok(l)
        }
    }
}

/// The socket systemd passed us, if it passed one.
///
/// `sd_listen_fds(3)` in the small: `$LISTEN_PID` guards against inheriting the
/// variables through an exec into some other process, and `$LISTEN_FDS` counts
/// what starts at fd 3. More than one socket is a misconfigured unit rather than
/// something to pick from, so it is an error and not a choice.
fn inherited_listener() -> Result<Option<OwnedFd>> {
    let Ok(pid) = std::env::var("LISTEN_PID") else {
        return Ok(None);
    };
    if pid.parse::<i32>().ok() != Some(std::process::id() as i32) {
        // Meant for a different process; ignore rather than steal.
        return Ok(None);
    }
    let count: i32 = std::env::var("LISTEN_FDS")
        .context("LISTEN_PID was set without LISTEN_FDS")?
        .parse()
        .context("LISTEN_FDS is not a number")?;
    match count {
        0 => Ok(None),
        1 => {
            // SAFETY: fd 3 is the listening socket systemd created and passed;
            // nothing else in this process has claimed it.
            Ok(Some(unsafe { OwnedFd::from_raw_fd(SD_LISTEN_FDS_START) }))
        }
        n => bail!("the service manager passed {n} sockets; this daemon serves exactly one"),
    }
}

/// Accept connections, classify each one, and serve the ones that belong.
async fn serve(
    listener: UnixListener,
    policy: PeerPolicy,
    store: Arc<dyn Store>,
    files: Arc<dyn ProtectedFiles>,
    config_changes: tokio::sync::broadcast::Sender<()>,
) -> Result<()> {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                error!(error = %e, "Accept failed");
                continue;
            }
        };

        // Decided once, at accept, before a single byte is parsed — so a peer
        // that should not be here never reaches the deserializer. Same ordering
        // and the same reason as shepherdd's own socket.
        let uid = shepherd_ipc::peer_uid(stream.as_fd());
        match policy.classify(stream.as_fd(), uid) {
            Ok(_role) => {
                debug!("Accepted a peer from the trusted session");
                let store = Arc::clone(&store);
                let files = Arc::clone(&files);
                let changes = config_changes.subscribe();
                // One task per connection. In practice there are two — a
                // request connection and a config watch — but a task keeps a
                // slow or wedged peer from stalling the accept loop, which is
                // what would turn a hung client into a device that cannot
                // reconnect.
                tokio::spawn(async move {
                    if let Err(e) = session_loop(stream, store, files, changes).await {
                        debug!(error = %e, "A state connection ended");
                    }
                });
            }
            Err(rejection) => {
                // A refusal answers nothing: a refusal that replies is a
                // refusal that can be probed.
                warn!(peer_pid = ?rejection.peer_pid, "Refused a peer: {rejection}");
                drop(stream);
            }
        }
    }
}

/// One accepted connection: read a request per line, write a reply per line.
///
/// The store is synchronous, so each call happens on a blocking thread rather
/// than on the runtime. That matters more here than it looks: a SQLite write
/// fsyncs, and holding a runtime worker for it would stall the accept loop and
/// every other connection with it.
async fn session_loop(
    stream: tokio::net::UnixStream,
    store: Arc<dyn Store>,
    files: Arc<dyn ProtectedFiles>,
    mut changes: tokio::sync::broadcast::Receiver<()>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        // `WatchConfig` changes what the connection is, so it is intercepted
        // here rather than dispatched: after it, this side only writes.
        if matches!(
            serde_json::from_str::<StateRequest>(&line),
            Ok(StateRequest::WatchConfig)
        ) {
            debug!("A peer is watching the policy for changes");
            loop {
                match changes.recv().await {
                    Ok(()) => {
                        let line = serde_json::to_string(&shepherd_state_proto::ConfigChanged {})?;
                        write_half.write_all(line.as_bytes()).await?;
                        write_half.write_all(b"\n").await?;
                        write_half.flush().await?;
                    }
                    // Lagged: the watcher missed changes while the client was
                    // slow. Send one anyway — the client re-reads the file, so
                    // "something changed" is the whole message and coalescing
                    // is correct rather than lossy.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let line = serde_json::to_string(&shepherd_state_proto::ConfigChanged {})?;
                        write_half.write_all(line.as_bytes()).await?;
                        write_half.write_all(b"\n").await?;
                        write_half.flush().await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
        let reply = match serde_json::from_str::<StateRequest>(&line) {
            Ok(request) => {
                let store = Arc::clone(&store);
                let files = Arc::clone(&files);
                tokio::task::spawn_blocking(move || {
                    shepherd_state_proto::server::handle(store.as_ref(), files.as_ref(), request)
                })
                .await
                .context("the store call panicked")?
            }
            Err(e) => {
                // Malformed input from a peer that passed the cgroup check is
                // a bug in shepherdd, not an attack — answer with an error
                // rather than dropping the connection, so it says so.
                warn!(error = %e, "Could not decode a request");
                serde_json::to_string(&shepherd_state_proto::WireResult::<()>::Err {
                    kind: shepherd_state_proto::WireErrorKind::Serialization,
                    message: format!("could not decode the request: {e}"),
                })?
            }
        };
        write_half.write_all(reply.as_bytes()).await?;
        write_half.write_all(b"\n").await?;
        write_half.flush().await?;
    }
    Ok(())
}
