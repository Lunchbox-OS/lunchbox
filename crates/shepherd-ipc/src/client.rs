//! IPC client implementation
//!
//! Speaks the JSON-RPC wire format defined in `shepherd_api::Request`
//! / `Response`. The typed helpers below are hand-written for the
//! calls IPC consumers actually make (launcher, HUD, e2e); adding a
//! new one is a mechanical two-line change. Callers that need
//! something more exotic can drop down to [`IpcClient::call`] with a
//! method name and a JSON blob.

use crate::ServerCheck;
use serde::de::DeserializeOwned;
use serde_json::Value;
use shepherd_api::{
    BrightnessInfo, DisplayMode, DisplayState, EntryView, ErrorCode, Event, HealthStatus,
    ReasonCode, Request, Response, ResponseResult, ServiceStateSnapshot, SessionInfo, StopMode,
    VolumeInfo,
};
use shepherd_util::EntryId;
use std::os::fd::AsFd;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::warn;

use crate::{IpcError, IpcResult};

/// IPC Client for connecting to shepherdd
pub struct IpcClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_request_id: u64,
}

impl IpcClient {
    /// Connect to shepherdd, refusing anything that is not this session's own
    /// daemon (issue #144).
    ///
    /// The socket sits in a directory owned by the uid every activity runs as,
    /// so an activity can `unlink()` it and bind its own listener at the same
    /// path. No file mode prevents that — see [`shepherd_ipc::ServerCheck`] —
    /// so the client identifies who answered instead, exactly as the daemon
    /// identifies who called.
    ///
    /// Use [`Self::connect_unverified`] for a client that legitimately lives
    /// outside the session.
    pub async fn connect(socket_path: impl AsRef<Path>) -> IpcResult<Self> {
        let stream = UnixStream::connect(socket_path).await?;

        match crate::classify_server(stream.as_fd()) {
            ServerCheck::Ours => {}
            // `sudo` reaches the daemon from an operator's own login session,
            // which is never shepherd's cgroup — the same exemption the daemon
            // makes for root, for the same reason.
            ServerCheck::Foreign { .. } if nix::unistd::getuid().is_root() => {}
            ServerCheck::Foreign { server, ours } => {
                warn!(
                    server_cgroup_id = server,
                    our_cgroup_id = ours,
                    "Something other than this session's shepherdd answered on the management \
                     socket; refusing to talk to it"
                );
                return Err(IpcError::ServerError(format!(
                    "the process listening on this socket is in cgroup {server}, not this \
                     session's ({ours}); it is not shepherd's daemon"
                )));
            }
            ServerCheck::Unknown(e) => {
                // Refused rather than trusted: an impostor can *cause* this by
                // exiting once the connection is accepted.
                warn!(error = %e, "Could not identify what answered on the management socket");
                return Err(IpcError::ServerError(format!(
                    "could not identify the process listening on this socket: {e}"
                )));
            }
            ServerCheck::SelfUnknown(e) => {
                // Nothing an activity does causes this, and refusing would
                // leave a device with a launcher that will not start.
                warn!(
                    error = %e,
                    "Could not read our own cgroup, so the daemon on this socket was not verified"
                );
            }
        }

        Ok(Self::from_stream(stream))
    }

    /// Connect without checking who answered.
    ///
    /// For clients that legitimately live outside the session's cgroup and
    /// therefore cannot pass the check — an operator's own tooling, and the e2e
    /// harness when it deliberately drives a daemon it placed elsewhere. Never
    /// what the launcher, the HUD or a keybinding one-shot should use: those are
    /// exactly the clients an impostor is worth deceiving.
    pub async fn connect_unverified(socket_path: impl AsRef<Path>) -> IpcResult<Self> {
        Ok(Self::from_stream(UnixStream::connect(socket_path).await?))
    }

    fn from_stream(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            next_request_id: 1,
        }
    }

    /// Low-level call: send a method name + JSON params, get back the
    /// full `Response`. Prefer the typed helpers below when they
    /// exist.
    pub async fn call_raw(&mut self, method: &str, params: Value) -> IpcResult<Response> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let request = Request::with_params(request_id, method, params);
        let mut json = serde_json::to_string(&request)?;
        json.push('\n');
        self.writer.write_all(json.as_bytes()).await?;

        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(IpcError::ConnectionClosed);
        }
        let response: Response = serde_json::from_str(line.trim())?;
        Ok(response)
    }

    /// Typed call: like `call_raw` but decodes the JSON result into
    /// `T` on success, and turns any server error into a
    /// `ServerError` on the client side.
    pub async fn call<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> IpcResult<T> {
        let response = self.call_raw(method, params).await?;
        match response.result {
            ResponseResult::Ok(value) => serde_json::from_value(value).map_err(IpcError::Json),
            ResponseResult::Err(e) => Err(IpcError::ServerError(e.message)),
        }
    }

    // ---------------------------------------------------------------
    // Typed helpers — one per RPC that IPC consumers use.
    // Wire names must match `docs/rpc-schema.json`.
    // ---------------------------------------------------------------

    pub async fn ping(&mut self) -> IpcResult<()> {
        self.call::<Value>("ping", Value::Null).await.map(|_| ())
    }

    pub async fn health(&mut self) -> IpcResult<HealthStatus> {
        self.call("health", Value::Null).await
    }

    pub async fn service_state(&mut self) -> IpcResult<ServiceStateSnapshot> {
        self.call("service_state", Value::Null).await
    }

    pub async fn list_entries(&mut self) -> IpcResult<Vec<EntryView>> {
        self.call("list_entries", serde_json::json!({})).await
    }

    pub async fn current_session(&mut self) -> IpcResult<Option<SessionInfo>> {
        self.call("current_session", Value::Null).await
    }

    /// Launch an entry. On approval returns the new session_id; on
    /// policy denial returns the reason codes wrapped in `Denied`.
    /// Any lower-layer failure (entry not found, spawn error, etc.)
    /// bubbles as a normal `IpcError::ServerError`.
    pub async fn launch(&mut self, entry_id: EntryId) -> IpcResult<LaunchOutcome> {
        self.call("launch", serde_json::json!({ "id": entry_id }))
            .await
    }

    pub async fn stop_current(&mut self, mode: StopMode) -> IpcResult<()> {
        self.call::<Value>("stop_current", serde_json::json!({ "mode": mode }))
            .await
            .map(|_| ())
    }

    /// Send a "back" navigation to the current session (Android `KEYCODE_BACK`).
    pub async fn back(&mut self) -> IpcResult<()> {
        self.call::<Value>("back", Value::Null).await.map(|_| ())
    }

    /// The compositor's windows (issue #154), for the administrator taskbar.
    pub async fn list_windows(&mut self) -> IpcResult<Vec<shepherd_api::WindowInfo>> {
        self.call::<Vec<shepherd_api::WindowInfo>>("list_windows", Value::Null)
            .await
    }

    /// Focus, close, hide or show one of them.
    pub async fn act_on_window(
        &mut self,
        id: u64,
        action: shepherd_api::WindowAction,
    ) -> IpcResult<()> {
        self.call::<Value>(
            "act_on_window",
            serde_json::json!({ "id": id, "action": action }),
        )
        .await
        .map(|_| ())
    }

    /// Leave administrator mode (issue #154). Offered by the HUD only once
    /// every window is closed; the management clients can always do it.
    ///
    /// **This logs the desktop session out**, which is what makes leaving a
    /// reset rather than a flag flip — nothing tracks what the mode started, so
    /// the session going away is the only guarantee the child's next activity
    /// gets the machine it would have got at boot. Expect this connection to
    /// die shortly after the reply.
    pub async fn exit_admin_mode(&mut self) -> IpcResult<()> {
        self.call::<Value>("exit_admin_mode", Value::Null)
            .await
            .map(|_| ())
    }

    /// Every application the system's `.desktop` files offer (issue #154),
    /// for administrator mode's picker.
    pub async fn list_desktop_apps(&mut self) -> IpcResult<Vec<shepherd_api::DesktopApp>> {
        self.call::<Vec<shepherd_api::DesktopApp>>("list_desktop_apps", Value::Null)
            .await
    }

    /// Start one of them by desktop file ID. Refused unless administrator mode
    /// is on.
    pub async fn launch_desktop_app(&mut self, id: &str) -> IpcResult<()> {
        self.call::<Value>("launch_desktop_app", serde_json::json!({ "id": id }))
            .await
            .map(|_| ())
    }

    /// Cover the screen while leaving administrator mode's work running
    /// (issue #154). There is no `unlock` counterpart here on purpose: the
    /// screen is opened again from the companion or web app, never from the
    /// device itself.
    pub async fn lock_device(&mut self) -> IpcResult<()> {
        self.call::<Value>("lock_device", Value::Null)
            .await
            .map(|_| ())
    }

    /// Report that the seat has been idle long enough to leave administrator
    /// mode. The daemon decides whether to act; `true` means it left the mode —
    /// and, as with any exit, logged the session out.
    pub async fn admin_idle_timeout(&mut self) -> IpcResult<bool> {
        self.call::<bool>("admin_idle_timeout", Value::Null).await
    }

    /// Reset the running activity to its starting state, keeping the session.
    /// Errors when there is no session or its activity can't be reset.
    pub async fn reset_current(&mut self) -> IpcResult<()> {
        self.call::<Value>("reset_current", serde_json::json!({}))
            .await
            .map(|_| ())
    }

    pub async fn extend_current(
        &mut self,
        by: Duration,
    ) -> IpcResult<Option<chrono::DateTime<chrono::Local>>> {
        #[derive(serde::Deserialize)]
        struct ExtendResult {
            new_deadline: Option<chrono::DateTime<chrono::Local>>,
        }
        let out: ExtendResult = self
            .call(
                "extend_current",
                serde_json::json!({ "seconds": by.as_secs() as i64 }),
            )
            .await?;
        Ok(out.new_deadline)
    }

    pub async fn reload_config(&mut self) -> IpcResult<usize> {
        #[derive(serde::Deserialize)]
        struct ReloadResult {
            entry_count: usize,
        }
        let out: ReloadResult = self.call("reload_config", Value::Null).await?;
        Ok(out.entry_count)
    }

    pub async fn logout(&mut self) -> IpcResult<()> {
        self.call::<Value>("logout", Value::Null).await.map(|_| ())
    }

    pub async fn get_volume(&mut self) -> IpcResult<VolumeInfo> {
        self.call("get_volume", Value::Null).await
    }

    pub async fn set_volume(&mut self, percent: u8) -> IpcResult<VolumeInfo> {
        self.call("set_volume", serde_json::json!({ "percent": percent }))
            .await
    }

    pub async fn set_mute(&mut self, muted: bool) -> IpcResult<VolumeInfo> {
        self.call("set_mute", serde_json::json!({ "muted": muted }))
            .await
    }

    pub async fn volume_up(&mut self, step: u8) -> IpcResult<VolumeInfo> {
        self.call("volume_up", serde_json::json!({ "step": step }))
            .await
    }

    pub async fn volume_down(&mut self, step: u8) -> IpcResult<VolumeInfo> {
        self.call("volume_down", serde_json::json!({ "step": step }))
            .await
    }

    pub async fn toggle_mute(&mut self) -> IpcResult<VolumeInfo> {
        self.call("toggle_mute", Value::Null).await
    }

    pub async fn get_brightness(&mut self) -> IpcResult<BrightnessInfo> {
        self.call("get_brightness", Value::Null).await
    }

    /// The HUD counter-scale factor currently in force. Shells seed this on
    /// connect because `HudScaleChanged` is only broadcast when it changes.
    pub async fn get_hud_scale(&mut self) -> IpcResult<f64> {
        self.call("get_hud_scale", Value::Null).await
    }

    /// The screen edge the HUD should occupy (issue #171). Fetched on every
    /// connect for the same reason as `get_hud_scale`:
    /// `HudOrientationChanged` is only broadcast when it changes, so a HUD
    /// that started late or reconnected mid-session has no other way to learn
    /// that the running activity moved it.
    pub async fn get_hud_orientation(&mut self) -> IpcResult<shepherd_api::HudOrientation> {
        self.call("get_hud_orientation", Value::Null).await
    }

    pub async fn get_display_state(&mut self) -> IpcResult<DisplayState> {
        self.call("get_display_state", Value::Null).await
    }

    pub async fn set_display_mode(&mut self, mode: DisplayMode) -> IpcResult<DisplayState> {
        self.call("set_display_mode", serde_json::json!({ "mode": mode }))
            .await
    }

    pub async fn set_brightness(&mut self, percent: u8) -> IpcResult<BrightnessInfo> {
        self.call("set_brightness", serde_json::json!({ "percent": percent }))
            .await
    }

    pub async fn brightness_up(&mut self, step: u8) -> IpcResult<BrightnessInfo> {
        self.call("brightness_up", serde_json::json!({ "step": step }))
            .await
    }

    pub async fn brightness_down(&mut self, step: u8) -> IpcResult<BrightnessInfo> {
        self.call("brightness_down", serde_json::json!({ "step": step }))
            .await
    }

    /// Turn the displays on or off. Answers `false` when a blank was suppressed
    /// because an activity is on screen (issue #144).
    pub async fn set_screen_power(&mut self, on: bool) -> IpcResult<bool> {
        self.call("set_screen_power", serde_json::json!({ "on": on }))
            .await
    }

    pub async fn set_auto_brightness(&mut self, enabled: bool) -> IpcResult<BrightnessInfo> {
        self.call(
            "set_auto_brightness",
            serde_json::json!({ "enabled": enabled }),
        )
        .await
    }

    pub async fn toggle_auto_brightness(&mut self) -> IpcResult<BrightnessInfo> {
        self.call("toggle_auto_brightness", Value::Null).await
    }

    /// Machine-readable server error code, for callers that need to
    /// distinguish e.g. `NotFound` from `PermissionDenied` (used by
    /// the launcher's error routing). Most consumers can rely on the
    /// `IpcError::ServerError` message.
    pub async fn call_with_code<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> IpcResult<Result<T, (ErrorCode, String)>> {
        let response = self.call_raw(method, params).await?;
        match response.result {
            ResponseResult::Ok(v) => Ok(Ok(serde_json::from_value(v)?)),
            ResponseResult::Err(e) => Ok(Err((e.code, e.message))),
        }
    }

    // ---------------------------------------------------------------
    // Event stream
    // ---------------------------------------------------------------

    /// Subscribe to events and consume this client to return an event
    /// stream. Subscribe is an IPC-side special case (the server
    /// flips a per-client subscription flag *after* the response
    /// frame is written) — it isn't dispatched through the trait.
    pub async fn subscribe(mut self) -> IpcResult<EventStream> {
        let response = self.call_raw("subscribe_events", Value::Null).await?;
        match response.result {
            ResponseResult::Ok(_) => {}
            ResponseResult::Err(e) => {
                return Err(IpcError::ServerError(e.message));
            }
        }
        Ok(EventStream {
            reader: self.reader,
        })
    }
}

/// Client-side mirror of the server's `LaunchOutcome`. Kept as a
/// small stable enum so callers can pattern-match without pulling in
/// `shepherd-management`.
///
/// Externally tagged (`{"Approved": {…}}`), matching how
/// `shepherd_management::LaunchOutcome` serializes. It was previously
/// `#[serde(untagged)]`, which could never match that shape: *every* launch
/// failed to decode, and a `Denied` outcome — the one carrying the reason the
/// child needs to see — was silently dropped on the floor.
#[derive(Debug, Clone, serde::Deserialize)]
pub enum LaunchOutcome {
    Approved {
        session_id: String,
        deadline: Option<chrono::DateTime<chrono::Local>>,
    },
    Denied {
        reasons: Vec<ReasonCode>,
    },
}

/// Stream of events from shepherdd
pub struct EventStream {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
}

impl EventStream {
    /// Wait for the next event
    pub async fn next(&mut self) -> IpcResult<Event> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(IpcError::ConnectionClosed);
        }

        let event: Event = serde_json::from_str(line.trim())?;
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    // Client tests would require a running server; see integration
    // tests under `crates/shepherd-e2e/`.
    //
    // The exception is wire-shape agreement with the server's types, which is
    // pure serde and needs no server at all — and which silently regressed
    // once already (see [`LaunchOutcome`]).
    use super::*;

    /// The exact bytes shepherdd puts on the wire, captured from a headless
    /// `launch` on 2026-08-20. Externally tagged, not untagged.
    const APPROVED_ON_THE_WIRE: &str = r#"{
        "Approved": {
            "session_id": "3288e400-38f6-415e-ad22-f89635794178",
            "deadline": "2026-08-20T22:00:47.706134268-04:00"
        }
    }"#;

    /// Built from the real [`ReasonCode`] so the inner shape cannot drift out
    /// from under this test; the outer `{"Denied": …}` tag is the assertion.
    fn denied_on_the_wire() -> String {
        let reasons = vec![ReasonCode::OutsideTimeWindow {
            next_window_start: None,
        }];
        format!(
            r#"{{"Denied":{{"reasons":{}}}}}"#,
            serde_json::to_string(&reasons).unwrap()
        )
    }

    #[test]
    fn decodes_the_approved_shape_the_server_sends() {
        match serde_json::from_str::<LaunchOutcome>(APPROVED_ON_THE_WIRE) {
            Ok(LaunchOutcome::Approved { session_id, .. }) => {
                assert_eq!(session_id, "3288e400-38f6-415e-ad22-f89635794178");
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    /// The one that actually mattered: a denial has to reach the UI, or a
    /// child who is out of time just sees the launcher do nothing.
    #[test]
    fn decodes_the_denied_shape_the_server_sends() {
        match serde_json::from_str::<LaunchOutcome>(&denied_on_the_wire()) {
            Ok(LaunchOutcome::Denied { reasons }) => assert_eq!(reasons.len(), 1),
            other => panic!("expected Denied, got {other:?}"),
        }
    }
}
