//! Turning a request off the wire into a call on the store it guards.
//!
//! One arm per [`shepherd_state_proto::StateRequest`] variant, and nothing else:
//! no policy, no interpretation, no decision about whether a child may do
//! anything. This daemon is custody, not judgment — the engine that knows what a
//! limit *means* stays in `shepherd-core`, inside shepherdd.
//!
//! Every arm serialises a [`WireResult`], so a store error travels as an error
//! of the same kind rather than as a dropped connection: callers already handle
//! `StoreError`, because SQLite could always fail.

use shepherd_store::{Store, StoreError};
use shepherd_util::ProtectedFiles;

use crate::{HelloReply, PROTO_VERSION, StateRequest, WireResult};

/// Handle one request, returning the line to write back.
///
/// Serialisation of the *reply* cannot fail for these types, but the result is
/// still checked rather than unwrapped: a panic here would take the daemon down
/// and, with socket activation, take it down again on the next connection.
pub fn handle(store: &dyn Store, files: &dyn ProtectedFiles, request: StateRequest) -> String {
    match request {
        StateRequest::Hello { proto } => {
            if proto != PROTO_VERSION {
                // The client refuses on mismatch and stops, so this is the only
                // place the disagreement is visible from the daemon's side.
                tracing::warn!(
                    client_proto = proto,
                    server_proto = PROTO_VERSION,
                    "A client speaks a different protocol version; it will refuse and stop"
                );
            }
            // Answer with our own version rather than echoing theirs, so the
            // client compares two real numbers.
            encode(WireResult::Ok {
                value: HelloReply {
                    proto: PROTO_VERSION,
                },
            })
        }

        StateRequest::AppendAudit { event } => {
            encode(WireResult::from_store(store.append_audit(*event)))
        }
        StateRequest::GetRecentAudits { limit } => {
            encode(WireResult::from_store(store.get_recent_audits(limit)))
        }

        StateRequest::GetUsage { entry_id, day } => {
            encode(WireResult::from_store(store.get_usage(&entry_id, day)))
        }
        StateRequest::AddUsage {
            entry_id,
            day,
            duration,
        } => encode(WireResult::from_store(
            store.add_usage(&entry_id, day, duration),
        )),
        StateRequest::GetUsageRange { entry_id, from, to } => encode(WireResult::from_store(
            store.get_usage_range(&entry_id, from, to),
        )),
        StateRequest::GetAllUsageForDate { date } => {
            encode(WireResult::from_store(store.get_all_usage_for_date(date)))
        }

        StateRequest::GetTokenState {
            subject,
            day,
            carry_over,
        } => encode(WireResult::from_store(
            store.get_token_state(&subject, day, carry_over),
        )),
        StateRequest::AdjustTokenBalance {
            subject,
            day,
            carry_over,
            delta_secs,
        } => encode(WireResult::from_store(
            store.adjust_token_balance(&subject, day, carry_over, delta_secs),
        )),
        StateRequest::SetTokenRatchet {
            subject,
            day,
            carry_over,
        } => encode(WireResult::from_store(
            store.set_token_ratchet(&subject, day, carry_over),
        )),

        StateRequest::GetCooldownUntil { subject } => {
            encode(WireResult::from_store(store.get_cooldown_until(&subject)))
        }
        StateRequest::SetCooldownUntil { subject, until } => encode(WireResult::from_store(
            store.set_cooldown_until(&subject, until),
        )),
        StateRequest::ClearCooldown { subject } => {
            encode(WireResult::from_store(store.clear_cooldown(&subject)))
        }

        StateRequest::LoadSnapshot => encode(WireResult::from_store(store.load_snapshot())),
        StateRequest::SaveSnapshot { snapshot } => {
            encode(WireResult::from_store(store.save_snapshot(&snapshot)))
        }

        StateRequest::IsHealthy => encode(WireResult::Ok {
            value: store.is_healthy(),
        }),

        StateRequest::GetDailyOverride { subject, date } => encode(WireResult::from_store(
            store.get_daily_override(&subject, date),
        )),
        StateRequest::UpsertDailyOverride {
            subject,
            date,
            availability,
            quota_delta_seconds,
        } => encode(WireResult::from_store(store.upsert_daily_override(
            &subject,
            date,
            availability,
            quota_delta_seconds,
        ))),
        StateRequest::ClearDailyOverride { subject, date } => encode(WireResult::from_store(
            store.clear_daily_override(&subject, date),
        )),
        StateRequest::ListDailyOverrides { date } => {
            encode(WireResult::from_store(store.list_daily_overrides(date)))
        }

        StateRequest::RecordAudioOutputSeen { output } => encode(WireResult::from_store(
            store.record_audio_output_seen(&output),
        )),
        StateRequest::SetAudioOutputLimits {
            output_key,
            max_volume,
            min_volume,
        } => encode(WireResult::from_store(store.set_audio_output_limits(
            &output_key,
            max_volume,
            min_volume,
        ))),
        StateRequest::GetAudioOutput { output_key } => {
            encode(WireResult::from_store(store.get_audio_output(&output_key)))
        }
        StateRequest::ListAudioOutputs => {
            encode(WireResult::from_store(store.list_audio_outputs()))
        }
        StateRequest::ForgetAudioOutput { output_key } => encode(WireResult::from_store(
            store.forget_audio_output(&output_key),
        )),

        StateRequest::GetSetting { key } => encode(WireResult::from_store(store.get_setting(&key))),
        StateRequest::SetSetting { key, value } => {
            encode(WireResult::from_store(store.set_setting(&key, &value)))
        }

        StateRequest::ReadFile { file } => encode(WireResult::from_store(
            files.read(file).map_err(as_store_error),
        )),
        StateRequest::WriteFile { file, contents } => encode(WireResult::from_store(
            files.write(file, &contents).map_err(as_store_error),
        )),
        StateRequest::DeleteFile { file } => encode(WireResult::from_store(
            files.delete(file).map_err(as_store_error),
        )),
        StateRequest::TakeFile { file } => encode(WireResult::from_store(
            files.take(file).map_err(as_store_error),
        )),

        StateRequest::WatchConfig => {
            // The connection loop turns this into a notification stream before
            // dispatch, so reaching here is a coding mistake rather than
            // anything a peer can cause. Answer rather than panic: with socket
            // activation a panic would take the daemon down and down again on
            // the next connection.
            encode(WireResult::<()>::Err {
                kind: crate::WireErrorKind::Serialization,
                message: "WatchConfig must be intercepted by the connection loop".into(),
            })
        }
    }
}

/// File errors travel as `StoreError`, so a caller has one error type for
/// everything the custodian can fail at rather than two that mean the same
/// things.
fn as_store_error(e: std::io::Error) -> StoreError {
    match e.kind() {
        std::io::ErrorKind::NotFound => StoreError::NotFound(e.to_string()),
        _ => StoreError::Io(e),
    }
}

/// Serialise a reply, or a reply saying serialisation failed.
fn encode<T: serde::Serialize>(reply: WireResult<T>) -> String {
    serde_json::to_string(&reply).unwrap_or_else(|e| {
        // Only reachable if a store type gains a non-serialisable field, which
        // is a build-time mistake surfacing at runtime — say so rather than
        // closing the connection and leaving the client guessing.
        let fallback: WireResult<()> = WireResult::Err {
            kind: crate::WireErrorKind::Serialization,
            message: format!("encoding a reply: {e}"),
        };
        serde_json::to_string(&fallback).expect("the fallback reply always encodes")
    })
}
