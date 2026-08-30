//! IPC layer for shepherdd
//!
//! Provides:
//! - Unix domain socket server
//! - NDJSON (newline-delimited JSON) protocol
//! - Client connection management
//! - Peer authentication by cgroup (issue #144)

mod client;
mod peer;
mod server;

pub use client::*;
pub use peer::{PeerError, PeerPolicy, Rejection, is_delegated_user_cgroup, own_cgroup_path};
pub use server::*;

use thiserror::Error;

/// IPC errors
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Invalid message: {0}")]
    InvalidMessage(String),

    #[error("Server error: {0}")]
    ServerError(String),
}

pub type IpcResult<T> = Result<T, IpcError>;
