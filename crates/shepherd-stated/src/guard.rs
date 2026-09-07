//! The session watchdog (issue #172).
//!
//! `shepherdd` runs as the kiosk uid, and so does every activity. Signal
//! permission is a uid comparison, so an activity can `kill` its own
//! supervisor — and #161's fallback, the `sh -c` wrapper that turns a dead
//! shepherdd into `loginctl terminate-session`, runs at that uid too. Kill the
//! wrapper first and nothing is left to run it. `SIGSTOP` skips even that: a
//! stopped shepherdd never exits, so the wrapper's `||` never fires, while the
//! engine that counts a child's time has stopped.
//!
//! This daemon is the one piece of shepherd that is outside the session, at a
//! uid nothing inside it can signal, that already knows which session is the
//! kiosk's. So it holds the dead man's switch: shepherdd feeds a connection,
//! and when the feeding stops the session ends.
//!
//! ## The state machine is pure, on purpose
//!
//! [`Guard`] has no clock, no socket and no bus of its own — every entry point
//! takes `now` and returns an [`Action`]. That is what lets the cases that
//! matter be tested as arithmetic rather than as a device: a kill, a `SIGSTOP`,
//! a suspend that outlasts the deadline, a session that never resolved. The
//! driver ([`run`]) is the only part that needs a runtime, and all it does is
//! turn timers and D-Bus signals into those events.
//!
//! ## What must not fire it
//!
//! A watchdog that fires when nothing is wrong costs a child their session
//! mid-activity, for a reason nothing on screen explains. Three cases are
//! handled deliberately rather than left to the deadline:
//!
//! * **Suspend.** A sleeping machine is not a wedged one, and both ends measure
//!   time with a clock that stops during suspend anyway — but "probably fine"
//!   is not a property to ship on a device that sleeps nightly. logind's
//!   `PrepareForSleep` disarms this outright and re-arms it on resume with a
//!   full fresh deadline, so the first beat after a resume is never late.
//! * **A reconnect.** Losing the connection starts a short settle
//!   ([`CLOSE_SETTLE`]) rather than firing at once, so a client that reconnects
//!   — the custodian restarted, a write timed out — cancels it by arriving.
//! * **An unresolved session.** This is never constructed without a session
//!   that resolved to exactly one id; `resolve` refuses to guess between two,
//!   and a watchdog that guessed during a `switch user` would log out whoever
//!   was next.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// How long the custodian waits for a heartbeat before it acts.
///
/// shepherdd beats every five seconds — a quarter of this, capped — so twelve
/// beats have to go missing. Deliberately loose: the cost of being early is a
/// child returned to the greeter mid-activity with unsaved work gone, and the
/// cost of being late is a minute of an unsupervised session that is about to
/// end anyway. A `kill` does not wait it out in any case; that is an EOF, and
/// [`CLOSE_SETTLE`] is what times it.
pub const BEAT_DEADLINE: Duration = Duration::from_secs(60);

/// How long to wait after the last supervision connection closes.
///
/// Short, because an EOF means the process is *gone* — the kernel closed its
/// descriptors, which is not something a wedged daemon does. Not zero, because
/// a client that lost the connection and reconnects should cancel this by
/// arriving, and because a session that is ending on its own should be allowed
/// to finish ending without this racing it.
pub const CLOSE_SETTLE: Duration = Duration::from_secs(5);

/// How long the *first* beat may take, measured from the connection opening.
///
/// Twice the ordinary deadline, because the first beat is the one with
/// a whole daemon startup in front of it: shepherdd opens this channel while it
/// is still opening a database, registering a GATT service and starting an HTTP
/// server, and its engine tick — which is what a beat attests — does not run
/// until all of that is done. Holding the first beat to the steady-state
/// deadline would make a slow boot look exactly like a killed daemon, on a
/// device where the difference is a child's session.
///
/// Nothing is unguarded during it: an activity cannot launch before the engine
/// that would launch it exists.
const FIRST_BEAT_GRACE: Duration = Duration::from_secs(120);

/// How long to wait for a terminated session to actually go before escalating.
const KILL_AFTER: Duration = Duration::from_secs(10);

/// What the guard was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A supervision connection was opened. Arms the guard.
    Opened,
    /// A heartbeat arrived on one.
    Beat,
    /// A supervision connection ended.
    Closed,
    /// logind says the machine is about to suspend.
    Suspending,
    /// logind says it has resumed.
    Resumed,
}

/// Why the guard fired, which is the first thing anyone reading the journal
/// afterwards needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Nothing holds a supervision connection any more: shepherdd exited or
    /// was killed.
    Gone,
    /// The connection is open and has gone quiet: stopped, wedged, or no longer
    /// ticking.
    Silent,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => write!(f, "shepherdd is gone"),
            Self::Silent => write!(f, "shepherdd stopped sending heartbeats"),
        }
    }
}

/// What the driver should do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// A beat arrived with little of the deadline left. Not a failure — a
    /// measurement, logged so the real margin on a real device is observable
    /// before it costs someone a session rather than after.
    NearMiss {
        remaining: Duration,
    },
    /// End the session.
    Fire(Reason),
}

/// The watchdog's state, with no I/O in it.
#[derive(Debug)]
pub struct Guard {
    beat_deadline: Duration,
    close_settle: Duration,
    /// How much of the deadline may be left when a beat arrives before it is
    /// worth a log line. A fraction of the deadline rather than a constant, so
    /// tuning one tunes both.
    near_miss: Duration,
    connections: usize,
    armed: bool,
    /// Whether a beat has ever arrived. Until one has, the client is still
    /// starting up and gets [`FIRST_BEAT_GRACE`] rather than the deadline.
    beaten: bool,
    suspended: bool,
    fired: bool,
    deadline: Option<(Instant, Reason)>,
}

impl Guard {
    pub fn new(beat_deadline: Duration, close_settle: Duration) -> Self {
        Self {
            beat_deadline,
            close_settle,
            near_miss: beat_deadline / 3,
            connections: 0,
            armed: false,
            beaten: false,
            suspended: false,
            fired: false,
            deadline: None,
        }
    }

    /// When the driver should wake up, if it should.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline.map(|(at, _)| at)
    }

    /// Whether losing supervision now would end the session.
    ///
    /// Only the tests ask: the driver acts on [`Action`], and a caller that
    /// asked this and then acted on the answer would be racing the deadline it
    /// just read.
    #[cfg(test)]
    pub fn armed(&self) -> bool {
        self.armed && !self.fired
    }

    pub fn on_event(&mut self, event: Event, now: Instant) -> Action {
        if self.fired {
            return Action::Nothing;
        }
        match event {
            Event::Opened => {
                self.connections += 1;
                self.armed = true;
                self.set_deadline(now, self.window(), Reason::Silent);
                Action::Nothing
            }
            Event::Beat => {
                // Only measured against a steady-state deadline: a first beat
                // that used most of the startup grace is not a near miss, it is
                // a boot.
                let near_miss = self
                    .beaten
                    .then(|| self.remaining(now))
                    .flatten()
                    .filter(|left| *left < self.near_miss);
                self.beaten = true;
                self.set_deadline(now, self.beat_deadline, Reason::Silent);
                match near_miss {
                    Some(remaining) => Action::NearMiss { remaining },
                    None => Action::Nothing,
                }
            }
            Event::Closed => {
                self.connections = self.connections.saturating_sub(1);
                if self.connections == 0 && self.armed {
                    // A settle rather than an immediate fire, so a client that
                    // is reconnecting cancels it by arriving.
                    self.set_deadline(now, self.close_settle, Reason::Gone);
                }
                Action::Nothing
            }
            Event::Suspending => {
                self.suspended = true;
                self.deadline = None;
                Action::Nothing
            }
            Event::Resumed => {
                self.suspended = false;
                if self.armed {
                    // A *full* deadline, not the remainder of the one that was
                    // running, and the whole window even with no connection:
                    // whatever the client was doing when the machine went to
                    // sleep, it deserves the whole window to say so again.
                    let reason = if self.connections == 0 {
                        Reason::Gone
                    } else {
                        Reason::Silent
                    };
                    self.set_deadline(now, self.window(), reason);
                }
                Action::Nothing
            }
        }
    }

    /// Called when the deadline may have passed.
    pub fn poll(&mut self, now: Instant) -> Action {
        if self.fired {
            return Action::Nothing;
        }
        match self.deadline {
            Some((at, reason)) if now >= at => {
                self.fired = true;
                self.deadline = None;
                Action::Fire(reason)
            }
            _ => Action::Nothing,
        }
    }

    /// How long to allow, which is the startup grace until the client has
    /// proved it is past its startup.
    fn window(&self) -> Duration {
        if self.beaten {
            self.beat_deadline
        } else {
            FIRST_BEAT_GRACE.max(self.beat_deadline)
        }
    }

    fn set_deadline(&mut self, now: Instant, after: Duration, reason: Reason) {
        if self.suspended {
            self.deadline = None;
            return;
        }
        self.deadline = Some((now + after, reason));
    }

    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|(at, _)| at.saturating_duration_since(now))
    }
}

/// What the connection loop holds: a way to report events, and the answer to
/// "will anything happen if I die" that every supervision connection is
/// greeted with.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Event>,
    armed: bool,
    caveat: Option<String>,
    deadline: Duration,
}

impl Handle {
    /// `armed` is whether losing supervision will *actually* end the session,
    /// and `caveat` is what to say when there is something to say — which is
    /// not the same question: polkit refusing and polkit not answering are
    /// different answers, and so is having no way to reach logind at all.
    pub fn new(
        tx: mpsc::UnboundedSender<Event>,
        armed: bool,
        caveat: Option<String>,
        deadline: Duration,
    ) -> Self {
        Self {
            tx,
            armed,
            caveat,
            deadline,
        }
    }

    /// What a new supervision connection is told, once.
    pub fn reply(&self) -> shepherd_state_proto::SuperviseReply {
        shepherd_state_proto::SuperviseReply {
            armed: self.armed,
            reason: self.caveat.clone(),
            deadline: self.deadline,
        }
    }

    /// Report an event. A closed channel means the driver is gone, which can
    /// only happen while the daemon is shutting down; there is nothing useful
    /// to do about it here and nothing to be gained by saying so per beat.
    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    /// Report [`Event::Opened`] now and [`Event::Closed`] when the returned
    /// value is dropped.
    ///
    /// A guard object rather than two calls, because the `Closed` half is the
    /// entire mechanism: every way a connection can end — EOF, a decode error,
    /// a task unwinding — has to report it, and the one that would be forgotten
    /// is the one that is not on the happy path.
    pub fn connection(&self) -> Connected {
        self.send(Event::Opened);
        Connected {
            handle: self.clone(),
        }
    }
}

/// One live supervision connection, from the guard's point of view.
pub struct Connected {
    handle: Handle,
}

impl Connected {
    pub fn beat(&self) {
        self.handle.send(Event::Beat);
    }
}

impl Drop for Connected {
    fn drop(&mut self) {
        self.handle.send(Event::Closed);
    }
}

/// What the watchdog ends, named once.
///
/// Two identifiers because the two steps reach different things: a session id
/// ends the *session*, and a uid ends everything the user is running. See
/// [`Terminator::kill_user`] for why the second is not a stronger version of the
/// first.
#[derive(Debug, Clone)]
pub struct Target {
    /// The session `resolve` settled on, carried from there rather than looked
    /// up again — the filter that produced it is the security-relevant part.
    pub session: String,
    /// The kiosk uid, for the escalation.
    pub uid: u32,
}

/// Ending a session, as an interface, so the state machine's decisions can be
/// tested without logind and the D-Bus calls have one implementation.
pub trait Terminator: Send + Sync + 'static {
    /// Ask logind to end the session.
    fn terminate<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;

    /// Kill everything the kiosk user is running, when asking did not work.
    ///
    /// **Not `KillSession`**, which would be the obvious pair to `terminate`
    /// and is the wrong call. A session scope holds sway, the launcher, the HUD
    /// and swayidle; the activities shepherd launches are somewhere else
    /// entirely — `shepherd-<id>.scope` under the user manager's `app.slice`,
    /// or a snap's or flatpak's own scope (measured on a device,
    /// `docs/ai/history/2026-08-29 003`). Killing the session scope in the one
    /// case this escalation exists for would take the compositor and leave the
    /// game running.
    ///
    /// `KillUser` covers both, and costs nothing to reach: it is the same
    /// polkit action (`org.freedesktop.login1.manage`) the terminate already
    /// needs, so there is no second grant and no wider rule.
    ///
    /// A firewalled Process entry is a system-manager scope rather than one of
    /// this uid's units, and used to be out of reach of this too. It is not any
    /// more: the helper now creates it with `--slice=user-<uid>.slice`, so it
    /// is inside the slice this kills (issue #172,
    /// `shepherd-firewall-helper::lifetime_args`).
    fn kill_user(&self, uid: u32) -> BoxFuture<'_, anyhow::Result<()>>;
}

/// Drive the guard: events in, terminations out.
///
/// Ends when the channel closes, or once it has fired and escalated. On a
/// device it usually never ends at all — the session goes away, `watch_for_loss`
/// returns, and the process exits with this task still in it.
pub async fn run(
    mut rx: mpsc::UnboundedReceiver<Event>,
    mut guard: Guard,
    target: Target,
    terminator: Arc<dyn Terminator>,
) {
    loop {
        let action = tokio::select! {
            event = rx.recv() => match event {
                Some(event) => guard.on_event(event, Instant::now()),
                // Nothing can send any more, which means the daemon is shutting
                // down. Not a reason to end a session.
                None => return,
            },
            () = sleep_until(guard.deadline()) => guard.poll(Instant::now()),
        };

        match action {
            Action::Nothing => {}
            Action::NearMiss { remaining } => warn!(
                remaining_ms = remaining.as_millis() as u64,
                "A heartbeat arrived with little of the deadline left; the margin is thinner \
                 than it should be"
            ),
            Action::Fire(reason) => {
                error!(
                    session = %target.session,
                    %reason,
                    "Nothing is supervising this session; ending it"
                );
                if let Err(e) = terminator.terminate(&target.session).await {
                    error!(
                        session = %target.session,
                        error = %e,
                        "Could not terminate the session"
                    );
                }
                // If logind honoured it, the session goes away, `watch_for_loss`
                // returns and this process exits — taking this task with it
                // before the sleep finishes. Reaching the other side of it means
                // the session is still there, and so is everything in it.
                tokio::time::sleep(KILL_AFTER).await;
                warn!(
                    session = %target.session,
                    uid = target.uid,
                    "The session is still here; killing everything this user is running"
                );
                if let Err(e) = terminator.kill_user(target.uid).await {
                    error!(uid = target.uid, error = %e, "Could not kill the user's processes");
                }
                info!(session = %target.session, "The watchdog has done what it can");
                return;
            }
        }
    }
}

/// `tokio::time::sleep_until`, or never.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shipped values, so the ratios these tests lean on — the startup grace
    // against the deadline, the settle against both — are the ones a device
    // gets rather than a set chosen to make the arithmetic tidy.
    const BEAT: Duration = BEAT_DEADLINE;
    const SETTLE: Duration = CLOSE_SETTLE;

    fn guard() -> (Guard, Instant) {
        (Guard::new(BEAT, SETTLE), Instant::now())
    }

    #[test]
    fn nothing_fires_before_anything_supervises() {
        // The custodian is reachable by root as well as by shepherdd, so it can
        // be connected to without anything being supervised. An unarmed guard
        // has no deadline and cannot fire.
        let (mut g, t0) = guard();
        assert_eq!(g.deadline(), None);
        assert!(!g.armed());
        assert_eq!(g.poll(t0 + BEAT * 10), Action::Nothing);
    }

    #[test]
    fn a_killed_shepherdd_ends_the_session() {
        // The defect this exists for: the connection is gone, and after the
        // settle the session goes with it.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Closed, t0);
        assert_eq!(g.poll(t0 + SETTLE / 2), Action::Nothing);
        assert_eq!(g.poll(t0 + SETTLE), Action::Fire(Reason::Gone));
    }

    #[test]
    fn a_stopped_shepherdd_ends_the_session_too() {
        // `SIGSTOP` holds every file descriptor open, so there is no EOF to
        // notice — only the silence.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Beat, t0 + Duration::from_secs(5));
        assert_eq!(g.poll(t0 + Duration::from_secs(20)), Action::Nothing);
        assert_eq!(
            g.poll(t0 + Duration::from_secs(5) + BEAT),
            Action::Fire(Reason::Silent)
        );
    }

    #[test]
    fn beats_keep_it_quiet_indefinitely() {
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        let mut now = t0;
        for _ in 0..100 {
            now += BEAT / 4;
            assert_eq!(g.on_event(Event::Beat, now), Action::Nothing);
            assert_eq!(g.poll(now), Action::Nothing);
        }
    }

    #[test]
    fn a_reconnect_cancels_the_settle() {
        // The client's connection can break while the client is perfectly
        // alive — a custodian restart, a write timeout. Arriving again inside
        // the settle window has to call that off, or a transient becomes a
        // logout.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Closed, t0);
        g.on_event(Event::Beat, t0);
        let back = t0 + SETTLE / 2;
        g.on_event(Event::Opened, back);
        // The settle it would have fired at comes and goes.
        assert_eq!(g.poll(t0 + SETTLE), Action::Nothing);
        // What runs from the reconnect is a full beat deadline, not the
        // remainder of anything.
        assert_eq!(
            g.poll(back + BEAT - Duration::from_secs(1)),
            Action::Nothing
        );
        assert_eq!(g.poll(back + BEAT), Action::Fire(Reason::Silent));
    }

    #[test]
    fn a_second_connection_does_not_disarm_on_the_first_close() {
        // Two supervision connections is not an arrangement shepherdd uses, but
        // the reconnect above can briefly produce one, and closing the older of
        // the two must not read as "nothing is supervising".
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Closed, t0);
        // Still one connection: the deadline is the beat deadline, not the
        // settle.
        assert_eq!(
            g.poll(t0 + SETTLE + Duration::from_secs(1)),
            Action::Nothing
        );
    }

    #[test]
    fn a_suspend_that_outlasts_the_deadline_changes_nothing() {
        // The false positive that would reach a child first: a device asleep
        // all night, woken to a session that was terminated for not beating
        // while nothing was running at all.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Beat, t0);
        g.on_event(Event::Suspending, t0 + Duration::from_secs(1));
        assert_eq!(g.deadline(), None);
        let morning = t0 + Duration::from_secs(8 * 60 * 60);
        assert_eq!(g.poll(morning), Action::Nothing);
        g.on_event(Event::Resumed, morning);
        // A *full* deadline from the resume, so the first beat afterwards is
        // never late.
        assert_eq!(
            g.poll(morning + BEAT - Duration::from_secs(1)),
            Action::Nothing
        );
        assert_eq!(g.poll(morning + BEAT), Action::Fire(Reason::Silent));
    }

    #[test]
    fn a_connection_lost_during_a_suspend_still_gets_the_full_window() {
        // Waking up is exactly when a client is most likely to have to
        // reconnect, so the resume window is the beat deadline rather than the
        // settle, and it says the honest reason if it does run out.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Beat, t0);
        g.on_event(Event::Suspending, t0);
        g.on_event(Event::Closed, t0);
        let morning = t0 + Duration::from_secs(8 * 60 * 60);
        g.on_event(Event::Resumed, morning);
        assert_eq!(
            g.poll(morning + SETTLE + Duration::from_secs(1)),
            Action::Nothing
        );
        assert_eq!(g.poll(morning + BEAT), Action::Fire(Reason::Gone));
    }

    #[test]
    fn a_late_beat_is_reported_before_it_becomes_a_logout() {
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Beat, t0);
        assert_eq!(g.on_event(Event::Beat, t0 + BEAT / 2), Action::Nothing);
        let late = t0 + BEAT / 2 + BEAT - Duration::from_secs(2);
        assert!(matches!(
            g.on_event(Event::Beat, late),
            Action::NearMiss { .. }
        ));
    }

    #[test]
    fn a_slow_startup_is_not_a_dead_daemon() {
        // shepherdd opens this channel while it is still starting: a database
        // to open, a GATT service to register, an HTTP server to bring up. Its
        // engine tick, which is what a beat attests, runs after all of that. A
        // first beat held to the steady-state deadline would make a slow boot
        // and a killed daemon look identical.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        let late = t0 + FIRST_BEAT_GRACE - Duration::from_secs(1);
        assert_eq!(g.poll(late), Action::Nothing);
        // ...and once it has proved it is running, the ordinary deadline
        // applies from the next beat on.
        g.on_event(Event::Beat, late);
        assert_eq!(
            g.poll(late + BEAT - Duration::from_secs(1)),
            Action::Nothing
        );
        assert_eq!(g.poll(late + BEAT), Action::Fire(Reason::Silent));
    }

    #[test]
    fn a_startup_that_dies_still_ends_the_session() {
        // The grace is about *silence*, not about the process: an EOF means the
        // kernel closed the descriptors, which a slow boot does not do.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Closed, t0);
        assert_eq!(g.poll(t0 + SETTLE), Action::Fire(Reason::Gone));
    }

    #[test]
    fn it_fires_once() {
        // The escalation from `TerminateSession` to `KillSession` is the
        // driver's, and it happens once. A guard that re-fired would queue a
        // second termination behind the first for a session already leaving.
        let (mut g, t0) = guard();
        g.on_event(Event::Opened, t0);
        g.on_event(Event::Closed, t0);
        assert_eq!(g.poll(t0 + SETTLE), Action::Fire(Reason::Gone));
        assert_eq!(g.poll(t0 + SETTLE * 10), Action::Nothing);
        assert_eq!(g.on_event(Event::Opened, t0 + SETTLE * 10), Action::Nothing);
        assert!(!g.armed());
    }
}
