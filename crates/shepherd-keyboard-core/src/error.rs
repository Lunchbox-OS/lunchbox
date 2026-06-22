//! Error type for the keyboard host core.

use std::path::PathBuf;

/// Errors produced while loading a bundle or decoding a gesture.
///
/// Callers should treat *any* of these as a reason to **fail closed**: degrade to
/// tap-only entry with no predictions rather than proceeding with an unverified or
/// unusable decoder (see the safety invariants in the host spec).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The bundle directory does not exist or is not a directory.
    #[error("bundle path {0} is missing or not a directory")]
    BundleMissing(PathBuf),

    /// The decoder rejected the bundle (bad signature, hash mismatch, unsupported
    /// schema, malformed files). Wraps the underlying `shepherd-swipe-core` error.
    #[error("bundle failed verification or was incompatible: {0}")]
    Decoder(#[from] shepherd_swipe_core::Error),
}

/// Convenience result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
