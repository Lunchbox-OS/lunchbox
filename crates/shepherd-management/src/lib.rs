//! Transport-agnostic management service for shepherdd. See `README.md`.

pub mod error;
pub mod service;
pub mod types;

pub use error::{ManagementError, ManagementResult};
pub use service::{DefaultManagementService, ManagementService};
pub use types::LaunchOutcome;
