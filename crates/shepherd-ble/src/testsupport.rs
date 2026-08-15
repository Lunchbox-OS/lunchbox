//! Shared test doubles for the crate's unit tests. Compiled only under
//! `#[cfg(test)]`. Lives at the crate root so both `rpc` and `server`
//! tests can drive the dispatcher against the same `ManagementService`
//! mock without each re-declaring the (large) trait impl.

use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDate};
use shepherd_api::{
    BrightnessInfo, BrightnessRestrictions, DailyOverride, DisplayMode, DisplayState, EntryView,
    Event, GroupView, HealthStatus, ServiceStateSnapshot, SessionInfo, StopMode, TokenStatus,
    UsageStat, VolumeInfo, VolumeRestrictions, WindowAction, WindowInfo,
};
use shepherd_management::{LaunchOutcome, ManagementError, ManagementResult, ManagementService};
use shepherd_util::{EntryId, LimitSubject};
use std::time::Duration;
use tokio::sync::broadcast;

use crate::protocol::RpcRequest;

/// Minimal `ManagementService` mock that records `health` calls and
/// returns canned responses for everything else. Methods that the
/// tests don't exercise return cheap defaults or errors.
pub(crate) struct MockSvc {
    pub health_calls: tokio::sync::Mutex<u32>,
}

impl MockSvc {
    pub fn new() -> Self {
        Self {
            health_calls: tokio::sync::Mutex::new(0),
        }
    }
}

#[async_trait]
impl ManagementService for MockSvc {
    async fn health(&self) -> HealthStatus {
        *self.health_calls.lock().await += 1;
        HealthStatus {
            live: true,
            ready: true,
            policy_loaded: true,
            host_adapter_ok: true,
            store_ok: true,
        }
    }
    async fn service_state(&self) -> ServiceStateSnapshot {
        unreachable!()
    }
    async fn list_entries(&self, _at: DateTime<Local>) -> Vec<EntryView> {
        vec![]
    }
    async fn get_entry(&self, id: &EntryId, _at: DateTime<Local>) -> ManagementResult<EntryView> {
        Err(ManagementError::NotFound(format!("no entry '{id}'")))
    }
    async fn current_session(&self) -> Option<SessionInfo> {
        None
    }
    async fn launch(&self, _id: EntryId) -> ManagementResult<LaunchOutcome> {
        Ok(LaunchOutcome::Denied { reasons: vec![] })
    }
    async fn stop_current(&self, _mode: StopMode) -> ManagementResult<()> {
        Err(ManagementError::NotFound("none".into()))
    }
    async fn extend_current(&self, _seconds: i64) -> ManagementResult<Option<DateTime<Local>>> {
        Ok(None)
    }
    async fn list_groups(&self, _at: DateTime<Local>) -> Vec<GroupView> {
        vec![]
    }
    async fn adjust_tokens(
        &self,
        _id: &LimitSubject,
        delta_seconds: i64,
    ) -> ManagementResult<TokenStatus> {
        Ok(TokenStatus {
            balance: Duration::from_secs(delta_seconds.max(0) as u64),
            minimum: Duration::ZERO,
            unlocked: delta_seconds > 0,
            max_balance: None,
            carry_over: false,
        })
    }
    async fn list_overrides(&self, _date: NaiveDate) -> ManagementResult<Vec<DailyOverride>> {
        Ok(vec![])
    }
    async fn get_override(
        &self,
        _id: &LimitSubject,
        _date: NaiveDate,
    ) -> ManagementResult<Option<DailyOverride>> {
        Ok(None)
    }
    async fn upsert_override(
        &self,
        _id: &LimitSubject,
        _date: NaiveDate,
        _availability: Option<bool>,
        _quota_delta_seconds: Option<i64>,
    ) -> ManagementResult<DailyOverride> {
        Err(ManagementError::BadRequest("nope".into()))
    }
    async fn delete_override(
        &self,
        _id: &LimitSubject,
        _date: NaiveDate,
    ) -> ManagementResult<bool> {
        Ok(false)
    }
    async fn usage_all(
        &self,
        _from: NaiveDate,
        _to: NaiveDate,
    ) -> ManagementResult<Vec<UsageStat>> {
        Ok(vec![])
    }
    async fn usage_entry(
        &self,
        _id: &EntryId,
        _from: NaiveDate,
        _to: NaiveDate,
    ) -> ManagementResult<Vec<UsageStat>> {
        Ok(vec![])
    }
    async fn get_volume(&self) -> ManagementResult<VolumeInfo> {
        Ok(VolumeInfo {
            percent: 0,
            muted: false,
            available: false,
            backend: None,
            restrictions: VolumeRestrictions {
                max_volume: Some(100),
                min_volume: Some(0),
                allow_mute: true,
                allow_change: true,
            },
        })
    }
    async fn set_volume(&self, _percent: u8) -> ManagementResult<VolumeInfo> {
        Err(ManagementError::Forbidden("no".into()))
    }
    async fn set_mute(&self, _muted: bool) -> ManagementResult<VolumeInfo> {
        Err(ManagementError::Forbidden("no".into()))
    }
    async fn volume_up(&self, _step: u8) -> ManagementResult<VolumeInfo> {
        Err(ManagementError::Forbidden("no".into()))
    }
    async fn volume_down(&self, _step: u8) -> ManagementResult<VolumeInfo> {
        Err(ManagementError::Forbidden("no".into()))
    }
    async fn toggle_mute(&self) -> ManagementResult<VolumeInfo> {
        Err(ManagementError::Forbidden("no".into()))
    }
    async fn get_brightness(&self) -> ManagementResult<BrightnessInfo> {
        Ok(BrightnessInfo {
            percent: 0,
            available: false,
            backend: None,
            device: None,
            restrictions: BrightnessRestrictions {
                max_brightness: Some(100),
                min_brightness: Some(0),
                allow_change: true,
            },
            auto_available: false,
            auto_enabled: false,
        })
    }
    async fn set_brightness(&self, _percent: u8) -> ManagementResult<BrightnessInfo> {
        Err(ManagementError::Internal("nope".into()))
    }
    async fn brightness_up(&self, _step: u8) -> ManagementResult<BrightnessInfo> {
        Err(ManagementError::Internal("nope".into()))
    }
    async fn brightness_down(&self, _step: u8) -> ManagementResult<BrightnessInfo> {
        Err(ManagementError::Internal("nope".into()))
    }
    async fn set_auto_brightness(&self, _enabled: bool) -> ManagementResult<BrightnessInfo> {
        Err(ManagementError::Internal("nope".into()))
    }
    async fn toggle_auto_brightness(&self) -> ManagementResult<BrightnessInfo> {
        Err(ManagementError::Internal("nope".into()))
    }
    async fn get_hud_scale(&self) -> f64 {
        1.0
    }
    async fn get_display_state(&self) -> DisplayState {
        DisplayState {
            mode: DisplayMode::SingleInternal,
            primary: None,
            secondary: None,
        }
    }
    async fn set_display_mode(&self, _mode: DisplayMode) -> DisplayState {
        self.get_display_state().await
    }
    async fn reload_config(&self) -> ManagementResult<usize> {
        Ok(42)
    }
    async fn logout(&self) {}
    async fn ping(&self) {}
    async fn list_windows(&self) -> ManagementResult<Vec<WindowInfo>> {
        Ok(vec![])
    }
    async fn act_on_window(&self, _id: u64, _action: WindowAction) -> ManagementResult<()> {
        Ok(())
    }
    fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        broadcast::channel(1).0.subscribe()
    }
}

/// Build an [`RpcRequest`] without spelling out the struct each time.
pub(crate) fn req(id: u32, method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        id,
        method: method.to_string(),
        params,
    }
}
