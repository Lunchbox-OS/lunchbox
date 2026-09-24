//! Protocol types for lunchboxd IPC
//!
//! This crate defines the stable API between lunchboxd and clients:
//! - Commands (requests from clients)
//! - Responses
//! - Events (service -> clients)
//! - Versioning

mod commands;
mod diagnostics;
mod events;
mod network;
mod types;
mod wifi;

pub use commands::*;
pub use diagnostics::*;
pub use events::*;
pub use network::*;
pub use types::*;
pub use wifi::*;

/// Current API version
pub const API_VERSION: u32 = 1;
