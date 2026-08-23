//! `ManagementService`: every operation an administrator can perform on a
//! running shepherdd, behind a single transport-agnostic trait.

use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDate};
use shepherd_api::{
    BrightnessInfo, BrightnessRestrictions, DailyOverride, DisplayMode, DisplayState, EntryKind,
    EntryView, Event, EventPayload, GroupView, HealthStatus, ServiceStateSnapshot,
    SessionEndReason, SessionInfo, StopMode, TokenStatus, UsageStat, VolumeInfo,
    VolumeRestrictions, WindowAction, WindowInfo,
};
use shepherd_config::{BrightnessPolicy, VolumePolicy, load_config};
use shepherd_core::{BeginStopDecision, CoreEngine, LaunchDecision, TokenAdjustError};
use shepherd_host_api::{
    BrightnessController, DisplayController, HidpiController, HostAdapter, LightSensor,
    SpawnOptions, VolumeController,
};
use shepherd_store::Store;
use shepherd_util::{EntryId, LimitSubject, MonotonicInstant};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, watch};
use tracing::{debug, warn};

use crate::auto_brightness::{AutoAction, AutoBrightnessCurve, AutoBrightnessState};
use crate::error::{ManagementError, ManagementResult};
use crate::types::LaunchOutcome;

/// Store key under which the runtime auto-brightness on/off state persists.
pub const AUTO_BRIGHTNESS_SETTING_KEY: &str = "auto_brightness_enabled";

/// Transport-agnostic management operations. `shepherd-http` and
/// `shepherd-ble` both translate their wire format into calls on this trait.
///
/// The `#[management_rpc]` attribute expands to also emit a
/// `dispatch_json(svc, method, params) -> Result<Value, RpcDispatchError>`
/// function that BLE (and, in future, other JSON-RPC transports) can
/// use directly — no hand-written per-method match arms required.
/// See `shepherd-management-macros` for the attribute's options.
#[shepherd_management_macros::management_rpc]
#[async_trait]
pub trait ManagementService: Send + Sync {
    // Health / state
    async fn health(&self) -> HealthStatus;
    async fn service_state(&self) -> ServiceStateSnapshot;

    // Entries
    #[rpc(default(at = "shepherd_util::now"))]
    async fn list_entries(&self, at: DateTime<Local>) -> Vec<EntryView>;
    #[rpc(default(at = "shepherd_util::now"))]
    async fn get_entry(&self, id: &EntryId, at: DateTime<Local>) -> ManagementResult<EntryView>;

    // Groups (issue #5)
    /// Categories that share a schedule and a combined budget. Returns the
    /// group's own state; a member's individual limits are on its `EntryView`.
    #[rpc(default(at = "shepherd_util::now"))]
    async fn list_groups(&self, at: DateTime<Local>) -> Vec<GroupView>;

    // Sessions
    async fn current_session(&self) -> Option<SessionInfo>;
    async fn launch(&self, id: EntryId) -> ManagementResult<LaunchOutcome>;
    #[rpc(default(mode = "default_graceful"))]
    async fn stop_current(&self, mode: StopMode) -> ManagementResult<()>;
    #[rpc(wrap_result = "new_deadline")]
    async fn extend_current(&self, seconds: i64) -> ManagementResult<Option<DateTime<Local>>>;

    // Overrides
    #[rpc(default(date = "today"))]
    async fn list_overrides(&self, date: NaiveDate) -> ManagementResult<Vec<DailyOverride>>;
    /// `id` is a limit subject: a bare entry ID, or `group:<id>` to override a
    /// whole category for the day (issue #5).
    #[rpc(default(date = "today"))]
    async fn get_override(
        &self,
        id: &LimitSubject,
        date: NaiveDate,
    ) -> ManagementResult<Option<DailyOverride>>;
    #[rpc(default(date = "today"))]
    async fn upsert_override(
        &self,
        id: &LimitSubject,
        date: NaiveDate,
        availability: Option<bool>,
        quota_delta_seconds: Option<i64>,
    ) -> ManagementResult<DailyOverride>;
    #[rpc(default(date = "today"), wrap_result = "deleted")]
    async fn delete_override(&self, id: &LimitSubject, date: NaiveDate) -> ManagementResult<bool>;

    // Tokens (issue #8)
    /// Grant or revoke banked time on a token gate. `id` is a limit subject —
    /// a bare entry ID, or `group:<id>` for a whole category.
    ///
    /// Granted time is indistinguishable from earned time: it is capped by
    /// `max_balance_seconds`, spent by the gated activity's sessions, and opens
    /// the gate only once the balance reaches `minimum_seconds`. To switch an
    /// activity on regardless, use an availability override.
    async fn adjust_tokens(
        &self,
        id: &LimitSubject,
        delta_seconds: i64,
    ) -> ManagementResult<TokenStatus>;

    // Usage
    #[rpc(default(from = "today", to = "today"))]
    async fn usage_all(&self, from: NaiveDate, to: NaiveDate) -> ManagementResult<Vec<UsageStat>>;
    #[rpc(default(from = "today", to = "today"))]
    async fn usage_entry(
        &self,
        id: &EntryId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> ManagementResult<Vec<UsageStat>>;

    // Volume
    async fn get_volume(&self) -> ManagementResult<VolumeInfo>;
    async fn set_volume(&self, percent: u8) -> ManagementResult<VolumeInfo>;
    async fn set_mute(&self, muted: bool) -> ManagementResult<VolumeInfo>;
    async fn volume_up(&self, step: u8) -> ManagementResult<VolumeInfo>;
    async fn volume_down(&self, step: u8) -> ManagementResult<VolumeInfo>;
    async fn toggle_mute(&self) -> ManagementResult<VolumeInfo>;

    // Brightness
    async fn get_brightness(&self) -> ManagementResult<BrightnessInfo>;
    async fn set_brightness(&self, percent: u8) -> ManagementResult<BrightnessInfo>;
    async fn brightness_up(&self, step: u8) -> ManagementResult<BrightnessInfo>;
    async fn brightness_down(&self, step: u8) -> ManagementResult<BrightnessInfo>;

    // Automatic (ambient-light) brightness
    async fn set_auto_brightness(&self, enabled: bool) -> ManagementResult<BrightnessInfo>;
    async fn toggle_auto_brightness(&self) -> ManagementResult<BrightnessInfo>;

    /// The HUD counter-scale factor in force (1.0 unless an
    /// `xwayland_native_resolution` activity is running). Shells fetch this on
    /// every connect: `HudScaleChanged` is a one-shot event at launch, so one
    /// that was not subscribed at that instant would otherwise stay
    /// un-counter-scaled for the rest of the session (issue #118).
    async fn get_hud_scale(&self) -> f64;

    // Display / docking (issue #87)
    async fn get_display_state(&self) -> DisplayState;
    async fn set_display_mode(&self, mode: DisplayMode) -> DisplayState;

    // Keepalive — pure round-trip used by IPC clients to detect a
    // wedged connection. The `ping` name aligns with the IPC wire
    // name; other transports can call it too but rarely need to.
    async fn ping(&self);

    // Config
    #[rpc(wrap_result = "entry_count")]
    async fn reload_config(&self) -> ManagementResult<usize>;

    // User
    async fn logout(&self);

    // Debug windows
    async fn list_windows(&self) -> ManagementResult<Vec<WindowInfo>>;
    async fn act_on_window(&self, id: u64, action: WindowAction) -> ManagementResult<()>;

    // Event stream
    fn subscribe_events(&self) -> broadcast::Receiver<Event>;
}

fn default_graceful() -> StopMode {
    StopMode::Graceful
}

fn today() -> NaiveDate {
    shepherd_util::now().date_naive()
}

/// Production implementation of [`ManagementService`]. Composes the
/// daemon's existing collaborators; constructed once by `shepherdd` and
/// shared via `Arc<dyn ManagementService>` to all transports.
pub struct DefaultManagementService {
    pub engine: Arc<Mutex<CoreEngine>>,
    pub store: Arc<dyn Store>,
    pub host: Arc<dyn HostAdapter>,
    pub volume: Arc<dyn VolumeController>,
    pub brightness: Arc<dyn BrightnessController>,
    /// Ambient light sensor, present only when the host exposes one. `None`
    /// disables automatic brightness entirely (the toggle rejects enabling).
    pub light_sensor: Option<Arc<dyn LightSensor>>,
    /// Runtime automatic-brightness state (on/off + manual override). Shared
    /// with the daemon's poll loop, which calls [`Self::auto_brightness_tick`].
    pub auto_brightness: Arc<Mutex<AutoBrightnessState>>,
    pub event_tx: broadcast::Sender<Event>,
    /// Broadcasts an event to all subscribers (IPC clients and SSE clients
    /// alike). Set by the daemon's main loop.
    pub broadcast_fn: Arc<dyn Fn(Event) + Send + Sync>,
    pub config_path: PathBuf,
    /// Fires when shepherdd should begin graceful shutdown. The logout
    /// operation flips this to `true`.
    pub shutdown_tx: watch::Sender<bool>,
    pub hidpi: Arc<dyn HidpiController>,
    pub display: Arc<dyn DisplayController>,
}

#[async_trait]
impl ManagementService for DefaultManagementService {
    // ---------------------------------------------------------------- health
    async fn health(&self) -> HealthStatus {
        let _eng = self.engine.lock().await;
        HealthStatus {
            live: true,
            ready: true,
            policy_loaded: true,
            host_adapter_ok: self.host.is_healthy(),
            store_ok: self.store.is_healthy(),
        }
    }

    async fn service_state(&self) -> ServiceStateSnapshot {
        let eng = self.engine.lock().await;
        eng.get_state()
    }

    // --------------------------------------------------------------- entries
    async fn list_entries(&self, at: DateTime<Local>) -> Vec<EntryView> {
        let eng = self.engine.lock().await;
        eng.list_entries(at)
    }

    async fn get_entry(&self, id: &EntryId, at: DateTime<Local>) -> ManagementResult<EntryView> {
        let eng = self.engine.lock().await;
        eng.list_entries(at)
            .into_iter()
            .find(|e| e.entry_id == *id)
            .ok_or_else(|| ManagementError::NotFound(format!("No entry with id '{id}'")))
    }

    // ---------------------------------------------------------------- groups
    async fn list_groups(&self, at: DateTime<Local>) -> Vec<GroupView> {
        let eng = self.engine.lock().await;
        eng.list_groups(at)
    }

    // -------------------------------------------------------------- sessions
    async fn current_session(&self) -> Option<SessionInfo> {
        let eng = self.engine.lock().await;
        eng.current_session()
            .map(|s| s.to_session_info(MonotonicInstant::now()))
    }

    async fn launch(&self, id: EntryId) -> ManagementResult<LaunchOutcome> {
        let now = shepherd_util::now();
        let now_mono = MonotonicInstant::now();

        let decision = {
            let eng = self.engine.lock().await;
            eng.request_launch(&id, now)
        };

        let plan = match decision {
            LaunchDecision::Denied { reasons } => {
                return Ok(LaunchOutcome::Denied { reasons });
            }
            LaunchDecision::Approved(plan) => plan,
        };

        let session_id = plan.session_id.clone();
        let plan_label = plan.label.clone();
        let plan_confirm_on_close = plan.confirm_on_close;

        {
            let mut eng = self.engine.lock().await;
            eng.start_session(plan, now, now_mono);
        }

        // Resolve the spawn parameters from policy. Populate firewall and
        // browser from the entry's policy so per-entry rules are actually
        // applied -- mirrors the IPC `Launch` path in shepherdd/src/main.rs.
        let (entry_kind, spawn_opts, needs_hidpi) = {
            let eng = self.engine.lock().await;
            let entry = eng.policy().get_entry(&id);
            let kind = entry.map(|e| e.kind.clone());
            let firewall =
                entry
                    .and_then(|e| e.firewall.clone())
                    .map(|fw| shepherd_host_api::FirewallSpec {
                        default_deny: fw.default_deny,
                        allow: fw.allow,
                        deny: fw.deny,
                    });
            let browser =
                entry
                    .and_then(|e| e.browser.clone())
                    .map(|b| shepherd_host_api::BrowserSpec {
                        policy_id: id.as_str().to_string(),
                        profile_id: b.profile_id,
                        mode: b.mode,
                        start_url: b.start_url,
                        url_allowlist: b.url_allowlist,
                        url_blocklist: b.url_blocklist,
                        disable_dev_tools: b.disable_dev_tools,
                        disable_incognito: b.disable_incognito,
                        disable_extensions: b.disable_extensions,
                        wipe_on_exit: b.wipe_on_exit,
                    });
            let input_compat = entry.map(|e| e.input_compat.clone()).unwrap_or_default();
            let input_compat_options = entry.map(|e| e.input_compat_options).unwrap_or_default();
            // Hand the activity the same check that gates its availability, so
            // (e.g.) a media grid hides online-only items instead of leaving
            // tiles that error on tap. The entry's own target wins over the
            // service's; `forward_check = false` suppresses both.
            let connectivity_check = entry.and_then(|e| {
                if !e.internet.forward_check {
                    return None;
                }
                e.internet
                    .check
                    .as_ref()
                    .or(eng.policy().service.internet.check.as_ref())
                    .map(|t| t.original.clone())
            });
            // The cache the activity writes to is the one shepherdd prefetches
            // into, so the eviction policy has to travel with the launch.
            let is_media = matches!(kind, Some(EntryKind::Media { .. }));
            let media_watched_grace_days =
                is_media.then(|| eng.policy().service.media.watched_grace_days);
            let media_cache_max_bytes =
                is_media.then(|| eng.policy().service.media.cache_max_bytes);
            let needs_hidpi = entry.is_some_and(|e| e.xwayland_native_resolution);
            let opts = if eng.policy().service.capture_child_output {
                let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
                let filename = format!(
                    "{}_{}.log",
                    id.as_str().replace(['/', '\\', ' '], "_"),
                    timestamp
                );
                SpawnOptions {
                    capture_stdout: true,
                    capture_stderr: true,
                    log_path: Some(eng.policy().service.child_log_dir.join(filename)),
                    firewall,
                    browser,
                    input_compat,
                    input_compat_options,
                    connectivity_check,
                    media_watched_grace_days,
                    media_cache_max_bytes,
                    ..Default::default()
                }
            } else {
                SpawnOptions {
                    firewall,
                    browser,
                    input_compat,
                    input_compat_options,
                    connectivity_check,
                    media_watched_grace_days,
                    media_cache_max_bytes,
                    ..Default::default()
                }
            };
            (kind, opts, needs_hidpi)
        };

        let Some(kind) = entry_kind else {
            let mut eng = self.engine.lock().await;
            eng.notify_launch_failed(None, "entry not found".into(), now_mono, now);
            return Err(ManagementError::NotFound("Entry not found".into()));
        };

        // Apply the XWayland HiDPI workaround before spawning so the client
        // sees the native scale on first map (mirror of the IPC launch path
        // in shepherdd::main).
        if needs_hidpi {
            self.hidpi.apply().await;
        }

        match self.host.spawn(session_id.clone(), &kind, spawn_opts).await {
            Ok(handle) => {
                let deadline = {
                    let mut eng = self.engine.lock().await;
                    eng.attach_host_handle(handle);
                    eng.current_session().and_then(|s| s.deadline)
                };

                (self.broadcast_fn)(Event::new(EventPayload::SessionStarted {
                    session_id: session_id.clone(),
                    entry_id: id.clone(),
                    label: plan_label,
                    deadline,
                    confirm_on_close: plan_confirm_on_close,
                }));

                Ok(LaunchOutcome::Approved {
                    session_id: session_id.to_string(),
                    deadline,
                })
            }
            Err(e) => {
                warn!(error = %e, "Spawn failed from management launch");
                // Roll back the scale change so the launcher reappears
                // with a correctly-sized HUD.
                self.hidpi.restore().await;
                let snap = {
                    let mut eng = self.engine.lock().await;
                    eng.notify_launch_failed(None, e.to_string(), now_mono, now);
                    eng.get_state()
                };
                (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
                Err(ManagementError::Internal(format!("Spawn failed: {e}")))
            }
        }
    }

    /// Stop the running activity, then report the session ended.
    ///
    /// Order matters and is the fix for issue #136. The session is marked
    /// stopping but **stays current** while the host tears the activity down,
    /// so for the whole (up to 5s) teardown window:
    ///
    /// - `request_launch` still sees an active session and denies anything
    ///   else, which is what stops a stray button press from launching an
    ///   unintended activity;
    /// - clients keep rendering the session, so the launcher grid is not put
    ///   back under the child's thumb while the old activity is still up;
    /// - the compositor scale is left alone until the activity's window is
    ///   actually gone.
    ///
    /// Only once the host confirms teardown is the session settled, announced
    /// and cleared. A host that could not kill the activity is reported as an
    /// error rather than silently swallowed.
    async fn stop_current(&self, mode: StopMode) -> ManagementResult<()> {
        let now = shepherd_util::now();
        // Read the clock *here*, before the teardown below blocks for up to
        // five seconds, and hand this instant to `finish_stop`. That is what
        // keeps the child from being charged for the "Closing…" spinner.
        // Moving this read below `host.stop().await` would silently start
        // billing teardown.
        let now_mono = MonotonicInstant::now();

        let reason = match mode {
            StopMode::Graceful => SessionEndReason::UserStop,
            StopMode::Force => SessionEndReason::AdminStop,
        };

        let handle = {
            let mut eng = self.engine.lock().await;
            match eng.begin_stop(reason) {
                BeginStopDecision::NoActiveSession => {
                    return Err(ManagementError::NotFound("No active session".into()));
                }
                BeginStopDecision::Stopping {
                    handle,
                    already_stopping,
                } => {
                    if already_stopping {
                        debug!("Stop already in flight; not starting a second teardown");
                    }
                    handle
                }
            }
        };

        // Tell everyone we are closing *before* the blocking teardown, so the
        // launcher and HUD can show it. Without this the screen looks
        // unchanged for the whole (up to 5s) wait, which is what made the
        // child press again on 2026-08-20 (issue #136).
        let snap = self.engine.lock().await.get_state();
        (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));

        // Tear the activity down first — everything below assumes it is gone.
        let stop_result = match handle {
            Some(h) => {
                let host_mode = match mode {
                    StopMode::Graceful => shepherd_host_api::StopMode::Graceful {
                        timeout: Duration::from_secs(5),
                    },
                    StopMode::Force => shepherd_host_api::StopMode::Force,
                };
                self.host.stop(&h, host_mode).await
            }
            None => Ok(()),
        };

        // Now that the window is down, hand the compositor back to the
        // launcher; idempotent when no workaround was active.
        self.hidpi.restore().await;

        let settled = {
            let mut eng = self.engine.lock().await;
            eng.finish_stop(now_mono, now)
        };

        if let Some(result) = settled {
            (self.broadcast_fn)(Event::new(EventPayload::SessionEnded {
                session_id: result.session_id,
                entry_id: result.entry_id,
                reason: result.reason,
                duration: result.duration,
            }));
        }
        let snap = self.engine.lock().await.get_state();
        (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));

        stop_result.map_err(|e| {
            warn!(error = %e, "Activity survived the stop request");
            ManagementError::Internal(format!("Failed to stop activity: {e}"))
        })
    }

    async fn extend_current(&self, seconds: i64) -> ManagementResult<Option<DateTime<Local>>> {
        let now = shepherd_util::now();
        let now_mono = MonotonicInstant::now();

        let new_deadline = {
            let mut eng = self.engine.lock().await;
            if !eng.has_active_session() {
                return Err(ManagementError::NotFound("No active session".into()));
            }
            if seconds >= 0 {
                eng.extend_current(Duration::from_secs(seconds as u64), now_mono, now)
            } else {
                eng.reduce_current(Duration::from_secs(seconds.unsigned_abs()), now_mono, now)
            }
        };

        let snap = self.engine.lock().await.get_state();
        (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));

        Ok(new_deadline)
    }

    // ------------------------------------------------------------- overrides
    async fn list_overrides(&self, date: NaiveDate) -> ManagementResult<Vec<DailyOverride>> {
        self.store
            .list_daily_overrides(date)
            .map_err(|e| ManagementError::Internal(e.to_string()))
    }

    async fn get_override(
        &self,
        id: &LimitSubject,
        date: NaiveDate,
    ) -> ManagementResult<Option<DailyOverride>> {
        self.store
            .get_daily_override(id, date)
            .map_err(|e| ManagementError::Internal(e.to_string()))
    }

    async fn upsert_override(
        &self,
        id: &LimitSubject,
        date: NaiveDate,
        availability: Option<bool>,
        quota_delta_seconds: Option<i64>,
    ) -> ManagementResult<DailyOverride> {
        if availability.is_none() && quota_delta_seconds.is_none() {
            return Err(ManagementError::BadRequest(
                "At least one of 'availability' or 'quota_delta_seconds' must be provided".into(),
            ));
        }

        let ov = self
            .store
            .upsert_daily_override(id, date, availability, quota_delta_seconds)
            .map_err(|e| ManagementError::Internal(e.to_string()))?;

        let snap = self.engine.lock().await.get_state();
        (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));

        Ok(ov)
    }

    async fn delete_override(&self, id: &LimitSubject, date: NaiveDate) -> ManagementResult<bool> {
        let deleted = self
            .store
            .clear_daily_override(id, date)
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        if deleted {
            let snap = self.engine.lock().await.get_state();
            (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
        }
        Ok(deleted)
    }

    // ---------------------------------------------------------------- tokens
    async fn adjust_tokens(
        &self,
        id: &LimitSubject,
        delta_seconds: i64,
    ) -> ManagementResult<TokenStatus> {
        let status = {
            let eng = self.engine.lock().await;
            eng.adjust_tokens(id, delta_seconds, shepherd_util::now())
                .map_err(|e| match e {
                    TokenAdjustError::UnknownSubject => {
                        ManagementError::NotFound(format!("No entry or group '{id}'"))
                    }
                    TokenAdjustError::NotGated => ManagementError::Unprocessable(format!(
                        "'{id}' has no token gate, so it has no balance to adjust"
                    )),
                    TokenAdjustError::Store(msg) => ManagementError::Internal(msg),
                })?
        };

        // The gate may have just opened or closed, so every client's entry
        // list is stale.
        let snap = self.engine.lock().await.get_state();
        (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));

        Ok(status)
    }

    // ----------------------------------------------------------------- usage
    async fn usage_all(&self, from: NaiveDate, to: NaiveDate) -> ManagementResult<Vec<UsageStat>> {
        if from > to {
            return Err(ManagementError::BadRequest(
                "`from` must not be after `to`".into(),
            ));
        }

        let label_map: std::collections::HashMap<String, String> = {
            let eng = self.engine.lock().await;
            eng.policy()
                .entries
                .iter()
                .map(|e| (e.id.as_str().to_owned(), e.label.clone()))
                .collect()
        };

        let mut stats = Vec::new();
        for entry in self.engine.lock().await.policy().entries.iter() {
            let entry_id = entry.id.clone();
            let label = label_map
                .get(entry_id.as_str())
                .cloned()
                .unwrap_or_else(|| entry_id.as_str().to_owned());

            let rows = self
                .store
                .get_usage_range(&entry_id, from, to)
                .map_err(|e| ManagementError::Internal(e.to_string()))?;

            for (date, duration) in rows {
                stats.push(UsageStat {
                    entry_id: entry_id.clone(),
                    label: label.clone(),
                    date,
                    duration_seconds: duration.as_secs(),
                });
            }
        }

        stats.sort_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then(a.entry_id.as_str().cmp(b.entry_id.as_str()))
        });
        Ok(stats)
    }

    async fn usage_entry(
        &self,
        id: &EntryId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> ManagementResult<Vec<UsageStat>> {
        if from > to {
            return Err(ManagementError::BadRequest(
                "`from` must not be after `to`".into(),
            ));
        }

        let label = {
            let eng = self.engine.lock().await;
            eng.policy()
                .get_entry(id)
                .map(|e| e.label.clone())
                .ok_or_else(|| ManagementError::NotFound(format!("No entry with id '{id}'")))?
        };

        let rows = self
            .store
            .get_usage_range(id, from, to)
            .map_err(|e| ManagementError::Internal(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|(date, duration)| UsageStat {
                entry_id: id.clone(),
                label: label.clone(),
                date,
                duration_seconds: duration.as_secs(),
            })
            .collect())
    }

    // ---------------------------------------------------------------- volume
    async fn get_volume(&self) -> ManagementResult<VolumeInfo> {
        let restrictions = self.volume_restrictions().await;
        let status = self
            .volume
            .get_status()
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        Ok(VolumeInfo {
            percent: status.percent,
            muted: status.muted,
            available: self.volume.capabilities().available,
            backend: self.volume.capabilities().backend.clone(),
            restrictions,
        })
    }

    async fn set_volume(&self, percent: u8) -> ManagementResult<VolumeInfo> {
        let restrictions = self.volume_restrictions().await;
        if !restrictions.allow_change {
            return Err(ManagementError::Forbidden(
                "Volume changes are not allowed".into(),
            ));
        }
        let clamped = restrictions.clamp_volume(percent);
        self.volume
            .set_volume(clamped)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        self.broadcast_volume_change().await
    }

    async fn set_mute(&self, muted: bool) -> ManagementResult<VolumeInfo> {
        let restrictions = self.volume_restrictions().await;
        if !restrictions.allow_mute {
            return Err(ManagementError::Forbidden(
                "Mute toggle is not allowed".into(),
            ));
        }
        self.volume
            .set_mute(muted)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        self.broadcast_volume_change().await
    }

    async fn volume_up(&self, step: u8) -> ManagementResult<VolumeInfo> {
        let restrictions = self.volume_restrictions().await;
        if !restrictions.allow_change {
            return Err(ManagementError::Forbidden(
                "Volume changes are not allowed".into(),
            ));
        }
        self.volume
            .volume_up(step)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        self.broadcast_volume_change().await
    }

    async fn volume_down(&self, step: u8) -> ManagementResult<VolumeInfo> {
        let restrictions = self.volume_restrictions().await;
        if !restrictions.allow_change {
            return Err(ManagementError::Forbidden(
                "Volume changes are not allowed".into(),
            ));
        }
        self.volume
            .volume_down(step)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        self.broadcast_volume_change().await
    }

    async fn toggle_mute(&self) -> ManagementResult<VolumeInfo> {
        let restrictions = self.volume_restrictions().await;
        if !restrictions.allow_mute {
            return Err(ManagementError::Forbidden(
                "Mute toggle is not allowed".into(),
            ));
        }
        self.volume
            .toggle_mute()
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        self.broadcast_volume_change().await
    }

    // ------------------------------------------------------------ brightness
    async fn get_brightness(&self) -> ManagementResult<BrightnessInfo> {
        let restrictions = self.brightness_restrictions().await;
        let (auto_available, auto_enabled) = self.auto_status().await;
        match self.brightness.get_status().await {
            Ok(s) => Ok(BrightnessInfo {
                percent: s.percent,
                available: self.brightness.capabilities().available,
                backend: self.brightness.capabilities().backend.clone(),
                device: self.brightness.capabilities().device.clone(),
                restrictions,
                auto_available,
                auto_enabled,
            }),
            Err(e) => {
                // No backlight detected (or read failed) → return an
                // "unavailable" info instead of an error so UIs can hide
                // the slider without treating it as a failure.
                if !self.brightness.capabilities().available {
                    Ok(BrightnessInfo {
                        percent: 0,
                        available: false,
                        backend: self.brightness.capabilities().backend.clone(),
                        device: self.brightness.capabilities().device.clone(),
                        restrictions,
                        auto_available,
                        auto_enabled,
                    })
                } else {
                    Err(ManagementError::Internal(e.to_string()))
                }
            }
        }
    }

    async fn set_brightness(&self, percent: u8) -> ManagementResult<BrightnessInfo> {
        let restrictions = self.brightness_restrictions().await;
        if !restrictions.allow_change {
            return Err(ManagementError::Forbidden(
                "Brightness changes are not allowed".into(),
            ));
        }
        let clamped = restrictions.clamp_brightness(percent);
        self.brightness
            .set_brightness(clamped)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        // A manual set temporarily wins over auto brightness: record the
        // ambient light at this moment so the poll loop holds off until the
        // room's lighting shifts noticeably (phone-style).
        self.register_manual_override().await;
        self.broadcast_brightness_change().await
    }

    async fn brightness_up(&self, step: u8) -> ManagementResult<BrightnessInfo> {
        let current = self.get_brightness().await?;
        let target = current.percent.saturating_add(step).min(100);
        self.set_brightness(target).await
    }

    async fn brightness_down(&self, step: u8) -> ManagementResult<BrightnessInfo> {
        let current = self.get_brightness().await?;
        let target = current.percent.saturating_sub(step);
        self.set_brightness(target).await
    }

    async fn set_auto_brightness(&self, enabled: bool) -> ManagementResult<BrightnessInfo> {
        self.apply_auto_enabled(enabled).await
    }

    async fn toggle_auto_brightness(&self) -> ManagementResult<BrightnessInfo> {
        let current = self.auto_brightness.lock().await.enabled();
        self.apply_auto_enabled(!current).await
    }

    async fn get_hud_scale(&self) -> f64 {
        self.hidpi.factor().await
    }

    // --------------------------------------------------------------- display
    async fn get_display_state(&self) -> DisplayState {
        self.display.state().await
    }

    async fn set_display_mode(&self, mode: DisplayMode) -> DisplayState {
        self.display.set_mode(mode).await
    }

    // ---------------------------------------------------------------- config
    async fn reload_config(&self) -> ManagementResult<usize> {
        match load_config(&self.config_path) {
            Ok(policy) => {
                let entry_count = policy.entries.len();
                {
                    let mut eng = self.engine.lock().await;
                    eng.reload_policy(policy);
                }
                let snap = self.engine.lock().await.get_state();
                (self.broadcast_fn)(Event::new(EventPayload::PolicyReloaded { entry_count }));
                (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
                Ok(entry_count)
            }
            Err(e) => {
                warn!(error = %e, "Config reload failed via management API");
                Err(ManagementError::Unprocessable(e.to_string()))
            }
        }
    }

    // ------------------------------------------------------------------ user
    async fn logout(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    async fn ping(&self) {}

    // --------------------------------------------------------------- windows
    async fn list_windows(&self) -> ManagementResult<Vec<WindowInfo>> {
        self.host
            .list_windows()
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))
    }

    async fn act_on_window(&self, id: u64, action: WindowAction) -> ManagementResult<()> {
        self.host
            .act_on_window(id, action)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))
    }

    // ---------------------------------------------------------------- events
    fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }
}

impl DefaultManagementService {
    async fn volume_restrictions(&self) -> VolumeRestrictions {
        let eng = self.engine.lock().await;
        let policy = if let Some(session) = eng.current_session()
            && let Some(entry) = eng.policy().get_entry(&session.plan.entry_id)
            && let Some(ref vol) = entry.volume
        {
            vol.clone()
        } else {
            eng.policy().volume.clone()
        };
        convert_volume_policy(&policy)
    }

    async fn brightness_restrictions(&self) -> BrightnessRestrictions {
        let eng = self.engine.lock().await;
        resolve_brightness_restrictions(&eng)
    }

    /// `(auto_available, auto_enabled)` — whether a light sensor is present
    /// and whether auto brightness is currently on.
    async fn auto_status(&self) -> (bool, bool) {
        let available = self.light_sensor.is_some();
        let enabled = available && self.auto_brightness.lock().await.enabled();
        (available, enabled)
    }

    /// Record that the user just set brightness by hand, so the auto poll loop
    /// backs off until the ambient light changes. No-op without a sensor or
    /// when auto is off.
    async fn register_manual_override(&self) {
        let Some(sensor) = self.light_sensor.as_ref() else {
            return;
        };
        let mut st = self.auto_brightness.lock().await;
        if !st.enabled() {
            return;
        }
        match sensor.read_lux() {
            Ok(lux) => st.begin_manual_override(lux),
            Err(e) => debug!(error = %e, "auto-brightness: manual-override lux read failed"),
        }
    }

    /// Turn auto brightness on/off, persist the choice, and (when enabling)
    /// apply an initial adjustment immediately rather than waiting a poll.
    async fn apply_auto_enabled(&self, enabled: bool) -> ManagementResult<BrightnessInfo> {
        if enabled && self.light_sensor.is_none() {
            return Err(ManagementError::Unprocessable(
                "No ambient light sensor available on this host".into(),
            ));
        }
        self.auto_brightness.lock().await.set_enabled(enabled);
        if let Err(e) = self.store.set_setting(
            AUTO_BRIGHTNESS_SETTING_KEY,
            if enabled { "true" } else { "false" },
        ) {
            warn!(error = %e, "Failed to persist auto-brightness setting");
        }
        if enabled {
            // Snap to the ambient light now; this also emits BrightnessChanged.
            self.auto_brightness_tick().await;
        }
        // Return fresh info regardless (the tick may have held if already
        // at target, but the auto_enabled flag still changed).
        self.broadcast_brightness_change().await
    }

    /// One iteration of the automatic-brightness control loop: sample the
    /// light sensor, map to a target through the configured curve and policy
    /// clamp, and write it — unless a manual override is holding. Called on a
    /// timer by the daemon and once on enable. Cheap no-op when auto is off.
    pub async fn auto_brightness_tick(&self) {
        let Some(sensor) = self.light_sensor.as_ref() else {
            return;
        };
        if !self.auto_brightness.lock().await.enabled() {
            return;
        }
        let lux = match sensor.read_lux() {
            Ok(lux) => lux,
            Err(e) => {
                debug!(error = %e, "auto-brightness: light sensor read failed");
                return;
            }
        };
        // Resolve the curve and policy clamp together under one engine lock so
        // a concurrent config reload can't split them.
        let (curve, restrictions) = {
            let eng = self.engine.lock().await;
            let ab = &eng.policy().auto_brightness;
            let curve = AutoBrightnessCurve {
                dim_lux: ab.dim_lux,
                bright_lux: ab.bright_lux,
                min_percent: ab.min_percent,
                max_percent: ab.max_percent,
            };
            (curve, resolve_brightness_restrictions(&eng))
        };
        let current = match self.brightness.get_status().await {
            Ok(s) => s.percent,
            Err(e) => {
                debug!(error = %e, "auto-brightness: backlight read failed");
                return;
            }
        };
        let action = {
            let mut st = self.auto_brightness.lock().await;
            st.tick(&curve, lux, current, |p| restrictions.clamp_brightness(p))
        };
        if let AutoAction::Apply(target) = action {
            match self.brightness.set_brightness(target).await {
                Ok(()) => {
                    let _ = self.broadcast_brightness_change().await;
                }
                Err(e) => warn!(error = %e, "auto-brightness: failed to set backlight"),
            }
        }
    }

    async fn broadcast_volume_change(&self) -> ManagementResult<VolumeInfo> {
        let status = self
            .volume
            .get_status()
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        (self.broadcast_fn)(Event::new(EventPayload::VolumeChanged {
            percent: status.percent,
            muted: status.muted,
        }));
        Ok(VolumeInfo {
            percent: status.percent,
            muted: status.muted,
            available: self.volume.capabilities().available,
            backend: self.volume.capabilities().backend.clone(),
            restrictions: self.volume_restrictions().await,
        })
    }

    async fn broadcast_brightness_change(&self) -> ManagementResult<BrightnessInfo> {
        let status = self
            .brightness
            .get_status()
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        let (auto_available, auto_enabled) = self.auto_status().await;
        (self.broadcast_fn)(Event::new(EventPayload::BrightnessChanged {
            percent: status.percent,
            auto_enabled,
        }));
        Ok(BrightnessInfo {
            percent: status.percent,
            available: self.brightness.capabilities().available,
            backend: self.brightness.capabilities().backend.clone(),
            device: self.brightness.capabilities().device.clone(),
            restrictions: self.brightness_restrictions().await,
            auto_available,
            auto_enabled,
        })
    }
}

/// Resolve the effective brightness restrictions for the active session: the
/// current entry's override if it has one, else the global default. Shared by
/// the RPC path and the auto-brightness poll loop.
fn resolve_brightness_restrictions(eng: &CoreEngine) -> BrightnessRestrictions {
    let policy = if let Some(session) = eng.current_session()
        && let Some(entry) = eng.policy().get_entry(&session.plan.entry_id)
        && let Some(ref br) = entry.brightness
    {
        br.clone()
    } else {
        eng.policy().brightness.clone()
    };
    convert_brightness_policy(&policy)
}

fn convert_volume_policy(p: &VolumePolicy) -> VolumeRestrictions {
    VolumeRestrictions {
        max_volume: p.max_volume,
        min_volume: p.min_volume,
        allow_mute: p.allow_mute,
        allow_change: p.allow_change,
    }
}

fn convert_brightness_policy(p: &BrightnessPolicy) -> BrightnessRestrictions {
    BrightnessRestrictions {
        max_brightness: p.max_brightness,
        min_brightness: p.min_brightness,
        allow_change: p.allow_change,
    }
}
