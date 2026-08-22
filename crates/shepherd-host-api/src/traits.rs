//! Host adapter traits

use async_trait::async_trait;
use shepherd_api::{
    BrowserMode, DisplayMode, DisplayState, EntryKind, EntryKindTag, InputCompatMode,
    InputCompatOptions, WindowAction, WindowInfo,
};
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

    /// Supervised-browser policy to materialize for the session (if supported).
    /// The host adapter writes a Chromium managed-policy JSON file and appends
    /// the corresponding Chrome command-line flags before spawning.
    pub browser: Option<BrowserSpec>,

    /// Input compatibility modes to apply (e.g., touch-to-mouse,
    /// gamepad-to-mouse+keyboard). Empty = none. The host adapter is
    /// responsible for any sidecar processes this implies and for spawning
    /// at most one sidecar per mode.
    pub input_compat: Vec<InputCompatMode>,

    /// Per-activity tunables passed through to the sidecars (deadzones,
    /// speeds, etc.). Sidecars use built-in defaults for any field left
    /// `None`.
    pub input_compat_options: InputCompatOptions,

    /// Connectivity check target to hand to the activity itself, resolved
    /// from the entry's `internet` policy (falling back to the service's) and
    /// suppressed by `forward_check = false`. Activity kinds that can act on
    /// it do; the rest ignore it. Today only `media` uses it, to hide
    /// online-only library items while the check fails.
    pub connectivity_check: Option<String>,

    /// How long, in days, a play protects a cached video from being displaced
    /// by a speculative download (`service.media.watched_grace_days`).
    ///
    /// Resolved by the caller, for the same reason as `connectivity_check`:
    /// shepherdd already knows it, and the activity must not have to be told
    /// twice. A media activity and shepherdd's prefetcher write to one cache
    /// directory, so if they disagreed on this they would spend the same disk
    /// by different rules. `None` for every other kind.
    pub media_watched_grace_days: Option<u64>,
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

/// Supervised-browser specification for a session.
///
/// Hosts that support it materialize this into a Chromium [managed-policy
/// JSON][policies] file (`<policy_id>.json`) plus a set of Chrome
/// command-line flags, wrapping the browser through documented controls only.
/// Mirrors `shepherd-config`'s validated `BrowserPolicy`, kept separate so the
/// host-api layer does not depend on the config crate (same split as
/// [`FirewallSpec`]).
///
/// [policies]: https://chromeenterprise.google/policies/
#[derive(Debug, Clone)]
pub struct BrowserSpec {
    /// Stem of the managed-policy JSON filename (the entry id). Sanitized to a
    /// safe filename by the host adapter.
    pub policy_id: String,
    /// On-disk user-data-dir segment (shared across entries with the same id).
    pub profile_id: String,
    /// How Chrome is launched.
    pub mode: BrowserMode,
    /// URL opened on launch, if any.
    pub start_url: Option<String>,
    /// Chromium `URLAllowlist` patterns.
    pub url_allowlist: Vec<String>,
    /// Chromium `URLBlocklist` patterns, applied after the allowlist.
    pub url_blocklist: Vec<String>,
    /// Disable DevTools.
    pub disable_dev_tools: bool,
    /// Disable incognito mode.
    pub disable_incognito: bool,
    /// Block extension installation.
    pub disable_extensions: bool,
    /// Wipe the profile directory after the session ends.
    pub wipe_on_exit: bool,
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

    /// An activity kind's readiness changed — whether activities of that kind
    /// can currently be shown or launched. Used to gate a kind while it warms
    /// up (e.g. Steam finishing its initial load, issue #76). Kinds that never
    /// emit this are treated as always ready.
    KindReadinessChanged { kind: EntryKindTag, ready: bool },

    /// Spawn failed after handle was created
    SpawnFailed {
        session_id: SessionId,
        error: String,
    },

    /// A launch was given up on before it ever produced a running activity.
    ///
    /// Distinct from [`Self::Exited`] because nothing ran: the session must be
    /// ended without charging the child for the wait (issue #135, where two
    /// Steam launches that never started were billed 60s each).
    LaunchFailed {
        handle: HostSessionHandle,
        error: String,
    },

    /// An activity outlived every kill the adapter knows how to send, and its
    /// session has already ended — so nothing is supervising it (issue #136).
    ///
    /// Emitted once when the adapter first gives up, and again with
    /// `resolved: true` if the reconciliation sweep eventually gets rid of it.
    /// Purely informational to the engine: the adapter keeps working on it.
    ActivityEscaped {
        session_id: SessionId,
        pid: u32,
        /// Human-readable name of the activity's command, for the audit log.
        command: String,
        /// True when a previously-escaped activity is finally gone.
        resolved: bool,
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
    /// The counter-scale factor currently in force — 1.0 unless the workaround
    /// is active — for `get_hud_scale` and for shells that connect after the
    /// last `HudScaleChanged` broadcast.
    ///
    /// The factor is otherwise only ever announced as a one-shot event at
    /// launch, so a shell that was not subscribed at that instant (it started
    /// late, or its connection dropped and reconnected mid-activity) would
    /// render un-counter-scaled for the rest of the session with no way to
    /// notice. Same reason [`DisplayController::state`] exists.
    async fn factor(&self) -> f64;
}

/// No-op [`HidpiController`] used in tests and on hosts where the
/// workaround does not apply.
pub struct NoOpHidpiController;

#[async_trait]
impl HidpiController for NoOpHidpiController {
    async fn apply(&self) {}
    async fn restore(&self) {}
    async fn factor(&self) -> f64 {
        1.0
    }
}

/// External-display / docking controller (issue #87).
///
/// Owns the compositor's display arrangement: on boot it records the primary
/// (first-enumerated) output, mirrors it onto any connected external display by
/// default, and toggles to "external only" on request. Threaded into the
/// management service so admins can drive it over HTTP/BLE, exactly like
/// [`HidpiController`].
///
/// Two implementations ship in the workspace:
/// - `DisplayManager` in `shepherdd` — the production sway-backed implementation
///   that also manages the `wl-mirror` process, routes audio, and broadcasts
///   `DisplayModeChanged` events.
/// - [`NoOpDisplayController`] — for tests and HTTP-only contexts that don't
///   have a compositor.
#[async_trait]
pub trait DisplayController: Send + Sync {
    /// The current arrangement, for `get_display_state` and for shells that
    /// connect after the last broadcast.
    async fn state(&self) -> DisplayState;
    /// Switch to `mode` and return the resulting state. A no-op (returns the
    /// unchanged state) when the mode does not apply — e.g. requesting
    /// `ExternalOnly` with no external display connected.
    async fn set_mode(&self, mode: DisplayMode) -> DisplayState;
}

/// No-op [`DisplayController`] for tests and hosts without docking support.
/// Always reports a single internal display with no external monitor.
pub struct NoOpDisplayController;

#[async_trait]
impl DisplayController for NoOpDisplayController {
    async fn state(&self) -> DisplayState {
        DisplayState {
            mode: DisplayMode::SingleInternal,
            primary: None,
            secondary: None,
        }
    }
    async fn set_mode(&self, _mode: DisplayMode) -> DisplayState {
        self.state().await
    }
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
