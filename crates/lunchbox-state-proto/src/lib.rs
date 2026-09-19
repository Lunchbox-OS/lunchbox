//! The wire between `lunchboxd` and `lunchbox-stated` (issue #157).
//!
//! One request per line, one response per line, NDJSON — the same framing
//! discipline as `lunchbox-ipc`, for the same reason: it is inspectable with
//! `socat` when something is wrong at three in the morning.
//!
//! ## This is a private protocol, deliberately
//!
//! It is **not** run through `lunchbox-management-macros` / `rpc_codegen`, which
//! exists to keep the TypeScript and Kotlin mirrors of the *management* API
//! honest. Both ends of this wire are Rust, ship in the same package, and are
//! upgraded together. Putting it through the codegen would buy drift protection
//! against a drift that cannot happen, and would land an internal protocol in
//! `docs/rpc-schema.json`, where it would read as public API.
//!
//! ## Versioning refuses rather than negotiates
//!
//! [`StateRequest::Hello`] carries [`PROTO_VERSION`] and a mismatch is an error,
//! not a fallback. The two binaries ship together, so a mismatch means a
//! half-finished upgrade — and the honest response to that is a loud failure,
//! not a compatibility shim that has to be carried forever.
//!
//! ## Shape
//!
//! Requests are one variant per [`lunchbox_store::Store`] method. Responses are
//! deliberately *not* a mirrored enum: they are [`WireResult`], generic over
//! whatever that method returns, so the client's 26 trait methods are each one
//! line and there is exactly one "the server said something unexpected" path
//! instead of 26.

use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use lunchbox_api::AudioOutput;
use lunchbox_store::{AuditEvent, StateSnapshot, StoreError};
use lunchbox_util::{EntryId, LimitSubject, ProtectedFile};

use methods::{wire_ty, with_store_methods};

mod methods;

pub mod client;
pub mod files;
pub mod server;
pub mod supervise;

pub use client::{CallError, RemoteStore, Transport};
pub use files::{ConfigWatch, RemoteFiles};
pub use supervise::Supervision;

/// The protocol both ends must agree on.
///
/// Bump when a variant's meaning changes, not when one is added: an older
/// client simply never sends a new variant, and an older server answers
/// `UnknownRequest` if it somehow does.
///
/// 2: generating the store half from `methods` made the no-argument variants
/// struct variants with no fields rather than unit variants, so `LoadSnapshot`
/// and `ListAudioOutputs` encode as `{"LoadSnapshot":{}}` instead of
/// `"LoadSnapshot"`. Both ends ship together, so this only ever shows up as a
/// half-finished upgrade — which is exactly what the handshake is for.
///
/// 3: the token gate stopped ratcheting (issue #193), so `SetTokenRatchet` is
/// gone and `TokenState` lost its `ratcheted` field. A removal, unlike an
/// addition, is not harmless to an older client: it would fail to decode every
/// token state for want of the field, and treat every gate as locked.
pub const PROTO_VERSION: u32 = 3;

/// The system user that owns the state and answers this socket.
///
/// A constant rather than a setting, and specifically **not** read from the
/// environment: on a device the environment belongs to the kiosk user, who is
/// also every activity (`docs/ai/history/2026-08-29 004`). A client that could
/// be pointed at another socket by an environment variable would be a way to
/// put something else in the custodian's place.
pub const STATE_USER: &str = "shepherd-state";

/// Where the custodian's socket lives for `user`.
///
/// Assembled from a compiled-in constant for the same reason [`STATE_USER`] is
/// one. `lunchbox_util::paths` reads `SHEPHERD_DATA_DIR` and `SHEPHERD_SOCKET`
/// from the environment; nothing here does.
pub fn socket_path(user: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/run/lunchboxd/state").join(format!("{user}.sock"))
}

/// Where the custodian keeps the *device's* protected files — the ones shared
/// by every kiosk user on it.
///
/// The admin record, the unbond queue and the reset sentinel live here rather
/// than under a user, because what they describe is the machine's: there is one
/// Bluetooth adapter and one BlueZ bond table, and forgetting a bond forgets it
/// for everyone. A claim kept per-user while the bond was system-wide gave a
/// two-child device behaviour nobody chose.
///
/// Same uid and same `0700` as the per-user directories, so it is no more
/// reachable from an activity than they are.
pub fn admin_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/var/lib/lunchboxd/admin")
}

/// Where the custodian keeps `user`'s protected files.
///
/// The directory itself is `0700` and owned by [`STATE_USER`], so nothing at
/// the kiosk uid can read what is inside it. Its *parents* are root-owned and
/// world-executable, which is what makes this useful to a client that cannot
/// read it: `lunchboxd` can `stat` this path and learn **whether this device is
/// one the custodian holds state for**, without being able to see the state.
///
/// That distinction is the whole point. A failed connection cannot otherwise be
/// told apart from a device that never had a custodian, and the two want
/// opposite responses — one is a fresh install, the other is protection that
/// has broken and must not be quietly downgraded.
///
/// An activity cannot forge the answer in either direction: the parent is
/// root-owned, so it can neither create this directory nor remove it.
pub fn state_dir(user: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/var/lib/lunchboxd/state").join(user)
}

/// One call. Exactly one variant per `Store` method, plus the file operations
/// and the handshake.
///
/// The store half is generated from the table in [`methods`] rather than
/// written out here: three copies of the same list -- variant, client method,
/// server arm -- is a shape where the compiler cannot see a mismatch, and the
/// one that mattered was an arm calling the wrong store method.
macro_rules! define_state_request {
    ($( $variant:ident => $method:ident ( $( $arg:ident : $mode:ident $($aty:ty)? ),* $(,)? ) -> $ret:ty; )*) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub enum StateRequest {
            /// Version handshake. Always first, and a mismatch is refused
            /// rather than negotiated -- see [`PROTO_VERSION`].
            Hello {
                proto: u32,
            },

            $(
                $variant {
                    $( $arg: wire_ty!($mode $($aty)?), )*
                },
            )*

            /// The one `Store` method outside the table: it returns a bare
            /// `bool`, so a broken connection has to read as unhealthy rather
            /// than as an error.
            IsHealthy,

            // The protected files. Not `Store` methods -- they go through
            // `ProtectedFiles`, whose four operations are few enough to write
            // out.
            ReadFile {
                file: ProtectedFile,
            },
            WriteFile {
                file: ProtectedFile,
                contents: String,
            },
            DeleteFile {
                file: ProtectedFile,
            },
            TakeFile {
                file: ProtectedFile,
            },

            /// Turn this connection into a notification stream for policy
            /// changes. Answered by the connection loop, not by `handle`.
            WatchConfig,

            /// Turn this connection into the supervision channel (issue #172).
            ///
            /// Answered once with a [`SuperviseReply`], after which the client
            /// only writes [`StateRequest::Heartbeat`] and the custodian only
            /// reads. Losing the connection, or going quiet on it, is what
            /// tells the custodian that nothing is supervising the session any
            /// more -- which is the whole mechanism, so it is a *connection*
            /// rather than a request: a killed lunchboxd cannot decline to
            /// close its file descriptors.
            Supervise,

            /// One beat on a [`StateRequest::Supervise`] connection.
            ///
            /// Carries nothing. It means "the loop that decides whether a
            /// child's time is up ran recently", and the only thing the
            /// custodian does with it is push the deadline out.
            Heartbeat,
        }
    };
}

with_store_methods!(define_state_request);

impl StateRequest {
    /// Whether re-sending this request after a failure could change the state
    /// twice.
    ///
    /// The client only ever retries a request it is sure never left the socket.
    /// This exists so that rule is stated in one place and can be *checked*,
    /// rather than living in a comment next to the retry.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::Hello { .. }
                | Self::GetRecentAudits { .. }
                | Self::GetUsage { .. }
                | Self::GetUsageRange { .. }
                | Self::GetAllUsageForDate { .. }
                | Self::GetTokenState { .. }
                | Self::GetCooldownUntil { .. }
                | Self::LoadSnapshot { .. }
                | Self::IsHealthy
                | Self::GetDailyOverride { .. }
                | Self::ListDailyOverrides { .. }
                | Self::GetAudioOutput { .. }
                | Self::ListAudioOutputs { .. }
                | Self::GetSetting { .. }
                | Self::ReadFile { .. }
                | Self::WatchConfig
                | Self::Supervise
                | Self::Heartbeat
        )
    }
}

/// Which `StoreError` the server hit, in a form that survives JSON.
///
/// `StoreError::Io` wraps a `std::io::Error`, which does not serialize, so the
/// wire carries the kind separately and the client rebuilds an error of the
/// right shape. Callers already handle every one of these — SQLite could always
/// fail — so nothing downstream has to learn about the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireErrorKind {
    Database,
    Serialization,
    NotFound,
    Io,
}

impl WireErrorKind {
    pub fn of(err: &StoreError) -> Self {
        match err {
            StoreError::Database(_) => Self::Database,
            StoreError::Serialization(_) => Self::Serialization,
            StoreError::NotFound(_) => Self::NotFound,
            StoreError::Io(_) => Self::Io,
        }
    }

    /// Rebuild a `StoreError` of this kind. `Io` becomes `Other`-flavoured
    /// rather than trying to recover an errno the server did not send.
    pub fn into_error(self, message: String) -> StoreError {
        match self {
            Self::Database => StoreError::Database(message),
            Self::Serialization => StoreError::Serialization(message),
            Self::NotFound => StoreError::NotFound(message),
            Self::Io => StoreError::Io(std::io::Error::other(message)),
        }
    }
}

/// One answer, generic over what the method returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WireResult<T> {
    Ok {
        value: T,
    },
    Err {
        kind: WireErrorKind,
        message: String,
    },
}

impl<T> WireResult<T> {
    pub fn from_store(result: Result<T, StoreError>) -> Self {
        match result {
            Ok(value) => Self::Ok { value },
            Err(e) => Self::Err {
                kind: WireErrorKind::of(&e),
                message: e.to_string(),
            },
        }
    }

    pub fn into_store(self) -> Result<T, StoreError> {
        match self {
            Self::Ok { value } => Ok(value),
            Self::Err { kind, message } => Err(kind.into_error(message)),
        }
    }
}

/// Sent on a [`StateRequest::WatchConfig`] connection when the policy changes.
///
/// Carries nothing: the client re-reads the file, which it must do anyway, and
/// a payload would be a second source of truth for what the policy currently
/// says.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChanged {}

/// What the handshake answers with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloReply {
    pub proto: u32,
}

/// What a [`StateRequest::Supervise`] connection is answered with, once.
///
/// The answer lunchboxd needs is not "did you hear me" but **"will anything
/// happen if I die"**, because a watchdog that cannot fire is worse than none:
/// it looks like one. So the custodian says whether it is actually able to end
/// the session, and lunchboxd -- which is still alive at that moment, and has a
/// diagnostics channel the watchdog will not have when it is needed -- says so
/// out loud if the answer is no.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperviseReply {
    /// Whether losing this connection will end the session.
    pub armed: bool,
    /// Why it will not, when it will not. `None` when `armed`.
    pub reason: Option<String>,
    /// How long the custodian will wait for a heartbeat before it acts, so the
    /// client can pick a beat interval from the deadline rather than from a
    /// constant the two crates would each have to hold a copy of.
    pub deadline: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mutating_request_is_excluded_from_retry() {
        // The retry rule is "only a request that never left the socket", but
        // the client also refuses to retry anything that mutates even when it
        // is sure. Getting `is_read_only` wrong in the permissive direction
        // would silently double-count usage, so the mutating list is spelled
        // out here rather than trusted to the `matches!` above.
        let day = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
        let subject = LimitSubject::entry("x");
        let mutating = vec![
            StateRequest::AddUsage {
                entry_id: EntryId::new("x"),
                day,
                duration: Duration::from_secs(1),
            },
            StateRequest::AdjustTokenBalance {
                subject: subject.clone(),
                day,
                carry_over: false,
                delta_secs: 1,
            },
            StateRequest::SetCooldownUntil {
                subject: subject.clone(),
                until: lunchbox_util::now(),
            },
            StateRequest::ClearCooldown {
                subject: subject.clone(),
            },
            StateRequest::UpsertDailyOverride {
                subject: subject.clone(),
                date: day,
                availability: None,
                quota_delta_seconds: None,
            },
            StateRequest::ClearDailyOverride { subject, date: day },
            StateRequest::SetAudioOutputLimits {
                output_key: "k".into(),
                max_volume: None,
                min_volume: None,
            },
            StateRequest::ForgetAudioOutput {
                output_key: "k".into(),
            },
            StateRequest::SetSetting {
                key: "k".into(),
                value: "v".into(),
            },
        ];
        for req in mutating {
            assert!(
                !req.is_read_only(),
                "{req:?} changes state and must never be retried"
            );
        }
    }

    #[test]
    fn a_request_round_trips_as_one_line() {
        // The framing is newline-delimited, so a request that serialized with
        // an embedded newline would desynchronise the stream for every call
        // after it.
        let req = StateRequest::GetUsage {
            entry_id: EntryId::new("tuxmath"),
            day: NaiveDate::from_ymd_opt(2026, 9, 2).unwrap(),
        };
        let line = serde_json::to_string(&req).expect("serialize");
        assert!(!line.contains('\n'), "framing assumes one line: {line}");
        let back: StateRequest = serde_json::from_str(&line).expect("deserialize");
        assert!(matches!(back, StateRequest::GetUsage { .. }));
    }

    #[test]
    fn an_error_keeps_its_kind_across_the_wire() {
        // `NotFound` and `Database` mean different things to callers, and
        // `StoreError::Io` cannot serialize at all — so the kind travels
        // separately and is rebuilt rather than flattened to a string.
        for (err, kind) in [
            (StoreError::NotFound("nope".into()), WireErrorKind::NotFound),
            (StoreError::Database("boom".into()), WireErrorKind::Database),
            (
                StoreError::Io(std::io::Error::other("disk")),
                WireErrorKind::Io,
            ),
        ] {
            let wire: WireResult<u8> = WireResult::from_store(Err(err));
            let line = serde_json::to_string(&wire).expect("serialize");
            let back: WireResult<u8> = serde_json::from_str(&line).expect("deserialize");
            let rebuilt = back.into_store().expect_err("still an error");
            assert_eq!(WireErrorKind::of(&rebuilt), kind);
        }
    }
}
