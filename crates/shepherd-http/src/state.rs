//! Shared application state for HTTP handlers.
//!
//! The HTTP layer is now a thin adapter around
//! [`shepherd_management::ManagementService`]; this struct is just the
//! Axum-extractable wrapper around the trait object.

use crate::files::FileService;
use shepherd_management::ManagementService;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub svc: Arc<dyn ManagementService>,
    /// Remote file management (issue #195), or `None` where the device has it
    /// switched off in `config.toml`.
    ///
    /// Its own field rather than a method on the trait: nothing about it
    /// reaches BLE or the companion, so it never belonged on
    /// `ManagementService` — see
    /// `docs/ai/history/2026-09-11 004 remote-file-manager-api (#195).md`.
    pub file_manager: Option<Arc<FileService>>,
}
