//! The client half of the session watchdog (issue #172).
//!
//! An activity runs as the kiosk uid, and so does `lunchboxd`. Signal
//! permission is a uid comparison, so any activity can `kill` — or `SIGSTOP` —
//! the daemon that supervises it, and the session carries on with no time
//! accounting, no bedtime, and no audit. #161 made a lunchboxd that *exits*
//! take the session down with it, but the `sh -c` wrapper that does that runs
//! at the kiosk uid too: kill it first and nothing is left to run the fallback.
//!
//! So the thing that reacts lives outside the session, at a uid nothing in it
//! can signal: the custodian. This is the connection it watches.
//!
//! ## Why a connection and not a message
//!
//! Nothing here has to be *sent* for the watchdog to fire. A killed process
//! closes its file descriptors whether it meant to or not, and that EOF is the
//! signal. The heartbeats exist only for the failures that keep the descriptor
//! open — `SIGSTOP`, a wedged runtime — and they are what makes those look the
//! same as a kill from the custodian's side.
//!
//! ## Why the beat comes from the engine tick
//!
//! [`Supervision::beat`] is called from lunchboxd's 100 ms tick, the same loop
//! that decides whether a child's time is up, and this thread refuses to send
//! anything if that loop has gone quiet ([`TICK_STALE`]). A heartbeat emitted
//! by a timer of its own would attest that *a thread* is alive, which is not
//! the property anyone wants from a watchdog: a lunchboxd whose engine has
//! stopped but whose runtime has not is exactly the failure this is for.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::SuperviseReply;
use crate::client::Transport;

/// How stale the engine tick may be before this stops vouching for it.
///
/// The tick runs every 100 ms, so anything approaching this is already
/// pathological. It is deliberately far shorter than the custodian's deadline:
/// this side decides *whether* to vouch, and the custodian decides how long to
/// tolerate not being vouched for. Splitting it that way means the margin that
/// absorbs an ordinary stall lives in one place rather than being the sum of
/// two constants in different crates.
const TICK_STALE: Duration = Duration::from_secs(2);

/// The longest this waits between beats, whatever the custodian's deadline is.
const MAX_BEAT_INTERVAL: Duration = Duration::from_secs(5);

/// How long to wait after a broken connection before dialling again.
///
/// The custodian is socket-activated and its socket unit has a start limit
/// (#161 had to widen it once already), so a client that reconnected in a tight
/// loop could fail the socket for the rest of the boot — turning a transient
/// into exactly the unprotected device this exists to prevent.
const RECONNECT_BACKOFF: [Duration; 4] = [
    Duration::from_millis(500),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
];

/// Shepherd's end of the watchdog.
///
/// Dropping it stops the thread and closes the connection, which the custodian
/// reads as supervision having ended — correct, because it has.
pub struct Supervision {
    /// Milliseconds since [`Self::started`] at the last engine tick.
    last_tick: Arc<AtomicU64>,
    started: Instant,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Supervision {
    /// Open the supervision channel for `user`, and report what the custodian
    /// said about it.
    ///
    /// The reply comes back to the caller rather than being logged here: it
    /// answers "will anything happen if I die", and the only process that can
    /// raise a diagnostic about that is the one asking.
    pub fn start(user: &str) -> std::io::Result<(Self, SuperviseReply)> {
        let transport = Transport::connect_for_user(user)?;
        let (reply, mut stream) = transport.into_supervision_stream()?;

        let started = Instant::now();
        let last_tick = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        // Beat interval from the custodian's deadline, not from a constant this
        // crate holds: the two ship together, but the deadline is the
        // custodian's to choose and this is the one place it has to be honoured.
        let interval = (reply.deadline / 4).min(MAX_BEAT_INTERVAL).max(
            // A deadline so short that a quarter of it is nothing would spin
            // this thread; refuse to go below a tenth of a second.
            Duration::from_millis(100),
        );

        let user = user.to_string();
        let thread_tick = Arc::clone(&last_tick);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut attempt = 0usize;
            loop {
                std::thread::sleep(interval);
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }

                // The tick is the thing being attested. If it has gone quiet,
                // say nothing and let the custodian's deadline run: a beat here
                // would be this thread vouching for a loop it cannot see.
                let elapsed = started.elapsed().as_millis() as u64;
                let last = thread_tick.load(Ordering::Relaxed);
                if last == 0 {
                    // The tick has never run. This channel is opened partway
                    // through startup, before the engine exists, so silence
                    // here is a daemon still starting rather than one that
                    // stopped — and the custodian allows for exactly that with
                    // a longer grace for the first beat. Not warned about, or
                    // every boot would log its way through startup.
                    continue;
                }
                if Duration::from_millis(elapsed.saturating_sub(last)) > TICK_STALE {
                    warn!(
                        stale_ms = elapsed.saturating_sub(last),
                        "The engine tick has gone quiet; not vouching for it"
                    );
                    continue;
                }

                match stream.beat() {
                    Ok(()) => attempt = 0,
                    Err(e) => {
                        // Reconnecting is worth trying: the custodian may have
                        // restarted, and lunchboxd is plainly still alive. The
                        // custodian tolerates a short gap for exactly this
                        // (its settle window), and ends the session if the gap
                        // is not short.
                        let backoff = RECONNECT_BACKOFF[attempt.min(RECONNECT_BACKOFF.len() - 1)];
                        warn!(error = %e, backoff_ms = backoff.as_millis() as u64,
                              "The supervision channel broke; reconnecting");
                        attempt += 1;
                        std::thread::sleep(backoff);
                        if thread_stop.load(Ordering::Relaxed) {
                            return;
                        }
                        match Transport::connect_for_user(&user)
                            .and_then(Transport::into_supervision_stream)
                        {
                            Ok((reply, fresh)) => {
                                debug!(armed = reply.armed, "Supervision channel re-established");
                                stream = fresh;
                                attempt = 0;
                            }
                            Err(e) => {
                                debug!(error = %e, "Could not re-open the supervision channel")
                            }
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                last_tick,
                started,
                stop,
                thread: Some(thread),
            },
            reply,
        ))
    }

    /// Record that the engine ticked. Called from the tick itself, so it is one
    /// atomic store and nothing else — no lock, no syscall, no allocation.
    pub fn beat(&self) {
        self.last_tick
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
}

impl Drop for Supervision {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Deliberately not joined: the thread wakes at most one interval from
        // now, and a shutdown path that blocked on it would make the tidy exit
        // slower than the untidy one.
        self.thread.take();
    }
}
