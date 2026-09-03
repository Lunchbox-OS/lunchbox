//! The wire between `shepherdd` and `shepherd-stated` (issue #157).
//!
//! One request per line, one response per line, NDJSON — the same framing
//! discipline as `shepherd-ipc`, for the same reason: it is inspectable with
//! `socat` when something is wrong at three in the morning.
//!
//! ## This is a private protocol, deliberately
//!
//! It is **not** run through `shepherd-management-macros` / `rpc_codegen`, which
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
//! Requests are one variant per [`shepherd_store::Store`] method. Responses are
//! deliberately *not* a mirrored enum: they are [`WireResult`], generic over
//! whatever that method returns, so the client's 26 trait methods are each one
//! line and there is exactly one "the server said something unexpected" path
//! instead of 26.

use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use shepherd_api::AudioOutput;
use shepherd_store::{AuditEvent, StateSnapshot, StoreError};
use shepherd_util::{EntryId, LimitSubject, ProtectedFile};

pub mod client;
pub mod files;
pub mod server;

pub use client::{CallError, RemoteStore, Transport};
pub use files::{ConfigWatch, RemoteFiles};

/// The protocol both ends must agree on.
///
/// Bump when a variant's meaning changes, not when one is added: an older
/// client simply never sends a new variant, and an older server answers
/// `UnknownRequest` if it somehow does.
pub const PROTO_VERSION: u32 = 1;

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
/// one. `shepherd_util::paths` reads `SHEPHERD_DATA_DIR` and `SHEPHERD_SOCKET`
/// from the environment; nothing here does.
pub fn socket_path(user: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/run/shepherdd/state").join(format!("{user}.sock"))
}

/// One call. Exactly one variant per `Store` method, plus the handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum StateRequest {
    Hello {
        proto: u32,
    },

    // Audit log
    AppendAudit {
        event: Box<AuditEvent>,
    },
    GetRecentAudits {
        limit: usize,
    },

    // Usage accounting
    GetUsage {
        entry_id: EntryId,
        day: NaiveDate,
    },
    AddUsage {
        entry_id: EntryId,
        day: NaiveDate,
        duration: Duration,
    },
    GetUsageRange {
        entry_id: EntryId,
        from: NaiveDate,
        to: NaiveDate,
    },
    GetAllUsageForDate {
        date: NaiveDate,
    },

    // Token balances
    GetTokenState {
        subject: LimitSubject,
        day: NaiveDate,
        carry_over: bool,
    },
    AdjustTokenBalance {
        subject: LimitSubject,
        day: NaiveDate,
        carry_over: bool,
        delta_secs: i64,
    },
    SetTokenRatchet {
        subject: LimitSubject,
        day: NaiveDate,
        carry_over: bool,
    },

    // Cooldowns
    GetCooldownUntil {
        subject: LimitSubject,
    },
    SetCooldownUntil {
        subject: LimitSubject,
        until: DateTime<Local>,
    },
    ClearCooldown {
        subject: LimitSubject,
    },

    // Crash-recovery snapshot
    LoadSnapshot,
    SaveSnapshot {
        snapshot: Box<StateSnapshot>,
    },

    // Health
    IsHealthy,

    // Daily overrides
    GetDailyOverride {
        subject: LimitSubject,
        date: NaiveDate,
    },
    UpsertDailyOverride {
        subject: LimitSubject,
        date: NaiveDate,
        availability: Option<bool>,
        quota_delta_seconds: Option<i64>,
    },
    ClearDailyOverride {
        subject: LimitSubject,
        date: NaiveDate,
    },
    ListDailyOverrides {
        date: NaiveDate,
    },

    // Audio outputs
    RecordAudioOutputSeen {
        output: Box<AudioOutput>,
    },
    SetAudioOutputLimits {
        output_key: String,
        max_volume: Option<u8>,
        min_volume: Option<u8>,
    },
    GetAudioOutput {
        output_key: String,
    },
    ListAudioOutputs,
    ForgetAudioOutput {
        output_key: String,
    },

    // Settings
    GetSetting {
        key: String,
    },
    SetSetting {
        key: String,
        value: String,
    },

    // The protected files: policy, and the BLE admin identity that goes with
    // it. Addressed by a closed set rather than a name — see [`ProtectedFile`].
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
    /// Read and remove in one step, for the factory-reset sentinel.
    TakeFile {
        file: ProtectedFile,
    },

    /// Turn this connection into a notification stream for policy changes.
    ///
    /// After this the connection carries no more requests and no replies: the
    /// custodian writes a [`ConfigChanged`] line whenever the policy file it
    /// owns is written. shepherdd used to watch the file itself with inotify;
    /// it cannot watch a directory it cannot open, so the watch moved to the
    /// side that can.
    ///
    /// A separate connection rather than multiplexing onto the request one,
    /// because a client that had to demultiplex replies from events could no
    /// longer treat "the next line" as its answer — which is what keeps the
    /// request path as simple as it is.
    WatchConfig,
}

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
                | Self::LoadSnapshot
                | Self::IsHealthy
                | Self::GetDailyOverride { .. }
                | Self::ListDailyOverrides { .. }
                | Self::GetAudioOutput { .. }
                | Self::ListAudioOutputs
                | Self::GetSetting { .. }
                | Self::ReadFile { .. }
                | Self::WatchConfig
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
            StateRequest::SetTokenRatchet {
                subject: subject.clone(),
                day,
                carry_over: false,
            },
            StateRequest::SetCooldownUntil {
                subject: subject.clone(),
                until: shepherd_util::now(),
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
