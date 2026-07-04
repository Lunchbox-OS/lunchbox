//! Transport-agnostic error type for management operations.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManagementError {
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Bad request: {0}")]
    BadRequest(String),
    #[error("Forbidden: {0}")]
    Forbidden(String),
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Unprocessable: {0}")]
    Unprocessable(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type ManagementResult<T> = Result<T, ManagementError>;
