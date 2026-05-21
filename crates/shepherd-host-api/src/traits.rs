//! Host adapter traits

use async_trait::async_trait;
use shepherd_api::{EntryKind, InputCompatMode, InputCompatOptions, WindowAction, WindowInfo};
use shepherd_util::SessionId;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{ExitStatus, HostCapabilities, HostSessionHandle};

/// Errors from host adapter operations
#[derive(Debug, Error)]
pub enum HostError {
    #[error("Spawn failed: {0}")]
    SpawnFailed(String),

    #[error("Stop failed: {0}")]
    StopFailed(String),

    #[error("Unsupported entry kind")]
    UnsupportedKind,

    #[error("Session not found")]
    SessionNotFound,

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type HostResult<T> = Result<T, HostError>;

/// Stop mode for session termination
#[derive(Debug, Clone, Copy)]
pub enum StopMode {
    /// Try graceful stop with timeout, then force
    Graceful { timeout: Duration },
    /// Force immediate termination
    Force,
}

impl Default for StopMode {
    fn default() -> Self {
        Self::Graceful {
            timeout: Duration::from_secs(5),
        }
    }
}

/// Options for spawning a session
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// Capture stdout to log file
    pub capture_stdout: bool,

    /// Capture stderr to log file
    pub capture_stderr: bool,

    /// Log file path (if capturing)
    pub log_path: Option<std::path::PathBuf>,

    /// Request fullscreen (if supported)
    pub fullscreen: bool,

    /// Request foreground focus (if supported)
    pub foreground: bool,

    /// Network firewall rules to apply to the session (if supported)
    pub firewall: Option<FirewallSpec>,

    /// Input compatibility modes to apply (e.g., touch-to-mouse,
    /// gamepad-to-mouse+keyboard). Empty = none. The host adapter is
    /// responsible for any sidecar processes this implies and for spawning
    /// at most one sidecar per mode.
    pub input_compat: Vec<InputCompatMode>,

    /// Per-activity tunables passed through to the sidecars (deadzones,
    /// speeds, etc.). Sidecars use built-in defaults for any field left
    /// `None`.
    pub input_compat_options: InputCompatOptions,
}

/// Network firewall specification for a session.
///
/// Hosts that support network filtering enforce this against the session's
/// process tree (on Linux, via systemd `IPAddressAllow=`/`IPAddressDeny=`
/// scope properties). Hostnames are not resolved at this layer; pair with a
/// browser-side allowlist if hostname matching is needed.
#[derive(Debug, Clone)]
pub struct FirewallSpec {
    /// If true, deny all traffic by default; only `allow` rules pass.
    /// If false, allow all traffic by default; `deny` rules block.
    pub default_deny: bool,
    /// Allow rules (CIDR strings or systemd address tokens like
    /// `any`, `localhost`, `link-local`, `multicast`)
    pub allow: Vec<String>,
    /// Deny rules (applied after `allow`)
    pub deny: Vec<String>,
}

/// Events from the host adapter
#[derive(Debug, Clone)]
pub enum HostEvent {
    /// Process/session has exited
    Exited {
        handle: HostSessionHandle,
        status: ExitStatus,
    },

    /// Window is ready (for UI notification)
    WindowReady { handle: HostSessionHandle },

    /// Spawn failed after handle was created
    SpawnFailed {
        session_id: SessionId,
        error: String,
    },
}

/// Host adapter trait - implemented by platform-specific adapters
#[async_trait]
pub trait HostAdapter: Send + Sync {
    /// Get the capabilities of this host adapter
    fn capabilities(&self) -> &HostCapabilities;

    /// Spawn a new session
    async fn spawn(
        &self,
        session_id: SessionId,
        entry_kind: &EntryKind,
        options: SpawnOptions,
    ) -> HostResult<HostSessionHandle>;

    /// Stop a running session
    async fn stop(&self, handle: &HostSessionHandle, mode: StopMode) -> HostResult<()>;

    /// Subscribe to host events
    fn subscribe(&self) -> mpsc::UnboundedReceiver<HostEvent>;

    /// Optional: set foreground focus (if supported)
    async fn set_foreground(&self, _handle: &HostSessionHandle) -> HostResult<()> {
        Err(HostError::Internal("Not supported".into()))
    }

    /// Optional: set fullscreen mode (if supported)
    async fn set_fullscreen(&self, _handle: &HostSessionHandle) -> HostResult<()> {
        Err(HostError::Internal("Not supported".into()))
    }

    /// Log out the current user session (e.g., exit the Sway compositor session)
    async fn logout(&self) -> HostResult<()> {
        Err(HostError::Internal("Not supported".into()))
    }

    /// Optional: list the windows the compositor is aware of, including any
    /// that have been moved to the scratchpad. Used by the management UI for
    /// debugging the Sway tree.
    async fn list_windows(&self) -> HostResult<Vec<WindowInfo>> {
        Err(HostError::Internal("Not supported".into()))
    }

    /// Optional: perform a debug action (close/hide/show) on a window
    /// identified by the compositor id reported by [`list_windows`].
    async fn act_on_window(&self, _window_id: u64, _action: WindowAction) -> HostResult<()> {
        Err(HostError::Internal("Not supported".into()))
    }

    /// Optional: ensure the shell/launcher is visible
    async fn ensure_shell_visible(&self) -> HostResult<()> {
        Ok(())
    }

    /// Optional: check if the host adapter is healthy
    fn is_healthy(&self) -> bool {
        true
    }
}

/// Per-activity compositor scale override (issue #45).
///
/// While an activity with `xwayland_native_resolution = true` is running,
/// the controller drops the compositor's output scale to 1.0 so XWayland
/// clients see the panel's native pixel grid, then restores it on exit.
/// Both methods are idempotent: calling `restore` when nothing is captured
/// (or `apply` twice in a row) is a no-op so callers don't need to track
/// state.
///
/// Two implementations ship in the workspace:
/// - `XwaylandHidpi` in `shepherdd` — the production sway-backed
///   implementation that also broadcasts `HudScaleChanged` events.
/// - [`NoOpHidpiController`] — for tests and HTTP-only contexts that
///   don't have a compositor.
#[async_trait]
pub trait HidpiController: Send + Sync {
    /// Apply the workaround for the next activity launch.
    async fn apply(&self);
    /// Restore the captured scale (and HUD scale factor).
    async fn restore(&self);
}

/// No-op [`HidpiController`] used in tests and on hosts where the
/// workaround does not apply.
pub struct NoOpHidpiController;

#[async_trait]
impl HidpiController for NoOpHidpiController {
    async fn apply(&self) {}
    async fn restore(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_mode_default() {
        let mode = StopMode::default();
        assert!(
            matches!(mode, StopMode::Graceful { timeout } if timeout == Duration::from_secs(5))
        );
    }
}
