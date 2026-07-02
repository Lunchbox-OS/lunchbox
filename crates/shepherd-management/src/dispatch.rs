//! Errors raised by the auto-generated JSON-RPC dispatcher over
//! `ManagementService` (see `shepherd_management_macros::management_rpc`).
//!
//! Kept separate from [`crate::error::ManagementError`] because these
//! errors are about the JSON-RPC framing itself (unknown method name,
//! malformed params, serialisation failure), not about the business
//! semantics of a specific method. Wire-transport crates translate
//! these into whatever their protocol expects — e.g. `shepherd-ble`
//! maps them onto its `ErrorCode` enum with JSON-RPC-style numeric
//! codes.

use crate::error::ManagementError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcDispatchError {
    /// The `method` string didn't match any RPC exposed by the trait.
    #[error("unknown method '{0}'")]
    MethodNotFound(String),
    /// The `params` blob failed to deserialise into the method's
    /// expected shape (missing required field, wrong type, etc.).
    #[error("invalid params: {0}")]
    InvalidParams(String),
    /// A trait method returned an error.
    #[error(transparent)]
    Management(#[from] ManagementError),
    /// The successful trait return value couldn't be encoded back to
    /// JSON. In practice this only happens for programmer errors in a
    /// custom `Serialize` impl.
    #[error("serialization failed: {0}")]
    Serialization(String),
}
