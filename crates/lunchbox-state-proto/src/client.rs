//! [`RemoteStore`] — the `Store` lunchboxd uses when its state lives behind the
//! custodian (issue #157) — and the [`Transport`] both it and
//! [`crate::RemoteFiles`] are built on.
//!
//! `RemoteStore` implements the same 26-method [`Store`] trait `SqliteStore`
//! does, so nothing in `lunchbox-core`, `lunchbox-management` or
//! `lunchbox-http` changes: they already hold an `Arc<dyn Store>`.
//!
//! ## Blocking, deliberately
//!
//! `Store` is synchronous and is already called inline from async contexts —
//! there is no `spawn_blocking` around store access today. So this does not
//! introduce blocking-in-async; it changes what is blocked on. A Unix round trip
//! is tens of microseconds, against a SQLite write that fsyncs in milliseconds,
//! so writes plausibly get *faster* and reads stay negligible.
//!
//! What is new is that the thing being waited on can hang where SQLite
//! effectively could not, so every call carries a timeout and a hang surfaces as
//! `StoreError::Database` — which every caller already handles.
//!
//! ## Retrying is not free, and mostly not done
//!
//! A dropped connection is worth reconnecting through. A *resent request* is
//! not: if `add_usage` reached the custodian and only the reply was lost,
//! sending it again counts the time twice, which is exactly the accounting this
//! whole issue exists to protect. So the rule is narrow and mechanical —
//! **retry only when the request provably never left this process**, i.e. the
//! connect or the write itself failed. A failure while reading the reply is
//! returned as an error, never retried.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate};
use lunchbox_api::{AudioOutput, AudioOutputRecord, DailyOverride};
use lunchbox_store::{AuditEvent, StateSnapshot, Store, StoreError, StoreResult, TokenState};
use lunchbox_util::{EntryId, LimitSubject};
use serde::de::DeserializeOwned;
use tracing::debug;

use crate::methods::{trait_ty, wire_send, with_store_methods};
use crate::{HelloReply, PROTO_VERSION, STATE_USER, StateRequest, WireResult};

/// How long any single call may take before it is a failure.
///
/// Generous for what is a local socket round trip, because the cost of being
/// wrong in the tight direction is a device that reports store errors under
/// load, and the cost of being wrong in the loose direction is a stall a person
/// notices and a log line that explains it.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// What a call can fail at: getting there, or what came back.
///
/// Kept apart because the two mean different things to a caller. A transport
/// failure is "the custodian is not answering"; a remote failure is the store's
/// own error, and callers already handle those because SQLite could always
/// fail.
#[derive(Debug)]
pub enum CallError {
    Transport(std::io::Error),
    Remote(StoreError),
}

impl From<CallError> for StoreError {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Transport(e) => StoreError::Database(format!("the state custodian: {e}")),
            CallError::Remote(e) => e,
        }
    }
}

impl From<CallError> for std::io::Error {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Transport(e) => e,
            // `NotFound` has to survive, or a caller cannot tell "there is no
            // admin record yet" from "the custodian is broken".
            CallError::Remote(StoreError::NotFound(m)) => {
                std::io::Error::new(std::io::ErrorKind::NotFound, m)
            }
            CallError::Remote(e) => std::io::Error::other(e.to_string()),
        }
    }
}

/// One connection to the custodian, and the rule for using it.
///
/// Shared by [`RemoteStore`] and [`crate::RemoteFiles`] so there is one
/// reconnect policy, one timeout, and one place the retry rule lives.
pub struct Transport {
    socket: PathBuf,
    /// The uid the socket must be served by. Explicit rather than implied, so
    /// the expectation is visible at the call site and a test can hold the same
    /// check to a different uid instead of skipping it.
    expect_uid: u32,
    conn: Mutex<Option<Connection>>,
}

struct Connection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Transport {
    /// Connect to the custodian serving `user`, and agree on a version.
    pub fn connect_for_user(user: &str) -> std::io::Result<Self> {
        let expect_uid = nix::unistd::User::from_name(STATE_USER)
            .ok()
            .flatten()
            .map(|u| u.uid.as_raw())
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "user {STATE_USER} does not exist, so the state custodian cannot be identified"
                ))
            })?;
        Self::connect_at(crate::socket_path(user), expect_uid)
    }

    /// Connect at an explicit path, served by an explicit uid.
    ///
    /// What [`Self::connect_for_user`] is built from, and what a test uses to
    /// run both halves of the wire as itself. A device always goes through the
    /// former, which fills in the two constants; nothing here reads the
    /// environment.
    pub fn connect_at(socket: PathBuf, expect_uid: u32) -> std::io::Result<Self> {
        let transport = Self {
            socket: socket.clone(),
            expect_uid,
            conn: Mutex::new(None),
        };
        // Handshake now so a version mismatch or a wrong owner is a startup
        // failure, not a surprise at the first call.
        {
            let mut guard = transport.conn.lock().expect("state transport lock");
            *guard = Some(Connection::open(&socket, expect_uid)?);
        }
        let reply: HelloReply = transport
            .call(&StateRequest::Hello {
                proto: PROTO_VERSION,
            })
            .map_err(std::io::Error::from)?;
        if reply.proto != PROTO_VERSION {
            return Err(std::io::Error::other(format!(
                "lunchbox-stated speaks protocol {} and this build speaks {PROTO_VERSION}; \
                 the two ship together, so this is a half-finished upgrade rather than \
                 something to work around",
                reply.proto
            )));
        }
        debug!(socket = %socket.display(), proto = PROTO_VERSION, "Connected to the state custodian");
        Ok(transport)
    }

    /// The socket this transport is talking to, for diagnostics.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Send one request and decode its reply.
    pub fn call<T: DeserializeOwned>(&self, request: &StateRequest) -> Result<T, CallError> {
        let line = serde_json::to_string(request).map_err(|e| {
            CallError::Remote(StoreError::Serialization(format!(
                "encoding a state request: {e}"
            )))
        })?;

        let mut guard = self.conn.lock().expect("state transport lock");

        // First attempt on whatever connection we have, opening one if the last
        // call tore it down.
        let mut sent = match guard.as_mut() {
            Some(conn) => conn.send(&line),
            None => Err(std::io::Error::other("not connected")),
        };

        if let Err(e) = &sent {
            // The request did not leave this process: either there was no
            // connection, or the write failed before completing. Reconnecting
            // and sending once more cannot double-apply anything.
            debug!(error = %e, "State connection unusable; reconnecting");
            *guard = None;
            let mut conn =
                Connection::open(&self.socket, self.expect_uid).map_err(CallError::Transport)?;
            sent = conn.send(&line);
            if sent.is_ok() {
                *guard = Some(conn);
            }
        }

        if let Err(e) = sent {
            *guard = None;
            return Err(CallError::Transport(e));
        }

        // From here the request *has* been sent. A failure now is reported, not
        // retried — see the module header: resending a write that already
        // applied would count usage twice.
        let conn = guard.as_mut().expect("connection present after a send");
        let reply = match conn.receive() {
            Ok(reply) => reply,
            Err(e) => {
                *guard = None;
                return Err(CallError::Transport(e));
            }
        };

        let parsed: WireResult<T> = serde_json::from_str(&reply).map_err(|e| {
            CallError::Remote(StoreError::Serialization(format!(
                "decoding a reply from the state custodian: {e}"
            )))
        })?;
        parsed.into_store().map_err(CallError::Remote)
    }

    /// Turn this transport into a one-way notification stream.
    ///
    /// Consumes it: after `WatchConfig` the connection carries no replies, so
    /// anything that tried to `call` on it would block until the timeout.
    pub fn into_watch_stream(self) -> std::io::Result<WatchStream> {
        let line = serde_json::to_string(&StateRequest::WatchConfig)
            .map_err(|e| std::io::Error::other(format!("encoding the watch request: {e}")))?;
        let mut conn = self
            .conn
            .into_inner()
            .expect("state transport lock")
            .ok_or_else(|| std::io::Error::other("not connected"))?;
        conn.send(&line)?;
        // No timeout on a watch: it is idle by design, and a read timeout would
        // read as the custodian having gone away every few seconds.
        conn.reader
            .get_ref()
            .set_read_timeout(None)
            .map_err(|e| std::io::Error::other(format!("clearing the read timeout: {e}")))?;
        Ok(WatchStream { conn })
    }

    /// Turn this transport into the supervision channel (issue #172).
    ///
    /// Consumes it for the same reason [`Self::into_watch_stream`] does: after
    /// `Supervise` the connection carries exactly one reply and then only
    /// heartbeats, so a `call` on it would wait out its timeout.
    ///
    /// The reply is read here rather than left to the caller because it is the
    /// answer to "will anything happen if I die", and a caller that forgot to
    /// read it would be a caller silently trusting a watchdog that may not be
    /// armed.
    pub fn into_supervision_stream(
        self,
    ) -> std::io::Result<(crate::SuperviseReply, SupervisionStream)> {
        let line = serde_json::to_string(&StateRequest::Supervise)
            .map_err(|e| std::io::Error::other(format!("encoding the supervise request: {e}")))?;
        let mut conn = self
            .conn
            .into_inner()
            .expect("state transport lock")
            .ok_or_else(|| std::io::Error::other("not connected"))?;
        conn.send(&line)?;
        let reply = conn.receive()?;
        let reply: crate::SuperviseReply = serde_json::from_str(&reply)
            .map_err(|e| std::io::Error::other(format!("decoding the supervision reply: {e}")))?;
        // The read timeout stays as it is: nothing else ever arrives on this
        // connection, and the client never reads it again.
        Ok((reply, SupervisionStream { conn }))
    }
}

/// A connection that only carries change notifications.
pub struct WatchStream {
    conn: Connection,
}

impl WatchStream {
    /// Block until the policy changes, or the watch ends.
    pub fn next_change(&mut self) -> std::io::Result<()> {
        self.conn.receive().map(|_| ())
    }
}

/// A connection that only carries heartbeats (issue #172).
///
/// The mirror of [`WatchStream`]: that one only reads, this one only writes.
/// Its value is not what it carries but that it *exists* — the custodian ends
/// the session when this connection stops being fed, and a killed process
/// cannot hold its file descriptors open.
pub struct SupervisionStream {
    conn: Connection,
}

impl SupervisionStream {
    /// Tell the custodian lunchbox is still supervising.
    ///
    /// Keeps the write timeout the connection was opened with: a beat that
    /// blocks forever would leave the custodian waiting on a deadline that
    /// never expires *and* this thread stuck, which is the one combination
    /// where nobody notices anything is wrong.
    pub fn beat(&mut self) -> std::io::Result<()> {
        let line = serde_json::to_string(&StateRequest::Heartbeat)
            .map_err(|e| std::io::Error::other(format!("encoding a heartbeat: {e}")))?;
        self.conn.send(&line)
    }
}

impl Connection {
    fn open(socket: &Path, expect_uid: u32) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        verify_owner(&stream, socket, expect_uid)?;
        stream.set_read_timeout(Some(CALL_TIMEOUT))?;
        stream.set_write_timeout(Some(CALL_TIMEOUT))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            reader,
            writer: stream,
        })
    }

    fn send(&mut self, line: &str) -> std::io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }

    fn receive(&mut self) -> std::io::Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the state custodian closed the connection",
            ));
        }
        Ok(line)
    }
}

/// Refuse a socket that is not owned by the custodian's uid.
///
/// The socket lives in a root-owned directory, so an activity cannot take its
/// name the way it can with lunchboxd's own management socket
/// (`docs/ai/history/2026-08-29 004`, finding 2). This is the belt to that
/// braces: it costs one `getsockopt` and turns a misconfigured directory into a
/// refusal rather than lunchboxd trusting whatever answered.
///
/// Deliberately a *uid* check and not a cgroup one, unlike
/// `lunchbox_ipc::classify_server`: the custodian is outside lunchboxd's cgroup
/// by design, so "is it mine?" is the wrong question here.
fn verify_owner(stream: &UnixStream, socket: &Path, expect_uid: u32) -> std::io::Result<()> {
    use std::os::fd::AsFd;
    let Some(peer) = peer_uid(stream.as_fd()) else {
        return Err(std::io::Error::other(format!(
            "could not read the owner of the state custodian at {}",
            socket.display()
        )));
    };
    // Root is accepted for the same reason the daemon accepts it: an operator
    // running the custodian by hand for debugging is not an attacker, and root
    // could read the files directly anyway.
    if peer == expect_uid || peer == 0 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "the socket at {} is served by uid {peer}, not the expected {expect_uid}",
            socket.display()
        )))
    }
}

/// `SO_PEERCRED`'s uid. Kept local rather than depending on `lunchbox-ipc`,
/// which would pull the whole IPC server into every client of this crate for
/// one `getsockopt`.
fn peer_uid(fd: std::os::fd::BorrowedFd<'_>) -> Option<u32> {
    nix::sys::socket::getsockopt(&fd, nix::sys::socket::sockopt::PeerCredentials)
        .ok()
        .map(|cred| cred.uid())
}

/// A `Store` served by `lunchbox-stated` over a Unix socket.
pub struct RemoteStore {
    transport: Transport,
}

impl RemoteStore {
    /// Connect to the custodian serving `user`'s state.
    ///
    /// Fails rather than degrading: choosing to fall back to an unprotected
    /// local store is the *caller's* decision to make once, at startup, and it
    /// has to be visible in a diagnostic when it happens. A store that quietly
    /// became local would be a downgrade nobody could see.
    pub fn connect(user: &str) -> StoreResult<Self> {
        Ok(Self {
            transport: Transport::connect_for_user(user)
                .map_err(|e| StoreError::Database(e.to_string()))?,
        })
    }

    /// Connect at an explicit path, served by an explicit uid.
    pub fn connect_at(socket: PathBuf, expect_uid: u32) -> StoreResult<Self> {
        Ok(Self {
            transport: Transport::connect_at(socket, expect_uid)
                .map_err(|e| StoreError::Database(e.to_string()))?,
        })
    }

    /// The socket this store is talking to, for diagnostics.
    pub fn socket_path(&self) -> &Path {
        self.transport.socket_path()
    }

    fn call<T: DeserializeOwned>(&self, request: &StateRequest) -> StoreResult<T> {
        self.transport.call(request).map_err(StoreError::from)
    }
}

/// Every `Store` method, from the table in [`crate::methods`].
///
/// One line of generated body each: build the variant, hand it to
/// [`RemoteStore::call`]. `is_healthy` is the exception and is written out
/// below, because it is the only method that cannot report *why* it failed.
macro_rules! define_remote_store {
    ($( $variant:ident => $method:ident ( $( $arg:ident : $mode:ident $($aty:ty)? ),* $(,)? ) -> $ret:ty; )*) => {
        impl Store for RemoteStore {
            $(
                fn $method(&self $(, $arg: trait_ty!($mode $($aty)?) )*) -> StoreResult<$ret> {
                    self.call(&StateRequest::$variant {
                        $( $arg: wire_send!($mode $arg), )*
                    })
                }
            )*

            /// The one method not in the table. It returns a bare `bool`, so
            /// it cannot report *why* it failed, and a broken connection has
            /// to read as unhealthy rather than panicking or pretending.
            fn is_healthy(&self) -> bool {
                self.call::<bool>(&StateRequest::IsHealthy).unwrap_or(false)
            }
        }
    };
}

with_store_methods!(define_remote_store);
