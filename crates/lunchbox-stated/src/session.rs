//! Which cgroup `lunchbox-stated` trusts, and how it finds out (issue #157).
//!
//! The broker is deliberately **not** in the session it serves — it runs under
//! the system manager as its own uid, so it cannot compare a peer against its
//! own cgroup the way `lunchboxd` does. It has to name the trusted cgroup
//! instead, and that name has to come from somewhere an activity cannot steer.
//!
//! logind is that somewhere. It creates the session scope, it is the only thing
//! that can, and it will say which scope belongs to which session.
//!
//! ## Why the graphical session specifically
//!
//! "A session scope of the right uid" is not narrow enough. Measured on this
//! host, every session the kiosk user could have:
//!
//! | session | class | type | seat |
//! | --- | --- | --- | --- |
//! | the kiosk itself, started by GDM | `user` | `wayland` | `seat0` |
//! | its user manager | `manager` | `unspecified` | — |
//! | an SSH login | `user` | `tty` | — |
//!
//! `Class=user` alone would admit an SSH login, which is a second way into the
//! same uid rather than the session Lunchbox runs in. Requiring **`Class=user`,
//! `Type=wayland`, and a seat** admits exactly the one GDM started.
//!
//! `lunchbox harden apply` denies the kiosk user SSH and console login, so on a
//! hardened device that second session cannot exist at all. This check does not
//! rely on that — it is what makes the rule correct on a device where hardening
//! was skipped.
//!
//! ## Why not the other candidates
//!
//! * **Trust-on-first-use** ("pin whoever connects first") is racy at boot in
//!   the wrong direction. The design should not rest on lunchboxd being quick.
//! * **`GetSessionByPID`** would work — the peer's pid is pinned by the pidfd,
//!   so it is not the racy lookup it looks like — but it turns one integer
//!   compare into a D-Bus round trip per connection, and moves the decision off
//!   the mechanism `lunchbox-ipc`'s `peer.rs` already justifies and tests.
//! * **The cgroup path as a string** would mean parsing `/proc` and agreeing
//!   with the kernel about where cgroup2 is mounted. The id is an inode number
//!   and `PIDFD_GET_INFO` reports the same one, so the comparison stays a `u64`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use zbus::zvariant::OwnedObjectPath;

/// logind's manager, enough of it to list sessions.
#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LogindManager {
    /// `(id, uid, user, seat, object_path)` for every current session.
    #[zbus(name = "ListSessions")]
    fn list_sessions(&self) -> zbus::Result<Vec<(String, u32, String, String, OwnedObjectPath)>>;

    /// Fires when a session appears or goes away. A logout/login gives the
    /// kiosk a *new* scope, so the trusted cgroup has to be re-resolved.
    #[zbus(signal)]
    fn session_new(&self, id: String, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    fn session_removed(&self, id: String, path: OwnedObjectPath) -> zbus::Result<()>;

    /// End a session (issue #172). Needs polkit's
    /// `org.freedesktop.login1.manage` from a uid that does not own the
    /// session, which is what `dist/polkit/50-lunchbox-session-guard.rules`
    /// grants and what [`crate::polkit`] checks for at startup.
    fn terminate_session(&self, session_id: &str) -> zbus::Result<()>;

    /// Kill everything a uid is running, for when asking did not work.
    ///
    /// `KillUser` rather than `KillSession` deliberately: the session scope
    /// holds the compositor and Lunchbox's own UI, while the activities live in
    /// the user manager's `app.slice`. See [`crate::guard::Terminator::kill_user`].
    fn kill_user(&self, uid: u32, signal_number: i32) -> zbus::Result<()>;

    /// `true` as the machine goes to sleep, `false` once it is back.
    ///
    /// The watchdog disarms on the first and re-arms on the second: a sleeping
    /// machine is not a wedged one, and a device that slept through the night
    /// must not wake to a session that was terminated for not beating while
    /// nothing was running at all.
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// One session's properties, as far as the filter cares.
#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait LogindSession {
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn type_(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn scope(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn seat(&self) -> zbus::Result<(String, OwnedObjectPath)>;
    /// `"active"`, `"online"` or `"closing"`. The last is a session logind is
    /// tearing down, which can outlive the login that replaced it.
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;
}

/// The kiosk's graphical session, resolved to something comparable.
#[derive(Debug, Clone)]
pub struct TrustedSession {
    /// logind's session id, for the log line. An operator who has to debug a
    /// refusal wants to know which session was trusted, not just its inode.
    pub id: String,
    /// The scope unit, e.g. `session-274.scope`.
    pub scope: String,
    /// The cgroup id every accepted peer must be in — the scope directory's
    /// inode number, which is what `PIDFD_GET_INFO` reports for a peer.
    pub cgroup_id: u64,
}

/// How long to wait for the kiosk's session to appear before giving up.
///
/// "No graphical session" is normally a *race*, not an answer: logind registers
/// the session, the display manager starts the compositor, the compositor execs
/// lunchboxd, and lunchboxd connects here. Socket activation means this daemon
/// starts at the end of that chain, so it is usually already there — but it is
/// not guaranteed to be, and treating a lost race as a failure is what took the
/// socket down (see [`resolve_waiting`]).
const SESSION_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// What [`resolve_waiting`] settled on.
pub enum Trust {
    /// The kiosk's graphical session.
    Session(TrustedSession),
    /// Nothing may be trusted, and why.
    ///
    /// Deliberately **not** an error. The daemon exits cleanly on this, which
    /// with `Restart=on-failure` spends no restart budget and leaves the socket
    /// armed for the next connection.
    Nothing(String),
}

/// [`resolve`], but wait for the session rather than failing when there is not
/// exactly one yet.
///
/// [`Trust::Nothing`] covers both ways there can be no answer, because a device
/// gets out of both by itself and neither is this daemon's to fix:
///
/// * **None.** At the greeter, or something probed the socket before anyone
///   logged in. There is genuinely nothing to trust.
/// * **More than one.** [`resolve`] refuses to guess, and it should. In the
///   ordinary case the extra one is transient — a `switch user`, or a login
///   overlapping a logout that has not finished — so it is worth waiting out.
///
/// Failing instead is what a device measured: five quick failures tripped
/// systemd's start limit, which failed the *socket* unit, after which every
/// connection for the rest of the boot was refused and lunchboxd ran on an
/// unprotected local store — a permanent downgrade from a transient race. That
/// is exactly the trade a second session must not be able to force, so it ends
/// the same way the empty case does.
pub async fn resolve_waiting(conn: &zbus::Connection, uid: u32) -> Result<Trust> {
    let manager = LogindManagerProxy::new(conn)
        .await
        .context("connecting to logind")?;
    // Subscribe *before* the first look, or a change between the two is missed
    // and the wait runs to its timeout for no reason.
    //
    // Departures as well as arrivals: the ambiguous case is waiting for a
    // session to *go*, which no arrival will ever signal.
    let mut arrivals = manager
        .receive_session_new()
        .await
        .context("subscribing to logind session arrivals")?;
    let mut departures = manager
        .receive_session_removed()
        .await
        .context("subscribing to logind session departures")?;

    let mut why = match resolve(conn, uid).await {
        Ok(session) => return Ok(Trust::Session(session)),
        Err(e) if !is_unresolved(&e) => return Err(e),
        Err(e) => e.to_string(),
    };

    tracing::info!(
        uid,
        timeout_secs = SESSION_WAIT.as_secs(),
        reason = %why,
        "Nothing to trust yet; waiting for the sessions to settle"
    );
    let deadline = tokio::time::Instant::now() + SESSION_WAIT;
    loop {
        use futures_util::StreamExt as _;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(Trust::Nothing(why));
        }
        let changed = tokio::time::timeout(remaining, async {
            tokio::select! {
                arrived = arrivals.next() => arrived.is_some(),
                departed = departures.next() => departed.is_some(),
            }
        })
        .await;
        match changed {
            // The set of sessions moved. It may not have moved the way that
            // helps — a user manager registers too — so re-resolve rather than
            // trusting the signal.
            Ok(true) => match resolve(conn, uid).await {
                Ok(session) => return Ok(Trust::Session(session)),
                Err(e) if !is_unresolved(&e) => return Err(e),
                Err(e) => {
                    why = e.to_string();
                    continue;
                }
            },
            // logind stopped talking to us, or the wait ran out.
            Ok(false) | Err(_) => return Ok(Trust::Nothing(why)),
        }
    }
}

/// Whether an error from [`resolve`] is "nothing to trust *yet*" rather than a
/// genuine failure.
///
/// Both of [`resolve`]'s refusals qualify — no graphical session, and more than
/// one — because both can clear on their own and neither means the daemon is
/// broken. Anything else (logind unreachable, a cgroup directory that cannot be
/// read) is a real error and stays one.
///
/// A string check because the distinction is between two `bail!`s in one
/// function; splitting the error into a type would be more ceremony than the
/// one call site needs, and both messages are asserted in tests so they cannot
/// drift silently.
fn is_unresolved(e: &anyhow::Error) -> bool {
    let message = e.to_string();
    // "graphical sessions", plural, is only ever the more-than-one refusal.
    message.contains("has no graphical session") || message.contains("graphical sessions")
}

/// Find the graphical session of `uid` and resolve its cgroup id.
///
/// Errors rather than guessing when there is no such session or more than one:
/// a broker that picked one of two would be trusting a cgroup nobody chose, and
/// the failure mode of trusting the wrong one is the failure this exists to
/// prevent.
pub async fn resolve(conn: &zbus::Connection, uid: u32) -> Result<TrustedSession> {
    let manager = LogindManagerProxy::new(conn)
        .await
        .context("connecting to logind")?;
    let sessions = manager
        .list_sessions()
        .await
        .context("listing logind sessions")?;

    let mut found: Vec<TrustedSession> = Vec::new();
    for (id, session_uid, _user, _seat, path) in sessions {
        if session_uid != uid {
            continue;
        }
        let session = LogindSessionProxy::builder(conn)
            .path(path.clone())
            .context("building a logind session proxy")?
            .build()
            .await
            .context("connecting to a logind session")?;

        // A session can disappear between ListSessions and these reads. That is
        // an ordinary race, not a refusal: skip it and let the caller act on
        // whatever is still there.
        let (Ok(class), Ok(kind), Ok(seat), Ok(scope), Ok(state)) = (
            session.class().await,
            session.type_().await,
            session.seat().await,
            session.scope().await,
            session.state().await,
        ) else {
            continue;
        };

        if !is_graphical(&class, &kind, &seat.0, &state) {
            continue;
        }

        let dir = scope_cgroup_path(uid, &scope);
        let meta = std::fs::metadata(&dir)
            .with_context(|| format!("reading the cgroup directory {}", dir.display()))?;
        found.push(TrustedSession {
            id,
            scope,
            cgroup_id: std::os::unix::fs::MetadataExt::ino(&meta),
        });
    }

    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!(
            "uid {uid} has no graphical session (looking for one with class=user, \
             type=wayland and a seat); nothing can be trusted until it logs in"
        ),
        n => bail!(
            "uid {uid} has {n} graphical sessions ({}); refusing to guess which one is \
             Lunchbox's",
            found
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Resolve once more when logind says a session went away, and finish when the
/// one we trust is the one that went.
///
/// Returns a future rather than spawning, so the caller can `select!` it
/// against the accept loop and let the process end by falling out of `main`.
///
/// Watching `SessionRemoved` rather than polling because the question is not
/// "is it still there" on a timer — it is "has the thing we based a security
/// decision on ceased to exist", and logind says so exactly once.
pub async fn watch_for_loss(
    conn: &zbus::Connection,
    trusted: TrustedSession,
) -> Result<impl std::future::Future<Output = Result<()>> + use<>> {
    let manager = LogindManagerProxy::new(conn)
        .await
        .context("connecting to logind")?;
    let mut removals = manager
        .receive_session_removed()
        .await
        .context("subscribing to logind session removals")?;

    Ok(async move {
        use futures_util::StreamExt as _;
        while let Some(signal) = removals.next().await {
            let Ok(args) = signal.args() else { continue };
            if args.id == trusted.id {
                tracing::info!(
                    session = %trusted.id,
                    "The trusted session ended; exiting so the next connection resolves the \
                     session that replaces it"
                );
                return Ok(());
            }
        }
        // logind went away, or the stream ended. Nothing left to notice a
        // logout with, so the trust this daemon holds can no longer be kept
        // honest — stop rather than serve on a promise we cannot keep.
        bail!("logind's session-removal signal ended; cannot keep the trusted session honest")
    })
}

/// Whether a logind session is the one Lunchbox runs in.
///
/// Split out from [`resolve`] because it is the whole security-relevant part of
/// the filter and the only part testable without a bus. The rows it has to get
/// right were measured on this host — see the table in the module header.
///
/// `state` is here because of something only a device showed: after a display
/// manager restart, the outgoing session can still be listed, still be
/// `class=user`/`type=wayland`/seated, and still own a live cgroup — while
/// logind has it as `closing`. Two sessions then match, the daemon refuses to
/// guess (correctly), and the custodian never starts. Excluding `closing`
/// leaves the one that is actually the session.
///
/// `active` and `online` are both accepted: a kiosk whose VT is switched away
/// is `online`, and it is still the session Lunchbox is running in.
fn is_graphical(class: &str, kind: &str, seat: &str, state: &str) -> bool {
    class == "user" && kind == "wayland" && !seat.is_empty() && state != "closing"
}

/// Where logind puts a session scope in the unified hierarchy.
///
/// Assembled rather than read from `/proc/self/cgroup`, because the broker is
/// in a different cgroup than the thing it is describing. The shape is logind's
/// and has been stable since cgroup v2: `user.slice/user-<uid>.slice/<scope>`.
fn scope_cgroup_path(uid: u32, scope: &str) -> PathBuf {
    PathBuf::from("/sys/fs/cgroup/user.slice")
        .join(format!("user-{uid}.slice"))
        .join(scope)
}

/// [`crate::guard::Terminator`] over logind.
///
/// The session id is the one [`resolve`] settled on, carried from there rather
/// than looked up again: the filter that produced it is the security-relevant
/// part, and a terminator that re-resolved could end a session the guard was
/// never watching.
pub struct LogindTerminator {
    manager: LogindManagerProxy<'static>,
}

impl LogindTerminator {
    pub async fn new(conn: &zbus::Connection) -> Result<Self> {
        Ok(Self {
            manager: LogindManagerProxy::new(conn)
                .await
                .context("connecting to logind to be able to end the session")?,
        })
    }
}

impl crate::guard::Terminator for LogindTerminator {
    fn terminate<'a>(
        &'a self,
        session_id: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.manager
                .terminate_session(session_id)
                .await
                .with_context(|| format!("asking logind to terminate session {session_id}"))
        })
    }

    fn kill_user(&self, uid: u32) -> futures_util::future::BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.manager
                // SIGKILL, and everything the uid is running: this only runs
                // after a terminate has already been ignored for ten seconds,
                // and something that ignored logind is not going to be moved by
                // a politer signal.
                .kill_user(uid, nix::sys::signal::Signal::SIGKILL as i32)
                .await
                .with_context(|| format!("asking logind to kill everything uid {uid} is running"))
        })
    }
}

/// Turn logind's sleep signal into guard events.
///
/// Returns a future to spawn. Failing to subscribe is *not* fatal: the guard
/// still works, it just loses the one thing that tells a suspend apart from a
/// wedge, so it is reported loudly and the daemon carries on.
pub async fn forward_sleep_signals(
    conn: &zbus::Connection,
    tx: tokio::sync::mpsc::UnboundedSender<crate::guard::Event>,
) -> Result<impl std::future::Future<Output = ()> + use<>> {
    let manager = LogindManagerProxy::new(conn)
        .await
        .context("connecting to logind")?;
    let mut signals = manager
        .receive_prepare_for_sleep()
        .await
        .context("subscribing to logind's sleep signal")?;

    Ok(async move {
        use futures_util::StreamExt as _;
        while let Some(signal) = signals.next().await {
            let Ok(args) = signal.args() else { continue };
            let event = if args.start {
                tracing::info!("The machine is suspending; the watchdog stands down");
                crate::guard::Event::Suspending
            } else {
                tracing::info!("Resumed; the watchdog is armed again with a full deadline");
                crate::guard::Event::Resumed
            };
            if tx.send(event).is_err() {
                return;
            }
        }
        tracing::warn!(
            "logind's sleep signal ended; a suspend would now look like a wedged lunchboxd"
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_path_is_the_shape_logind_uses() {
        // Measured against a GDM-started kiosk session on 26.04: lunchboxd sat
        // in `/user.slice/user-1001.slice/session-274.scope`, whose directory
        // inode was the cgroup id `PIDFD_GET_INFO` reported for it.
        assert_eq!(
            scope_cgroup_path(1001, "session-274.scope"),
            PathBuf::from("/sys/fs/cgroup/user.slice/user-1001.slice/session-274.scope")
        );
    }

    #[test]
    fn nothing_to_trust_yet_is_told_apart_from_a_real_failure() {
        // `resolve_waiting` waits on both of `resolve`'s `bail!`s and gives up
        // on anything else, and it tells them apart by their message. Getting
        // this wrong is not a compile error and not a test failure anywhere
        // else — it is a device that fails at boot instead of waiting, and a
        // failure here is what took a socket unit down for a whole boot and
        // left lunchboxd on an unprotected store.
        let missing = anyhow::anyhow!(
            "uid 1001 has no graphical session (looking for one with class=user, \
             type=wayland and a seat); nothing can be trusted until it logs in"
        );
        assert!(is_unresolved(&missing));

        // Two sessions is transient in the ordinary case (a `switch user`, or a
        // login overlapping a logout), so it waits rather than failing. If it
        // does not clear, the daemon stops cleanly with the socket armed — the
        // one thing it must not do is spend restart budget, because running out
        // is what makes a second session a downgrade that outlasts it.
        let ambiguous = anyhow::anyhow!(
            "uid 1001 has 2 graphical sessions (7, 9); refusing to guess which \
                             one is Lunchbox's"
        );
        assert!(is_unresolved(&ambiguous));

        let broken = anyhow::anyhow!("connecting to logind");
        assert!(!is_unresolved(&broken));

        // The two refusals are told apart by a plural, so a singular message
        // must not match the ambiguous arm by accident.
        assert!(!missing.to_string().contains("graphical sessions"));
    }

    #[test]
    fn only_the_graphical_session_is_trusted() {
        // Every session the kiosk uid actually had on a device running GDM,
        // measured. The SSH row is the one that matters: it is `class=user`
        // too, so a filter that checked only the class would admit a second
        // way into the same uid.
        assert!(
            is_graphical("user", "wayland", "seat0", "active"),
            "the session GDM started must be trusted"
        );
        assert!(
            !is_graphical("manager", "unspecified", "", "active"),
            "the user manager is not a session Lunchbox runs in"
        );
        assert!(
            !is_graphical("user", "tty", "", "active"),
            "an SSH login is class=user, and must not be trusted"
        );
        // A seatless graphical session should not happen, but "no seat" is the
        // property that separates a login from a remote one, so it decides.
        assert!(!is_graphical("user", "wayland", "", "active"));

        // Measured on a device: after `systemctl restart gdm` the outgoing
        // session was still listed, still seated and still owned a live cgroup,
        // with logind reporting `closing`. Two sessions matched, the daemon
        // refused to guess, and the custodian would not start.
        assert!(
            !is_graphical("user", "wayland", "seat0", "closing"),
            "a session logind is tearing down is not the one Lunchbox runs in"
        );
        // But a kiosk whose VT is switched away is `online`, and is still it.
        assert!(is_graphical("user", "wayland", "seat0", "online"));
    }
}
