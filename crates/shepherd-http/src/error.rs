//! HTTP API error type

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;
use shepherd_management::ManagementError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Bad request: {0}")]
    BadRequest(String),
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Forbidden: {0}")]
    Forbidden(String),
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Unprocessable: {0}")]
    Unprocessable(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, code) = match &self {
            ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApiError::Unprocessable(_) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable"),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        (
            status,
            Json(json!({ "error": code, "message": self.to_string() })),
        )
            .into_response()
    }
}

impl From<ManagementError> for ApiError {
    fn from(e: ManagementError) -> Self {
        match e {
            ManagementError::NotFound(msg) => ApiError::NotFound(msg),
            ManagementError::BadRequest(msg) => ApiError::BadRequest(msg),
            ManagementError::Forbidden(msg) => ApiError::Forbidden(msg),
            ManagementError::Conflict(msg) => ApiError::Conflict(msg),
            ManagementError::Unprocessable(msg) => ApiError::Unprocessable(msg),
            ManagementError::Internal(msg) => ApiError::Internal(msg),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
