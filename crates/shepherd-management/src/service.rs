//! `ManagementService`: every operation an administrator can perform on a
//! running shepherdd, behind a single transport-agnostic trait.

use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDate};
use shepherd_api::{
    AudioOutputRecord, BrightnessInfo, BrightnessRestrictions, DailyOverride, Diagnostic,
    DiagnosticCode, DiagnosticSet, DiagnosticSeverity, DiagnosticSink, DiagnosticSubject,
    DisplayMode, DisplayState, EntryKind, EntryView, Event, EventPayload, GroupView, HealthStatus,
    HudOrientation, NetworkStatusView, ServiceStateSnapshot, SessionEndReason, SessionInfo,
    StopMode, TokenStatus, UsageStat, VolumeInfo, VolumeRestrictions, WindowAction, WindowInfo,
};
use shepherd_config::{BrightnessPolicy, VolumePolicy, load_config};
use shepherd_core::{BeginStopDecision, CoreEngine, LaunchDecision, TokenAdjustError};
use shepherd_host_api::{
    BrightnessController, DisplayController, HidpiController, HostAdapter, HudLayoutController,
    LightSensor, NetworkInfoProvider, NetworkSnapshot, SpawnOptions, SponsorBlockSpec,
    VolumeController, VolumeError,
};
use shepherd_store::Store;
use shepherd_util::{EntryId, LimitSubject, MonotonicInstant};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, mpsc, watch};
use tracing::{debug, info, warn};

use crate::auto_brightness::{AutoAction, AutoBrightnessCurve, AutoBrightnessState};
use crate::error::{ManagementError, ManagementResult};
use crate::listener::WebListenerHandle;
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
    /// Reset the running activity to its starting state without ending the
    /// session — the HUD's "reboot the console" button.
    ///
    /// Stops the activity cleanly (so it flushes its own saved data), discards
    /// the resume state that would otherwise put it straight back where it
    /// was, and relaunches it under the same session: same id, same deadline,
    /// same clock. Fails if there is no session, or its activity doesn't
    /// support being reset — see `EntryKind::supports_reset`.
    async fn reset_current(&self) -> ManagementResult<()>;
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

    // Per-output volume limits (issue #124)
    async fn list_audio_outputs(&self) -> ManagementResult<Vec<AudioOutputRecord>>;
    #[rpc(default(max_volume = "Default::default", min_volume = "Default::default"))]
    async fn set_audio_output_limits(
        &self,
        output_key: String,
        max_volume: Option<u8>,
        min_volume: Option<u8>,
    ) -> ManagementResult<AudioOutputRecord>;
    async fn forget_audio_output(&self, output_key: String) -> ManagementResult<bool>;
    async fn select_audio_output(&self, output_key: String) -> ManagementResult<VolumeInfo>;

    // Brightness
    async fn get_brightness(&self) -> ManagementResult<BrightnessInfo>;
    async fn set_brightness(&self, percent: u8) -> ManagementResult<BrightnessInfo>;
    async fn brightness_up(&self, step: u8) -> ManagementResult<BrightnessInfo>;
    async fn brightness_down(&self, step: u8) -> ManagementResult<BrightnessInfo>;

    // Automatic (ambient-light) brightness
    async fn set_auto_brightness(&self, enabled: bool) -> ManagementResult<BrightnessInfo>;
    async fn toggle_auto_brightness(&self) -> ManagementResult<BrightnessInfo>;

    /// Turn the displays on or off, refusing to blank while an activity is up.
    ///
    /// Called by `swayidle` through `shepherd-launcher --screen-off/--screen-on`
    /// (issue #144): the compositor socket has no name on a hardened device, so
    /// the blanking has to run on the connection shepherdd holds.
    ///
    /// The "is anything running?" check lives here rather than in the caller
    /// because it used to be a separate `--is-idle-allowed` process, and a
    /// launch landing between that check and the blank turned the screen off on
    /// a child mid-activity. Returns whether it actually acted.
    async fn set_screen_power(&self, on: bool) -> ManagementResult<bool>;

    /// The HUD counter-scale factor in force (1.0 unless an
    /// `xwayland_native_resolution` activity is running). Shells fetch this on
    /// every connect: `HudScaleChanged` is a one-shot event at launch, so one
    /// that was not subscribed at that instant would otherwise stay
    /// un-counter-scaled for the rest of the session (issue #118).
    async fn get_hud_scale(&self) -> f64;

    /// The screen edge the HUD should occupy (issue #171): the running
    /// activity's `hud_orientation` if it asked for one, else the global
    /// `[service.hud]` setting. Fetched on every connect for the same reason
    /// as `get_hud_scale` — `HudOrientationChanged` fires only on change, so a
    /// HUD that was not subscribed at that instant would otherwise lay itself
    /// out on the wrong edge for the rest of the session.
    async fn get_hud_orientation(&self) -> HudOrientation;

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

    /// Re-fetch what the media libraries are made of, now (issue #165).
    ///
    /// Everything on the media path is cached with a TTL and swept on a timer:
    /// a YouTube playlist listing is good for six hours, a SponsorBlock bucket
    /// for a day, a failed download waits six hours before anything tries
    /// again, and the sweep that would notice runs hourly. Add a video to a
    /// playlist and it can be most of a day before the device has it. This is
    /// the override — it re-asks for the listings and the segments, forgets the
    /// download cooldowns, and sweeps immediately.
    ///
    /// Device-wide rather than per activity: the video cache and the segment
    /// buckets are one directory shared by every library, and the thing an
    /// administrator wants is "pick up what I changed", not "pick up what I
    /// changed in this one place".
    ///
    /// **Returns as soon as the work is accepted, not when it is done.** A
    /// refresh shells out to `yt-dlp` once per playlist and then downloads
    /// videos; the companion's RPC deadline is fifteen seconds. What actually
    /// happened arrives as diagnostics — a refresh that could not reach what it
    /// went for raises [`DiagnosticCode::MediaRefreshFailed`], and a successful
    /// one clears it — which both clients already display.
    async fn refresh_media(&self) -> ManagementResult<()>;

    // User
    async fn logout(&self);

    /// Administrator-facing conditions currently true of this device (issue
    /// #143).
    ///
    /// Also carried on `service_state`, but the web UI never fetches a whole
    /// snapshot — it queries per page — so the set needs a call of its own to
    /// be reachable from a browser at all.
    async fn list_diagnostics(&self) -> DiagnosticSet;

    /// Where this device is on the network, and where its web management
    /// interface is listening (issue #182).
    ///
    /// Read-only, and the answer to a question the companion app cannot ask
    /// any other way: it reached the device over BLE and has no idea what its
    /// address is. Without this, using the web interface or SSH means
    /// `arp`-ing the LAN for a device that does not announce itself.
    ///
    /// Deliberately does not repeat the connectivity checks. Those already
    /// ride `service_state`'s `internet_status`, and a UI showing both reads
    /// them from there.
    async fn network_status(&self) -> NetworkStatusView;

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

/// A snapshot of what the audio watch loop last saw. Compared field-for-field to
/// decide whether anything actually changed since the previous tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedAudioState {
    pub percent: u8,
    pub muted: bool,
    /// Identity key of the active output; `None` where outputs cannot be
    /// enumerated (any non-PipeWire host).
    pub output_key: Option<String>,
    /// Keys of every output present, sorted so the comparison is about the set
    /// and not about the order `pw-dump` happened to list them in. Lets a device
    /// being plugged in or pulled out count as a change even when it is not the
    /// one playing.
    pub present_keys: Vec<String>,
}

impl ObservedAudioState {
    fn from_snapshot(snap: &shepherd_host_api::AudioSnapshot) -> Self {
        let mut present_keys: Vec<String> = snap.outputs.iter().map(|o| o.key.clone()).collect();
        present_keys.sort();
        Self {
            percent: snap.status.percent,
            muted: snap.status.muted,
            output_key: snap.active.as_ref().map(|o| o.key.clone()),
            present_keys,
        }
    }
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
    /// Nudges shepherdd's media prefetcher to re-fetch libraries and segments
    /// immediately (issue #165). `None` on any embedding without a prefetcher —
    /// in which case [`ManagementService::refresh_media`] reports that rather
    /// than answering "done" to a button that did nothing.
    ///
    /// A bare channel rather than a collaborator trait because the work is on
    /// the far side of it: this crate must not link the media stack, and the
    /// prefetcher already owns every decision about what a sweep does.
    pub media_refresh_tx: Option<mpsc::Sender<()>>,
    /// Fires when shepherdd should begin graceful shutdown. The logout
    /// operation flips this to `true`.
    pub shutdown_tx: watch::Sender<bool>,
    pub hidpi: Arc<dyn HidpiController>,
    /// HUD placement (issue #171). A launch hands it the entry's
    /// `hud_orientation`; a session end drops back to the global setting.
    pub hud_layout: Arc<dyn HudLayoutController>,
    pub display: Arc<dyn DisplayController>,
    /// What [`Self::audio_watch_tick`] last observed, so the poll loop only
    /// broadcasts on a real change. `None` until the first tick establishes a
    /// baseline.
    pub last_audio_state: Arc<Mutex<Option<ObservedAudioState>>>,
    /// Where to report conditions a parent should see. `None` in tests and on
    /// any embedding that does not surface diagnostics; raising must never be
    /// load-bearing for the operation that noticed the problem.
    pub diagnostics: Option<Arc<dyn DiagnosticSink>>,
    /// How to read this device's own networking (issue #182). `None` on an
    /// embedding with no way to look, which reports itself as
    /// `NetworkSource::Unavailable` rather than as a device with no network.
    pub network: Option<Arc<dyn NetworkInfoProvider>>,
    /// What the web management interface is really doing, as opposed to what
    /// the config asked for. Written by whoever owns the listener; `Disabled`
    /// by default, which is the truth for an embedding that never starts one.
    pub web_listener: WebListenerHandle,
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
        let plan_can_reset = plan.can_reset;
        let plan_can_turn_pages = plan.can_turn_pages;
        let plan_hud_orientation = plan.hud_orientation;

        {
            let mut eng = self.engine.lock().await;
            eng.start_session(plan, now, now_mono);
        }

        let (entry_kind, spawn_opts, needs_hidpi) = {
            let eng = self.engine.lock().await;
            resolve_spawn(&eng, &id, now)
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
        // Before the spawn, like the scale hack above and for the same reason:
        // the HUD should already be on the right edge, with its exclusive zone
        // reserved on the right side, when the activity first maps.
        self.hud_layout.apply(plan_hud_orientation).await;

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
                    can_reset: plan_can_reset,
                    can_turn_pages: plan_can_turn_pages,
                }));

                Ok(LaunchOutcome::Approved {
                    session_id: session_id.to_string(),
                    deadline,
                })
            }
            Err(e) => {
                warn!(error = %e, "Spawn failed from management launch");
                // Roll back the scale change so the launcher reappears
                // with a correctly-sized HUD, and its edge with it.
                self.hidpi.restore().await;
                self.hud_layout.restore().await;
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
        // launcher; both are idempotent when nothing was overridden.
        self.hidpi.restore().await;
        self.hud_layout.restore().await;

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

    async fn reset_current(&self) -> ManagementResult<()> {
        let now = shepherd_util::now();
        let now_mono = MonotonicInstant::now();

        // Claim the restart before touching the process. From here until
        // `finish_restart` the engine ignores the activity's exit, so every
        // path below must reach that call.
        let (request, kind, spawn_opts) = {
            let mut eng = self.engine.lock().await;
            let Some(request) = eng.begin_restart() else {
                return Err(ManagementError::NotFound(
                    "No active session that can be reset".into(),
                ));
            };
            let (kind, spawn_opts, _) = resolve_spawn(&eng, &request.entry_id, now);
            (request, kind, spawn_opts)
        };

        let Some(kind) = kind else {
            // The entry vanished from policy under us (a reload between the
            // launch and now). Nothing to relaunch.
            self.finish_reset(None, now_mono, now).await;
            return Err(ManagementError::NotFound("Entry not found".into()));
        };

        // Stop gracefully so the activity saves what it owns -- for RetroArch
        // that is the in-game save, which a reset must not cost the child.
        // Deliberately no `hidpi.restore()`: the replacement wants the same
        // output scale, and bouncing it would flash the whole screen.
        if let Some(handle) = &request.host_handle {
            let _ = self
                .host
                .stop(
                    handle,
                    shepherd_host_api::StopMode::Graceful {
                        timeout: Duration::from_secs(5),
                    },
                )
                .await;
        }

        // Only now, with the activity gone, is it safe to remove the state it
        // would otherwise resume from.
        if let Err(e) = self
            .host
            .discard_saved_state(&kind, Some(request.entry_id.as_str()))
            .await
        {
            // Not fatal: the activity still comes back, just where it left off
            // rather than at its start screen. Better than no activity at all.
            warn!(error = %e, "Could not discard saved state; resetting anyway");
        }

        match self
            .host
            .spawn(request.session_id.clone(), &kind, spawn_opts)
            .await
        {
            Ok(handle) => {
                self.finish_reset(Some(handle), now_mono, now).await;
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "Relaunch after reset failed");
                // The session has no process behind it now, so it ends.
                self.hidpi.restore().await;
                self.hud_layout.restore().await;
                self.finish_reset(None, now_mono, now).await;
                Err(ManagementError::Internal(format!(
                    "Relaunch after reset failed: {e}"
                )))
            }
        }
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
        // One snapshot, not a status read plus two separate identity reads: this
        // is on the hot path — every client refetches it on every event — and
        // the three reads could disagree with each other besides.
        let (status, active) = match self.volume.observe().await {
            Ok(snap) => (snap.status, snap.active),
            // The topology could not be read, but the reading itself still can
            // be and is still true. Failing the whole call would blank the
            // volume on every surface — including the HUD — which is a worse
            // answer than the right number attributed to the output that was
            // selected a moment ago. Naming that output also keeps the
            // restriction lookup below off the global-limit fallback.
            Err(e) => {
                warn!(error = %e, "Could not read the audio topology; reporting the last output seen");
                let status = self
                    .volume
                    .get_status()
                    .await
                    .map_err(|e| ManagementError::Internal(e.to_string()))?;
                (status, self.last_seen_active_output().await)
            }
        };
        Ok(VolumeInfo {
            percent: status.percent,
            muted: status.muted,
            available: self.volume.capabilities().available,
            backend: self.volume.capabilities().backend.clone(),
            restrictions: self
                .volume_restrictions_for(active.as_ref().map(|o| o.key.as_str()))
                .await,
            output: active,
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

    // -------------------------------------------------- per-output limits

    async fn list_audio_outputs(&self) -> ManagementResult<Vec<AudioOutputRecord>> {
        let observed = self.volume.observe().await;
        // Record everything that is plugged in, not just whatever is selected.
        // A device has to be on this list before a parent can choose it, and
        // waiting for it to become the default first would mean the one device
        // you want to switch away from is the only one you can see.
        if let Ok(snap) = &observed {
            for output in &snap.outputs {
                if let Err(e) = self.store.record_audio_output_seen(output) {
                    warn!(error = %e, key = %output.key, "Failed to record an audio output");
                }
            }
        }
        let mut rows = self
            .store
            .list_audio_outputs()
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        match &observed {
            Ok(snap) => {
                for row in &mut rows {
                    row.active = Some(row.output.key.as_str()) == snap.active_key();
                    row.available = snap.outputs.iter().any(|o| o.key == row.output.key);
                }
            }
            // The read failed. Answering with the empty topology would mark
            // every row `available: false`, which both UIs render as "Not
            // connected" with the switch disabled — a transient fault shown to
            // the parent as a hardware fact, on the one screen they would use
            // to fix it. Report the last liveness actually observed instead.
            Err(e) => {
                warn!(error = %e, "Could not read the audio topology; reporting the last liveness seen");
                let last = self.last_audio_state.lock().await;
                for row in &mut rows {
                    match last.as_ref() {
                        Some(seen) => {
                            row.active =
                                seen.output_key.as_deref() == Some(row.output.key.as_str());
                            row.available = seen.present_keys.iter().any(|k| k == &row.output.key);
                        }
                        // No successful read has ever happened. Offer the choice
                        // and let the attempt fail loudly rather than greying out
                        // every device, which is what `available` documents as
                        // the reason for its default.
                        None => {
                            row.active = false;
                            row.available = true;
                        }
                    }
                }
            }
        }
        Ok(rows)
    }

    async fn set_audio_output_limits(
        &self,
        output_key: String,
        max_volume: Option<u8>,
        min_volume: Option<u8>,
    ) -> ManagementResult<AudioOutputRecord> {
        for (name, v) in [("max_volume", max_volume), ("min_volume", min_volume)] {
            if let Some(v) = v
                && v > 100
            {
                return Err(ManagementError::BadRequest(format!(
                    "{name} must be 0-100, got {v}"
                )));
            }
        }
        if let (Some(min), Some(max)) = (min_volume, max_volume)
            && min > max
        {
            return Err(ManagementError::BadRequest(format!(
                "min_volume ({min}) must not exceed max_volume ({max})"
            )));
        }

        let known = self
            .store
            .set_audio_output_limits(&output_key, max_volume, min_volume)
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        if !known {
            return Err(ManagementError::NotFound(format!(
                "Unknown audio output: {output_key}"
            )));
        }

        // A new cap has to bite immediately, including on the output that is
        // playing right now — otherwise setting a headphone limit does nothing
        // until the next time someone switches away and back.
        self.enforce_volume_ceiling().await;
        let _ = self.broadcast_volume_change().await;

        let mut row = self
            .store
            .get_audio_output(&output_key)
            .map_err(|e| ManagementError::Internal(e.to_string()))?
            .ok_or_else(|| {
                ManagementError::NotFound(format!("Unknown audio output: {output_key}"))
            })?;
        row.active =
            self.volume.current_output().await.map(|o| o.key).as_deref() == Some(&output_key);
        Ok(row)
    }

    /// Move audio to another output.
    ///
    /// The parent's side of the same switch the daemon already watches for: it
    /// lands on the identical code path as a jack insert or a dock, so the new
    /// output's cap is applied on arrival exactly as it would be if the hardware
    /// had made the choice.
    async fn select_audio_output(&self, output_key: String) -> ManagementResult<VolumeInfo> {
        if self
            .volume
            .observe()
            .await
            .ok()
            .and_then(|s| s.active.map(|o| o.key))
            .as_deref()
            == Some(&output_key)
        {
            // Already there. Not an error — two parents on two phones can both
            // tap the same row — but there is nothing to switch or clamp.
            return self.get_volume().await;
        }
        self.volume
            .select_output(&output_key)
            .await
            .map_err(|e| match e {
                // "not available" from the host means *this output* cannot be
                // switched to — usually because it is unplugged. Passing the
                // Display text through unchanged would prefix it with "Volume
                // control not available", which tells a parent the wrong thing:
                // volume control is fine, the device is simply gone.
                VolumeError::NotAvailable(why) => ManagementError::BadRequest(why),
                other => ManagementError::Internal(other.to_string()),
            })?;

        if let Some(active) = self.volume.current_output().await {
            if active.key != output_key {
                // wpctl reported success but the default did not move — a
                // higher-priority device grabbed it back, or the id we resolved
                // named something else by the time the call landed.
                return Err(ManagementError::Internal(format!(
                    "asked for {output_key} but the active output is {}",
                    active.key
                )));
            }
            if let Err(e) = self.store.record_audio_output_seen(&active) {
                warn!(error = %e, "Failed to record the selected audio output");
            }
        }
        // Same two steps the watch loop takes on a switch it merely observed.
        self.enforce_volume_ceiling().await;
        self.broadcast_volume_change().await
    }

    async fn forget_audio_output(&self, output_key: String) -> ManagementResult<bool> {
        let removed = self
            .store
            .forget_audio_output(&output_key)
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        if removed {
            // Dropping a row can only relax limits, but the clients still need
            // to hear that the effective restrictions changed.
            let _ = self.broadcast_volume_change().await;
        }
        Ok(removed)
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

    async fn set_screen_power(&self, on: bool) -> ManagementResult<bool> {
        // Blanking is suppressed while an activity is on screen; turning the
        // screen back on never is, so a device that blanked just before a
        // launch still wakes.
        if !on && self.current_session().await.is_some() {
            return Ok(false);
        }
        self.host
            .set_screen_power(on)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        Ok(true)
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

    async fn get_hud_orientation(&self) -> HudOrientation {
        self.hud_layout.orientation().await
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

    async fn refresh_media(&self) -> ManagementResult<()> {
        let Some(tx) = self.media_refresh_tx.as_ref() else {
            return Err(ManagementError::Unprocessable(
                "Media refresh is not available on this device".into(),
            ));
        };
        match tx.try_send(()) {
            Ok(()) => {
                info!("media refresh requested via management API");
                Ok(())
            }
            // The channel holds one request, which is all a request with no
            // arguments can usefully mean: a second press while the first is
            // still queued asks for the same sweep. Reporting a conflict would
            // train an administrator to press it again.
            Err(mpsc::error::TrySendError::Full(())) => {
                debug!("media refresh already pending");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(())) => Err(ManagementError::Internal(
                "The media prefetcher is not running".into(),
            )),
        }
    }

    // ------------------------------------------------------------------ user
    async fn logout(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    async fn ping(&self) {}

    // --------------------------------------------------------------- windows
    async fn list_diagnostics(&self) -> DiagnosticSet {
        self.engine.lock().await.diagnostics()
    }

    async fn network_status(&self) -> NetworkStatusView {
        let listener = self.web_listener.get();
        let snapshot = match &self.network {
            Some(provider) => provider.snapshot().await,
            None => NetworkSnapshot::unavailable(),
        };
        NetworkStatusView::new(
            snapshot.connectivity,
            snapshot.source,
            snapshot.interfaces,
            listener,
        )
    }

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
    /// Close out a reset started by `CoreEngine::begin_restart`, handing the
    /// engine the replacement process's handle — or `None` when there isn't
    /// one, which ends the session.
    ///
    /// Must run on every path out of `reset_current`: while a restart is in
    /// flight the engine ignores the activity's exit, so skipping this would
    /// leave a session that outlives its own process.
    async fn finish_reset(
        &self,
        handle: Option<shepherd_host_api::HostSessionHandle>,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) {
        let ended = {
            let mut eng = self.engine.lock().await;
            eng.finish_restart(handle, now_mono, now)
        };

        if let Some(shepherd_core::CoreEvent::SessionEnded {
            session_id,
            entry_id,
            reason,
            duration,
        }) = ended
        {
            (self.broadcast_fn)(Event::new(EventPayload::SessionEnded {
                session_id,
                entry_id,
                reason,
                duration,
            }));
        }

        let snap = self.engine.lock().await.get_state();
        (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
    }

    /// Restrictions from config alone: the running activity's override if it has
    /// one, otherwise the global `[service.volume]`.
    async fn policy_volume_restrictions(&self) -> VolumeRestrictions {
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

    /// The restrictions actually in force for a given output: the config
    /// restrictions above, combined with any per-output limits the parent set.
    ///
    /// The two are combined by taking the **stricter** of each bound rather than
    /// letting one override the other, so the result always fails safe. Capping
    /// gaming at 60 and headphones at 50 yields 50; neither setting can be used
    /// to raise a limit the other imposed.
    async fn volume_restrictions_for(&self, output_key: Option<&str>) -> VolumeRestrictions {
        let mut r = self.policy_volume_restrictions().await;
        let Some(key) = output_key else {
            return r;
        };
        let Ok(Some(row)) = self.store.get_audio_output(key) else {
            // Never seen, or the store is unavailable: the global limit stands.
            // A new device is therefore no louder than the machine's default,
            // and no quieter either.
            return r;
        };
        r.max_volume = stricter_max(r.max_volume, row.max_volume);
        r.min_volume = stricter_min(r.min_volume, row.min_volume);
        // A floor above the ceiling is unsatisfiable; the ceiling is the safety
        // bound, so it wins.
        if let (Some(min), Some(max)) = (r.min_volume, r.max_volume)
            && min > max
        {
            r.min_volume = Some(max);
        }
        r
    }

    /// Restrictions for whatever output is active right now.
    ///
    /// A failed read must not collapse to `None` here.
    /// [`Self::volume_restrictions_for`] answers `None` with the *global* limit,
    /// so a transient `pw-dump` failure would quietly raise the ceiling on an
    /// output the parent had capped lower — headphones pinned at 30 would accept
    /// 80 for as long as the fault lasted. A cap that relaxes itself under a
    /// fault is worse than no cap, so fall back to the last output actually
    /// observed: stale, but never more permissive than what was true while we
    /// could still see.
    async fn volume_restrictions(&self) -> VolumeRestrictions {
        let key = match self.volume.observe().await {
            Ok(snap) => snap.active.map(|o| o.key),
            Err(e) => {
                warn!(error = %e, "Could not read the active audio output; keeping the last one seen");
                self.last_audio_state
                    .lock()
                    .await
                    .as_ref()
                    .and_then(|seen| seen.output_key.clone())
            }
        };
        self.volume_restrictions_for(key.as_deref()).await
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
        let info = self.get_volume().await?;
        (self.broadcast_fn)(Event::new(EventPayload::VolumeChanged {
            percent: info.percent,
            muted: info.muted,
            restrictions: info.restrictions.clone(),
            output: info.output.clone(),
        }));
        Ok(info)
    }

    /// Pull the volume down if it sits above the ceiling now in force.
    ///
    /// Limits used to apply only to changes routed through us, so an output
    /// whose remembered volume already exceeded its cap stayed loud — which is
    /// most of the point of a headphone limit. Called when the active output
    /// changes and when a cap is set.
    async fn enforce_volume_ceiling(&self) {
        // Enforcement is the last place that should give up on a failed read:
        // "I cannot see which output this is" must not become "so leave it
        // loud". The reading is still available, and the last output seen is a
        // better guess than none — it can only make the ceiling stricter.
        let (percent, key) = match self.volume.observe().await {
            Ok(snap) => (snap.status.percent, snap.active_key().map(str::to_owned)),
            Err(_) => {
                let Ok(status) = self.volume.get_status().await else {
                    return;
                };
                let key = self
                    .last_audio_state
                    .lock()
                    .await
                    .as_ref()
                    .and_then(|seen| seen.output_key.clone());
                (status.percent, key)
            }
        };
        let restrictions = self.volume_restrictions_for(key.as_deref()).await;
        let Some(max) = restrictions.max_volume else {
            return;
        };
        if percent <= max {
            return;
        }
        info!(
            from = percent,
            to = max,
            output = key.as_deref().unwrap_or("?"),
            "Volume above the limit for this output; turning it down"
        );
        if let Err(e) = self.volume.set_volume(max).await {
            warn!(error = %e, "Failed to enforce the volume limit");
        }
    }

    /// One pass of the audio-output watch loop (issue #124).
    ///
    /// The default sink can change with no involvement from us — a headset is
    /// plugged in, WirePlumber auto-switches to a higher-priority device, the
    /// dock router diverts to HDMI — and because PipeWire remembers volume per
    /// route, the reading genuinely changes with it. Nothing else in the daemon
    /// observes that, so without this poll every client keeps displaying the
    /// previous output's volume until someone happens to change it.
    ///
    /// Also catches volume changed behind our back (a bare `wpctl` call), which
    /// is the same staleness with a different cause.
    ///
    /// Broadcasts only on an actual change, so a quiet host produces no events.
    pub async fn audio_watch_tick(&self) {
        let snap = match self.volume.observe().await {
            Ok(snap) => {
                self.clear_diagnostic(DiagnosticCode::AudioTopologyUnreadable);
                snap
            }
            // Skip the tick rather than baseline an empty topology, and say so
            // where a parent can see it: while this holds, the per-output caps
            // and both device lists are running on the last state observed.
            // Clears itself on the next tick that reads successfully.
            Err(e) => {
                warn!(error = %e, "Could not read the audio topology");
                self.raise_diagnostic(Diagnostic {
                    code: DiagnosticCode::AudioTopologyUnreadable,
                    subject: DiagnosticSubject::Service,
                    severity: DiagnosticSeverity::Warning,
                    // A sentence for a parent, not an error chain: the raw
                    // cause is already on the log line above.
                    message: "The audio devices could not be read, so volume limits are \
                              using the last state seen"
                        .to_string(),
                    remedy: Some(
                        "Check that PipeWire is running: systemctl --user status pipewire".into(),
                    ),
                    since: shepherd_util::now(),
                });
                return;
            }
        };

        let mut last = self.last_audio_state.lock().await;
        let now = ObservedAudioState::from_snapshot(&snap);
        if last.as_ref() == Some(&now) {
            return;
        }
        let first_observation = last.is_none();
        let switched = last
            .as_ref()
            .is_some_and(|prev| prev.output_key != now.output_key);
        *last = Some(now);
        drop(last);

        // The first tick only establishes the baseline; broadcasting there would
        // emit a spurious event on every daemon start.
        // Discovery: every output present becomes a row the parent can set a
        // limit on or switch to — not just the one playing, or the only device
        // you could see would be the one you wanted to switch away from. The
        // snapshot already lists them all, so this costs nothing beyond the poll.
        for out in &snap.outputs {
            if let Err(e) = self.store.record_audio_output_seen(out) {
                warn!(error = %e, key = %out.key, "Failed to record the observed audio output");
            }
        }

        if first_observation {
            // Still enforce on the first tick: shepherdd may have just started
            // onto an output that is already too loud.
            self.enforce_volume_ceiling().await;
            return;
        }
        if switched {
            self.enforce_volume_ceiling().await;
        }
        // A device appearing or disappearing rides on `VolumeChanged` rather
        // than an event of its own. Every client that renders the output list
        // already refetches it on this event, and the payload is the whole audio
        // state rather than just a number, so a new event type would add wire
        // surface to all three clients and tell them nothing new.
        if let Err(e) = self.broadcast_volume_change().await {
            warn!(error = %e, "Failed to broadcast observed audio change");
        }
    }

    /// The output we last saw in use, rebuilt from the store's row for it.
    ///
    /// Used when the topology cannot be read, so an answer names the output that
    /// was selected a moment ago instead of claiming there is none. The row is
    /// where the description and kind already live, so nothing has to be
    /// remembered twice.
    async fn last_seen_active_output(&self) -> Option<shepherd_api::AudioOutput> {
        let key = self
            .last_audio_state
            .lock()
            .await
            .as_ref()?
            .output_key
            .clone()?;
        self.store
            .get_audio_output(&key)
            .ok()
            .flatten()
            .map(|row| row.output)
    }

    /// Report a condition, if anything is listening. Never fails the caller.
    fn raise_diagnostic(&self, diagnostic: Diagnostic) {
        if let Some(sink) = &self.diagnostics {
            sink.raise(diagnostic);
        }
    }

    /// Withdraw a service-scoped condition. Cheap enough to call every tick.
    fn clear_diagnostic(&self, code: DiagnosticCode) {
        if let Some(sink) = &self.diagnostics {
            sink.clear(code, &DiagnosticSubject::Service);
        }
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

/// The lower of two ceilings; `None` means "no ceiling from this source".
fn stricter_max(a: Option<u8>, b: Option<u8>) -> Option<u8> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (x, None) | (None, x) => x,
    }
}

/// The higher of two floors; `None` means "no floor from this source".
fn stricter_min(a: Option<u8>, b: Option<u8>) -> Option<u8> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (x, None) | (None, x) => x,
    }
}

/// Resolve everything needed to spawn an entry: its kind, its spawn options
/// (firewall, browser policy, input-compat sidecars, log capture), and whether
/// it wants the XWayland HiDPI workaround.
///
/// Shared by `launch` and `reset_current` so a restarted activity comes back
/// under exactly the same rules it launched under — a reset that quietly
/// dropped the firewall or the browser policy would be a hole.
fn resolve_spawn(
    eng: &CoreEngine,
    id: &EntryId,
    now: DateTime<Local>,
) -> (Option<shepherd_api::EntryKind>, SpawnOptions, bool) {
    let entry = eng.policy().get_entry(id);
    let kind = entry.map(|e| e.kind.clone());
    let firewall =
        entry
            .and_then(|e| e.firewall.clone())
            .map(|fw| shepherd_host_api::FirewallSpec {
                default_deny: fw.default_deny,
                allow: fw.allow,
                deny: fw.deny,
            });
    let browser = entry
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
    let media_watched_grace_days = is_media.then(|| eng.policy().service.media.watched_grace_days);
    let media_cache_max_bytes = is_media.then(|| eng.policy().service.media.cache_max_bytes);
    // The household's SponsorBlock settings. The entry's own on/off override
    // rides on the entry kind and is applied by the host when it builds the
    // argv, so both directions of override work.
    let media_sponsorblock = is_media.then(|| {
        let sb = &eng.policy().service.media.sponsorblock;
        SponsorBlockSpec {
            enabled: sb.enabled,
            categories: sb.categories.clone(),
            api: sb.api.clone(),
        }
    });
    let needs_hidpi = entry.is_some_and(|e| e.xwayland_native_resolution);

    let log_path = eng.policy().service.capture_child_output.then(|| {
        let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
        let filename = format!(
            "{}_{}.log",
            id.as_str().replace(['/', '\\', ' '], "_"),
            timestamp
        );
        eng.policy().service.child_log_dir.join(filename)
    });

    let opts = SpawnOptions {
        entry_id: Some(id.as_str().to_string()),
        capture_stdout: log_path.is_some(),
        capture_stderr: log_path.is_some(),
        log_path,
        firewall,
        browser,
        input_compat,
        input_compat_options,
        connectivity_check,
        media_watched_grace_days,
        media_cache_max_bytes,
        media_sponsorblock,
        ..Default::default()
    };

    (kind, opts, needs_hidpi)
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
