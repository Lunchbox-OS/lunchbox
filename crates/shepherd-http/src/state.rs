//! Shared application state for HTTP handlers.
//!
//! The HTTP layer is now a thin adapter around
//! [`shepherd_management::ManagementService`]; this struct is just the
//! Axum-extractable wrapper around the trait object.

use shepherd_management::ManagementService;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub svc: Arc<dyn ManagementService>,
}
