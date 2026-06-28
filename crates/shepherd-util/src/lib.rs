//! Shared utilities for shepherdd
//!
//! This crate provides:
//! - ID types (EntryId, SessionId, ClientId)
//! - Time utilities (monotonic time, duration helpers)
//! - Error types
//! - Rate limiting helpers
//! - Default paths for socket, data, and log directories
//! - Analog-stick navigation state machine shared by the launcher UIs

mod android;
mod error;
pub mod gamepad_nav;
mod ids;
mod paths;
mod rate_limit;
mod time;

pub use android::*;
pub use error::*;
pub use ids::*;
pub use paths::*;
pub use rate_limit::*;
pub use time::*;
