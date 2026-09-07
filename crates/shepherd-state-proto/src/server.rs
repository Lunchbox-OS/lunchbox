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

use crate::methods::{wire_recv, with_store_methods};
use shepherd_store::{Store, StoreError};
use shepherd_util::ProtectedFiles;

use crate::{HelloReply, PROTO_VERSION, StateRequest, WireResult};

/// Handle one request, returning the line to write back.
///
/// Serialisation of the *reply* cannot fail for these types, but the result is
/// still checked rather than unwrapped: a panic here would take the daemon down
/// and, with socket activation, take it down again on the next connection.
/// Every `Store` method, from the table in [`crate::methods`].
///
/// Generated from the same entry as the client method that builds the variant,
/// so an arm cannot call a different store method than the one its variant
/// names -- which is a mismatch that typechecks, and used to be caught only by
/// a test.
///
/// Returns `Err(request)` for anything not in the table, handing it back to
/// [`handle`] rather than duplicating the rest of the match here.
macro_rules! define_store_dispatch {
    ($( $variant:ident => $method:ident ( $( $arg:ident : $mode:ident $($aty:ty)? ),* $(,)? ) -> $ret:ty; )*) => {
        fn dispatch_store(store: &dyn Store, request: StateRequest) -> Result<String, StateRequest> {
            match request {
                $(
                    StateRequest::$variant { $( $arg, )* } => Ok(encode(
                        WireResult::from_store(store.$method( $( wire_recv!($mode $arg) ),* ))
                    )),
                )*
                other => Err(other),
            }
        }
    };
}

with_store_methods!(define_store_dispatch);

pub fn handle(store: &dyn Store, files: &dyn ProtectedFiles, request: StateRequest) -> String {
    // The generated half first; `handle` keeps only what is not a `Store` call.
    let request = match dispatch_store(store, request) {
        Ok(encoded) => return encoded,
        Err(request) => request,
    };

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

        // The one `Store` method outside the table: a bare `bool`, so it has
        // no `StoreResult` to wrap.
        StateRequest::IsHealthy => encode(WireResult::Ok {
            value: store.is_healthy(),
        }),

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

        // Unreachable: `dispatch_store` hands back only what is not a `Store`
        // call, so every remaining variant is named above. Answered rather than
        // `unreachable!`, for the reason `WatchConfig` gives — under socket
        // activation a panic takes the daemon down, and down again on the next
        // connection.
        handled => {
            tracing::error!(
                request = ?handled,
                "A request reached the tail of `handle` after the generated dispatch declined \
                 it; this is a coding mistake, not something a peer can cause"
            );
            encode(WireResult::<()>::Err {
                kind: crate::WireErrorKind::Serialization,
                message: "request not handled".into(),
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
