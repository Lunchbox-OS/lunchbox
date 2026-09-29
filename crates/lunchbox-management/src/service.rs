//! `ManagementService`: every operation an administrator can perform on a
//! running lunchboxd, behind a single transport-agnostic trait.

use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDate};
use lunchbox_api::{
    AudioOutputRecord, BrightnessInfo, BrightnessRestrictions, DailyOverride, DesktopApp,
    Diagnostic, DiagnosticCode, DiagnosticSet, DiagnosticSeverity, DiagnosticSink,
    DiagnosticSubject, DisplayMode, DisplayState, EntryKind, EntryView, Event, EventPayload,
    GroupView, HealthStatus, HudOrientation, NetworkStatusView, SavedWifiNetwork,
    ServiceStateSnapshot, SessionEndReason, SessionInfo, StopMode, TokenStatus, UsageStat,
    VolumeInfo, VolumeRestrictions, WifiJoinRequest, WifiJoinState, WifiScanView, WindowAction,
    WindowInfo, WindowOwner, aggregate_networks,
};
use lunchbox_config::{BrightnessPolicy, VolumePolicy, parse_config};
use lunchbox_core::{BeginStopDecision, CoreEngine, CoreEvent, LaunchDecision, TokenAdjustError};
use lunchbox_host_api::{
    BrightnessController, DisplayController, HidpiController, HostAdapter, HudLayoutController,
    LightSensor, NetworkInfoProvider, NetworkSnapshot, SpawnOptions, SponsorBlockSpec,
    VolumeController, VolumeError, WifiController, WifiError,
};
use lunchbox_store::{AuditEvent, AuditEventType, Store};
use lunchbox_util::{EntryId, LimitSubject, MonotonicInstant, ProtectedFile, ProtectedFiles};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, mpsc, watch};
use tracing::{debug, info, warn};

use crate::auth::{AdminRoster, AdminRosterError, AdminSummary, EnrolmentRequestInfo};
use crate::auto_brightness::{AutoAction, AutoBrightnessCurve, AutoBrightnessState};
use crate::error::{ManagementError, ManagementResult};
use crate::listener::WebListenerHandle;
use crate::types::{LaunchOutcome, PolicyDocument};
use crate::webauth::{LoginRequestInfo, WebAuth, WebAuthError, WebAuthStatus, WebSessionInfo};

/// Store key under which the runtime auto-brightness on/off state persists.
pub const AUTO_BRIGHTNESS_SETTING_KEY: &str = "auto_brightness_enabled";

/// Transport-agnostic management operations. `lunchbox-http` and
/// `lunchbox-ble` both translate their wire format into calls on this trait.
///
/// The `#[management_rpc]` attribute expands to also emit a
/// `dispatch_json(svc, method, params) -> Result<Value, RpcDispatchError>`
/// function that BLE (and, in future, other JSON-RPC transports) can
/// use directly — no hand-written per-method match arms required.
/// See `lunchbox-management-macros` for the attribute's options.
#[lunchbox_management_macros::management_rpc]
#[async_trait]
pub trait ManagementService: Send + Sync {
    // Health / state
    async fn health(&self) -> HealthStatus;
    async fn service_state(&self) -> ServiceStateSnapshot;

    // Entries
    #[rpc(default(at = "lunchbox_util::now"))]
    async fn list_entries(&self, at: DateTime<Local>) -> Vec<EntryView>;
    #[rpc(default(at = "lunchbox_util::now"))]
    async fn get_entry(&self, id: &EntryId, at: DateTime<Local>) -> ManagementResult<EntryView>;

    // Groups (issue #5)
    /// Categories that share a schedule and a combined budget. Returns the
    /// group's own state; a member's individual limits are on its `EntryView`.
    #[rpc(default(at = "lunchbox_util::now"))]
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
    /// Called by `swayidle` through `lunchbox-launcher --screen-off/--screen-on`
    /// (issue #144): the compositor socket has no name on a hardened device, so
    /// the blanking has to run on the connection lunchboxd holds.
    ///
    /// The "is anything running?" check lives here rather than in the caller
    /// because it used to be a separate `--is-idle-allowed` process, and a
    /// launch landing between that check and the blank turned the screen off on
    /// a child mid-activity. "Anything" includes administrator mode (issue
    /// #154), which runs no session for the first half of that check to see.
    /// Returns whether it actually acted.
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

    // The policy file itself (issue #185). Read and replaced by the web
    // config editor.
    //
    // **Neither is `async`, deliberately.** `#[management_rpc]` turns every
    // async method into a `dispatch_json` arm, and a policy is tens of
    // kilobytes against BLE's 16 KiB frame cap
    // (`lunchbox_ble::protocol::MAX_FRAME_BYTES`) — so putting a config on
    // the JSON-RPC surface would be publishing a method that exists and
    // cannot work. They are reached over dedicated HTTP routes instead, the
    // same way #156 kept the login exchange off this trait. The macro skips
    // non-async items, so this is the whole of the mechanism.
    //
    // Synchronous also because [`lunchbox_util::ProtectedFiles`] is: on a
    // device each call is a round trip to the state custodian's socket.
    // Callers on an async runtime should use `spawn_blocking`.

    /// The policy file's exact bytes, with a tag for [`Self::write_policy`].
    ///
    /// Returns the text whether or not it parses. A config the daemon cannot
    /// read is exactly the one an editor is most needed for, and refusing to
    /// hand it over would leave the only fix to a device with no shell on it.
    fn read_policy(&self) -> ManagementResult<PolicyDocument> {
        Err(ManagementError::Unprocessable(
            "This device does not expose its policy file".into(),
        ))
    }

    /// Replace the policy file.
    ///
    /// Validates `text` with the parser the daemon boots from *before*
    /// anything touches the disk. That check is the control, not the editor's
    /// client-side validator: a policy lunchboxd cannot parse is survivable on
    /// reload — it keeps the running one — but fatal at startup, which on a
    /// device is a session that ends rather than a message someone reads.
    ///
    /// `if_match` is a [`PolicyDocument::version`] the caller believes is
    /// current; a mismatch is a [`ManagementError::Conflict`] and nothing is
    /// written. `None` skips the check, which is what a caller that has not
    /// read the file first is asking for.
    ///
    /// **Does not reload.** The write lands through a rename, which the state
    /// custodian's watch — or lunchboxd's own, on a device without one — turns
    /// into a reload within a second. That is the same path `sudoedit` and
    /// `lunchbox install policy` already take, and going around it here would
    /// only add a second `PolicyLoaded` row to the audit log a moment before
    /// the watcher's arrives.
    fn write_policy(&self, text: &str, if_match: Option<&str>) -> ManagementResult<PolicyDocument> {
        let _ = (text, if_match);
        Err(ManagementError::Unprocessable(
            "This device does not expose its policy file".into(),
        ))
    }

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

    // Web management authentication (issue #156)
    //
    // The login itself is not here. Signing in is a *pre-auth* HTTP exchange —
    // `POST /api/v1/auth/login` and friends — and a transport that has already
    // authenticated its peer, as BLE has by the time a write lands, has no use
    // for it. What is here is the half an authenticated administrator performs:
    // approving a browser's request from the phone, setting the password
    // without SSH, and ending a session on a device they no longer hold.

    /// Whether the web UI has a password yet, and whether a paired companion
    /// exists to approve a login. Answers the companion's "is this device set
    /// up?" and the browser's "which door do I offer?".
    async fn web_auth_status(&self) -> ManagementResult<WebAuthStatus>;

    /// Set or replace the web UI's password.
    ///
    /// No old password required: the caller has already proved they are the
    /// administrator by reaching this trait at all — over a bonded BLE link, or
    /// with a live session. This is the reset flow that means a parent who
    /// forgot the password does not have to find an SSH client.
    async fn set_web_password(&self, password: String) -> ManagementResult<()>;

    /// Every live browser session, so an administrator can see what is signed
    /// in and end anything they do not recognise.
    async fn list_web_sessions(&self) -> ManagementResult<Vec<WebSessionInfo>>;

    /// End one session by its public id.
    async fn revoke_web_session(&self, id: String) -> ManagementResult<()>;

    /// Browsers waiting on an approval, each with the six digits it is
    /// displaying. The parent compares those digits against the screen in
    /// front of them — the same Numeric Comparison ritual as BLE pairing, for
    /// the same reason: a racing attacker's request shows a different number.
    async fn list_login_requests(&self) -> ManagementResult<Vec<LoginRequestInfo>>;

    /// Approve a waiting browser, minting the session it collects on its next
    /// poll.
    async fn approve_login_request(&self, id: String) -> ManagementResult<()>;

    /// Refuse a waiting browser, so it stops waiting and says so.
    async fn deny_login_request(&self, id: String) -> ManagementResult<()>;

    // Administrators (issue #149).
    //
    // On the trait rather than routed inside `lunchbox-ble`, so a browser can
    // do them too. A parent at a laptop is at least as likely to be the one
    // holding the device when a second phone asks to be let in, and leaving
    // approval to the phone alone would have meant the household's only way to
    // add a caregiver was to find whoever already had one.
    //
    // Reaching the claim machine from here is what [`crate::AdminRoster`] is
    // for: it owns the records, the bonds and the enrolment handshake, and
    // lives in a crate that sits above this one.

    /// Every phone that may administer this device.
    async fn list_admins(&self) -> ManagementResult<Vec<AdminSummary>>;

    /// Remove one, and forget its Bluetooth bond.
    ///
    /// Refuses the last one — that would leave the device with no
    /// administrator and a phone still bonded to it. `factory_reset`, which is
    /// the BLE transport's own, is the way to unclaim a device.
    async fn revoke_admin(&self, id: String) -> ManagementResult<()>;

    /// Phones waiting to be let in, each with the six digits it is showing.
    /// Whoever approves compares those against that phone's screen — the same
    /// Numeric Comparison ritual as pairing and as a browser login, for the
    /// same reason: a racing request carries a different number.
    async fn list_enrolment_requests(&self) -> ManagementResult<Vec<EnrolmentRequestInfo>>;

    /// Let a waiting phone administer this device.
    async fn approve_enrolment_request(&self, id: String) -> ManagementResult<AdminSummary>;

    /// Turn a waiting phone away.
    async fn deny_enrolment_request(&self, id: String) -> ManagementResult<()>;

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

    // Wi-Fi configuration (issue #194)
    /// Ask for a fresh scan and return at once.
    ///
    /// Results arrive as property changes seconds later, so there is nothing
    /// to wait for. A UI calls this when its network page opens and then polls
    /// [`Self::wifi_networks`].
    async fn wifi_scan(&self) -> ManagementResult<()>;

    /// What is in range, plus what the last join is doing.
    ///
    /// One call for both because the UI is already polling this to refresh
    /// signal strengths, and a join's outcome cannot be returned by the call
    /// that started it — association plus DHCP outlives the companion's
    /// 15-second RPC timeout.
    async fn wifi_networks(&self) -> WifiScanView;

    /// Networks this device already knows. Never includes a password.
    async fn wifi_saved_networks(&self) -> ManagementResult<Vec<SavedWifiNetwork>>;

    /// Remember a network, and join it if the request says to.
    ///
    /// Returns once the profile is written and the activation accepted, not
    /// once the device is on the network. Saving over an existing profile for
    /// the same SSID updates it rather than adding a second.
    async fn wifi_save(&self, request: WifiJoinRequest) -> ManagementResult<SavedWifiNetwork>;

    /// Join a network this device already has a profile for.
    async fn wifi_connect(&self, id: String) -> ManagementResult<()>;

    /// Delete a saved profile. `false` when there was nothing to delete.
    async fn wifi_forget(&self, id: String) -> ManagementResult<bool>;

    // Administrator mode (issue #154)
    /// Relax the kiosk so a caregiver can set the device up in place: the
    /// compositor's key grabs are released, the screen stops blanking, and
    /// windows opened here stop being reported as unsupervised.
    ///
    /// Refused while an activity is running — the caregiver stops it first,
    /// rather than this ending a child's session from a button that does not
    /// say so.
    async fn enter_admin_mode(&self) -> ManagementResult<()>;

    /// Leave administrator mode **and log the desktop session out**.
    ///
    /// The logout is the point, not a side effect: nothing tracks what a
    /// caregiver started in the mode, so ending the session is the only way to
    /// guarantee the child's next activity meets the machine it would have met
    /// at boot. Whatever is still on screen goes with it, and the device comes
    /// back to a fresh kiosk.
    ///
    /// Deliberately never refused, and idempotent. This is the escape hatch:
    /// the HUD only offers its own exit once no windows are left, so a window
    /// that will not close would otherwise strand the device. Leaving from
    /// here always works, whatever is still on screen. A call that finds the
    /// mode already off leaves the session alone — it did not leave anything,
    /// so it has nothing to clean up after.
    async fn exit_admin_mode(&self) -> ManagementResult<()>;

    /// The compositor reports the seat has been idle for the configured span.
    ///
    /// Leaves administrator mode, but **only when nothing is open**. Walking
    /// away is a legitimate workflow — a Steam download on a slow connection is
    /// the motivating case — so a timeout that closed a caregiver's windows
    /// would break the most valuable thing the mode does. With windows up this
    /// is a no-op and the mode persists until somebody leaves it deliberately.
    ///
    /// Leaving this way logs the session out too, exactly as
    /// [`ManagementService::exit_admin_mode`] does — a mode nobody came back
    /// to is no cleaner than one somebody left on purpose.
    ///
    /// Returns whether it actually left. Idle notification comes from
    /// `swayidle`, which is already the device's idle authority.
    async fn admin_idle_timeout(&self) -> ManagementResult<bool>;

    /// Lock the screen (issue #154).
    ///
    /// Available only inside administrator mode, and the way to walk away from
    /// a device mid-setup: the Steam download keeps running, and the child
    /// cannot touch it. There is deliberately no local way back — see
    /// [`ManagementService::unlock_device`].
    async fn lock_device(&self) -> ManagementResult<()>;

    /// Unlock the screen.
    ///
    /// The counterpart, and the reason the lock is safe to offer: it exists
    /// only here, on the management transports, so the person who locked the
    /// device is the only one who can open it again.
    async fn unlock_device(&self) -> ManagementResult<()>;

    /// Every application the system's `.desktop` files offer, as a normal
    /// desktop would list them (issue #154).
    ///
    /// Readable outside administrator mode — it is a catalogue, and a picker
    /// wants it drawn before the mode is entered — but nothing in it can be
    /// started until the mode is on.
    async fn list_desktop_apps(&self) -> ManagementResult<Vec<DesktopApp>>;

    /// Start one of them, by desktop file ID.
    ///
    /// **Refused unless administrator mode is on.** Otherwise this would be a
    /// permanently open "run anything" RPC on a device whose entire purpose is
    /// that only configured activities run.
    async fn launch_desktop_app(&self, id: String) -> ManagementResult<()>;

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
    lunchbox_util::now().date_naive()
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
    fn from_snapshot(snap: &lunchbox_host_api::AudioSnapshot) -> Self {
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

/// Write `text` to `path` through a temp file and a rename.
///
/// The same shape [`lunchbox_util::LocalProtectedFiles::write`] uses, for the
/// case it does not cover: a device without the state custodian, whose policy
/// is an ordinary file at an arbitrary path. A partial write must not be able
/// to leave a policy the daemon cannot parse, and the rename is also what the
/// config watcher is watching for.
fn write_atomically(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Production implementation of [`ManagementService`]. Composes the
/// daemon's existing collaborators; constructed once by `lunchboxd` and
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
    /// Where the policy lives when the custodian does not hold it: the path
    /// `--config` named. On a device with a custodian this is the *signpost*,
    /// which grants nothing — see `policy_files`.
    pub config_path: PathBuf,
    /// The state custodian's files, when it holds this device's policy
    /// (issue #157). `Some` exactly when `lunchboxd`'s own
    /// `StateSource::holds_policy` is true.
    ///
    /// Load-bearing rather than decorative: without it this service reads
    /// `config_path`, which on an installed device is the zero-entry signpost
    /// that stands where the policy used to be. A reload from there empties
    /// the launcher.
    pub policy_files: Option<Arc<dyn ProtectedFiles>>,
    /// Nudges lunchboxd's media prefetcher to re-fetch libraries and segments
    /// immediately (issue #165). `None` on any embedding without a prefetcher —
    /// in which case [`ManagementService::refresh_media`] reports that rather
    /// than answering "done" to a button that did nothing.
    ///
    /// A bare channel rather than a collaborator trait because the work is on
    /// the far side of it: this crate must not link the media stack, and the
    /// prefetcher already owns every decision about what a sweep does.
    pub media_refresh_tx: Option<mpsc::Sender<()>>,
    /// Fires when lunchboxd should begin graceful shutdown. The logout
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
    /// How to scan for and join wireless networks (issue #194). `None` on an
    /// embedding with no wireless backend, which reports itself as
    /// unsupported — a device that cannot look, not a device with no networks
    /// in range.
    pub wifi: Option<Arc<dyn WifiController>>,
    /// What the web management interface is really doing, as opposed to what
    /// the config asked for. Written by whoever owns the listener; `Disabled`
    /// by default, which is the truth for an embedding that never starts one.
    pub web_listener: WebListenerHandle,
    /// The web UI's credential store (issue #156). `None` wherever the
    /// management API is switched off — in which case the seven web-auth
    /// methods answer "not configured" rather than pretending to work, because
    /// a companion that silently set a password on a device with no HTTP
    /// server would be lying to the person holding the phone.
    pub web_auth: Option<Arc<WebAuth>>,
    /// The device's administrator roster (issue #149) — in practice the BLE
    /// claim machine, which owns the records and the bonds.
    ///
    /// Empty on a device with BLE management switched off, and in every
    /// embedding that has no claim machine. The roster methods then answer
    /// "not enabled" rather than an empty list, because "nobody administers
    /// this device" and "this device cannot tell you" are different answers
    /// and a UI should not show the first when it means the second.
    ///
    /// Behind a lock and set after construction, like
    /// [`crate::webauth::WebAuth::set_companion`] and for the same reason: the
    /// claim machine does not exist until the BLE server is built, and the BLE
    /// server needs this service to build.
    pub admins: std::sync::RwLock<Option<Arc<dyn AdminRoster>>>,
}

impl DefaultManagementService {
    /// The wireless backend, or the error a device without one owes its
    /// caller. Separate from an empty scan: "this device cannot look" and
    /// "nothing is in range" are different answers.
    fn wifi(&self) -> ManagementResult<&Arc<dyn WifiController>> {
        self.wifi
            .as_ref()
            .ok_or_else(|| ManagementError::Unprocessable("This device has no Wi-Fi".into()))
    }
}

/// Map a backend failure onto the transports' vocabulary.
///
/// `NotAuthorized` becomes `Forbidden` rather than `Internal` on purpose: it
/// is the one failure here that a person can fix, and it needs to reach a UI
/// as something other than "something went wrong".
fn wifi_error(error: WifiError) -> ManagementError {
    match error {
        WifiError::NoAdapter => ManagementError::Unprocessable(error.to_string()),
        WifiError::NotAuthorized => ManagementError::Forbidden(error.to_string()),
        WifiError::UnknownNetwork => ManagementError::NotFound(error.to_string()),
        WifiError::Rejected(message) => ManagementError::BadRequest(message),
        WifiError::Backend(message) => ManagementError::Internal(message),
    }
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
        let now = lunchbox_util::now();
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
        // in lunchboxd::main).
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
        let now = lunchbox_util::now();
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
                    StopMode::Graceful => lunchbox_host_api::StopMode::Graceful {
                        timeout: Duration::from_secs(5),
                    },
                    StopMode::Force => lunchbox_host_api::StopMode::Force,
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
        let now = lunchbox_util::now();
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
                    lunchbox_host_api::StopMode::Graceful {
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
        let now = lunchbox_util::now();
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
            eng.adjust_tokens(id, delta_seconds, lunchbox_util::now())
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
        //
        // Administrator mode counts alongside an activity (issue #154): the
        // screen must not blank on a caregiver halfway through a package
        // install or a Steam login, and the mode deliberately creates no
        // session for the first check to see.
        if !on {
            let busy = {
                // One lock for both questions: two would be a needless second
                // acquire on the daemon's hottest mutex, on a path swayidle
                // takes every two minutes.
                let eng = self.engine.lock().await;
                eng.current_session().is_some() || eng.admin_mode()
            };
            if busy {
                return Ok(false);
            }
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
        // Read from where this device actually keeps its policy, which since
        // #157 is the state custodian rather than a path in the kiosk user's
        // home. Reading `config_path` here used to reload the *signpost* — a
        // valid policy with zero entries — so pressing "reload" on an
        // installed device emptied the launcher until something wrote the real
        // file again.
        match self.policy_text().and_then(|text| {
            parse_config(&text).map_err(|e| ManagementError::Unprocessable(e.to_string()))
        }) {
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
                Err(e)
            }
        }
    }

    fn read_policy(&self) -> ManagementResult<PolicyDocument> {
        Ok(PolicyDocument::of(self.policy_text()?))
    }

    fn write_policy(&self, text: &str, if_match: Option<&str>) -> ManagementResult<PolicyDocument> {
        // Before anything else, and before anything touches the disk. The
        // editor validates too, in wasm, but that is an affordance the caller
        // controls; this is the check.
        let policy =
            parse_config(text).map_err(|e| ManagementError::Unprocessable(e.to_string()))?;

        if let Some(expected) = if_match {
            // A missing file reads as "no version", which no caller can match,
            // so a device whose policy vanished under an open editor gets the
            // conflict rather than a silent recreate.
            let current = self
                .policy_text()
                .ok()
                .map(|t| PolicyDocument::version_of(&t));
            if current.as_deref() != Some(expected) {
                return Err(ManagementError::Conflict(
                    "The policy on the device changed since this copy of it was read".into(),
                ));
            }
        }

        match &self.policy_files {
            Some(files) => files.write(ProtectedFile::Config, text).map_err(|e| {
                ManagementError::Internal(format!(
                    "The state custodian refused to write the policy: {e}"
                ))
            })?,
            None => write_atomically(&self.config_path, text).map_err(|e| {
                ManagementError::Internal(format!(
                    "Could not write {}: {e}",
                    self.config_path.display()
                ))
            })?,
        }

        let entry_count = policy.entries.len();
        // Best-effort, like every other audit call on this path: failing to
        // record a change that happened must not report the change as failed.
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::PolicyWritten {
                entry_count,
            }));
        info!(
            entry_count,
            custodial = self.policy_files.is_some(),
            "Policy replaced through the management API"
        );

        Ok(PolicyDocument::of(text.to_string()))
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

    // -------------------------------------------------------------- web auth
    async fn web_auth_status(&self) -> ManagementResult<WebAuthStatus> {
        Ok(self.require_web_auth()?.status())
    }

    async fn set_web_password(&self, password: String) -> ManagementResult<()> {
        self.require_web_auth()?
            .set_password(&password)
            .map_err(web_auth_error)
    }

    async fn list_web_sessions(&self) -> ManagementResult<Vec<WebSessionInfo>> {
        // No session is `current` from here: this trait is reached over BLE
        // and over a browser's own connection alike, and only the HTTP layer
        // knows which session is asking. The web UI marks its own row through
        // `GET /api/v1/auth/sessions` instead.
        Ok(self.require_web_auth()?.list_sessions(None))
    }

    async fn revoke_web_session(&self, id: String) -> ManagementResult<()> {
        self.require_web_auth()?
            .revoke_session(&id)
            .map_err(web_auth_error)
    }

    async fn list_login_requests(&self) -> ManagementResult<Vec<LoginRequestInfo>> {
        Ok(self.require_web_auth()?.list_login_requests())
    }

    async fn approve_login_request(&self, id: String) -> ManagementResult<()> {
        self.require_web_auth()?
            .approve_login_request(&id)
            .map_err(web_auth_error)
    }

    async fn deny_login_request(&self, id: String) -> ManagementResult<()> {
        self.require_web_auth()?
            .deny_login_request(&id)
            .map_err(web_auth_error)
    }

    async fn list_admins(&self) -> ManagementResult<Vec<AdminSummary>> {
        Ok(self.require_admins()?.list_admins())
    }

    async fn revoke_admin(&self, id: String) -> ManagementResult<()> {
        self.require_admins()?
            .revoke_admin(&id)
            .map(|_| ())
            .map_err(admin_roster_error)
    }

    async fn list_enrolment_requests(&self) -> ManagementResult<Vec<EnrolmentRequestInfo>> {
        Ok(self.require_admins()?.list_enrolment_requests())
    }

    async fn approve_enrolment_request(&self, id: String) -> ManagementResult<AdminSummary> {
        self.require_admins()?
            .approve_enrolment_request(&id)
            .map_err(admin_roster_error)
    }

    async fn deny_enrolment_request(&self, id: String) -> ManagementResult<()> {
        self.require_admins()?
            .deny_enrolment_request(&id)
            .map_err(admin_roster_error)
    }

    // ------------------------------------------------------------------ user
    async fn logout(&self) {
        self.request_logout();
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

    // ------------------------------------------------------------------ wifi
    async fn wifi_scan(&self) -> ManagementResult<()> {
        self.wifi()?.scan().await.map_err(wifi_error)
    }

    async fn wifi_networks(&self) -> WifiScanView {
        // Infallible: a network page's only response to an error is to render
        // nothing, and "this device has no Wi-Fi" is what an absent backend
        // truthfully means.
        let Some(wifi) = &self.wifi else {
            return WifiScanView {
                supported: false,
                radio_enabled: false,
                networks: Vec::new(),
                truncated: false,
                last_scan_age_s: None,
                join: WifiJoinState::Idle,
                can_configure: false,
            };
        };
        let snapshot = wifi.networks().await;
        let (networks, truncated) = aggregate_networks(snapshot.networks);
        // Checked against the radio only when there is a radio reading to
        // check it against; see `WifiJoinState::reconciled`.
        let join = wifi.join_state().await;
        let join = if snapshot.supported {
            join.reconciled(&networks)
        } else {
            join
        };
        WifiScanView {
            supported: snapshot.supported,
            radio_enabled: snapshot.radio_enabled,
            networks,
            truncated,
            last_scan_age_s: snapshot.last_scan_age_s,
            join,
            can_configure: wifi.can_configure().await,
        }
    }

    async fn wifi_saved_networks(&self) -> ManagementResult<Vec<SavedWifiNetwork>> {
        self.wifi()?.saved().await.map_err(wifi_error)
    }

    async fn wifi_save(&self, request: WifiJoinRequest) -> ManagementResult<SavedWifiNetwork> {
        // Validate before the backend, and before the audit row: a rejected
        // request did not happen, and saying why beats twenty seconds of
        // association failing for a reason that reads as something else.
        request
            .validate()
            .map_err(|e| ManagementError::BadRequest(e.message().to_string()))?;

        let saved = self.wifi()?.save(&request).await.map_err(wifi_error)?;

        // Best-effort, like every other audit call on this path: failing to
        // record a change that happened must not report the change as failed.
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::WifiNetworkSaved {
                ssid: saved.ssid.clone(),
                connected: request.connect,
            }));
        // `request` is deliberately not in this line. Its Debug redacts the
        // password, but naming the field at all invites someone to widen it.
        info!(
            ssid = %saved.ssid,
            connect = request.connect,
            "saved a Wi-Fi network"
        );
        Ok(saved)
    }

    async fn wifi_connect(&self, id: String) -> ManagementResult<()> {
        self.wifi()?.connect(&id).await.map_err(wifi_error)
    }

    async fn wifi_forget(&self, id: String) -> ManagementResult<bool> {
        let wifi = self.wifi()?;
        // Read the name before deleting it: afterwards there is nothing left
        // to look it up from, and an audit row saying only a UUID answers
        // nobody's question about why the device fell off the network.
        let ssid = wifi
            .saved()
            .await
            .ok()
            .and_then(|saved| saved.into_iter().find(|n| n.id == id).map(|n| n.ssid));

        let removed = wifi.forget(&id).await.map_err(wifi_error)?;
        if removed {
            let _ =
                self.store
                    .append_audit(AuditEvent::new(AuditEventType::WifiNetworkForgotten {
                        ssid: ssid.clone().unwrap_or_else(|| id.clone()),
                    }));
            info!(ssid = ?ssid, "forgot a Wi-Fi network");
        }
        Ok(removed)
    }

    async fn list_windows(&self) -> ManagementResult<Vec<WindowInfo>> {
        self.host
            .list_windows()
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))
    }

    // ------------------------------------------------------- administrator
    async fn enter_admin_mode(&self) -> ManagementResult<()> {
        let (event, snap) = {
            let mut eng = self.engine.lock().await;
            let event = eng.enter_admin_mode().map_err(|entry_id| {
                ManagementError::Conflict(format!(
                    "'{entry_id}' is running; stop it before entering administrator mode"
                ))
            })?;
            (event, eng.get_state())
        };

        // The compositor is told after the engine has committed, so a failure
        // here leaves the mode on everywhere except the key grabs, rather than
        // a daemon that denies being in a mode it is in. Reported rather than
        // swallowed: a caregiver whose Ctrl+w still ends the session needs to
        // know why.
        if let Err(e) = self.host.set_admin_mode(true).await {
            warn!(error = %e, "Entered administrator mode, but the compositor did not switch binding mode");
        }
        self.broadcast_admin_mode(event, snap);
        Ok(())
    }

    async fn exit_admin_mode(&self) -> ManagementResult<()> {
        self.leave_admin_mode(false).await;
        Ok(())
    }

    async fn admin_idle_timeout(&self) -> ManagementResult<bool> {
        if !self.engine.lock().await.admin_mode() {
            return Ok(false);
        }

        // Lunchbox's own furniture — the launcher, the HUD — is always mapped,
        // so "nothing is open" means nothing the caregiver opened. A window
        // stashed on the scratchpad counts as open: it is somebody's work, and
        // it comes back.
        let open = self
            .host
            .list_windows()
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?
            .into_iter()
            .filter(|w| w.owner != WindowOwner::Lunchbox)
            .count();
        if open > 0 {
            // Decision 10 of the design: with work still on screen the timeout
            // locks rather than leaving. Walking away from a slow download is a
            // supported way to use the mode, so the timeout must protect the
            // device without touching what is running on it.
            debug!(
                open,
                "Seat idle with windows open; locking instead of leaving"
            );
            self.set_locked(true, true).await?;
            return Ok(false);
        }

        // Same path as the deliberate exit, logout included: a timed-out
        // session is no cleaner than one somebody left on purpose.
        if self.leave_admin_mode(true).await {
            info!("Administrator mode timed out with nothing open");
            return Ok(true);
        }
        // Raced with a deliberate exit; the mode is off either way.
        Ok(false)
    }

    async fn lock_device(&self) -> ManagementResult<()> {
        self.set_locked(true, false).await
    }

    async fn unlock_device(&self) -> ManagementResult<()> {
        self.set_locked(false, false).await
    }

    async fn list_desktop_apps(&self) -> ManagementResult<Vec<DesktopApp>> {
        // Reads a few dozen small files across the XDG search path. Off the
        // async worker so a slow or stale network mount cannot stall the
        // runtime, which is also where every other RPC is served from.
        tokio::task::spawn_blocking(lunchbox_config::desktop::list_desktop_apps)
            .await
            .map_err(|e| ManagementError::Internal(format!("desktop scan failed: {e}")))
    }

    async fn launch_desktop_app(&self, id: String) -> ManagementResult<()> {
        if !self.engine.lock().await.admin_mode() {
            return Err(ManagementError::Conflict(
                "Administrator mode is not on; turn it on before launching applications".into(),
            ));
        }

        let wanted = id.clone();
        let entry = tokio::task::spawn_blocking(move || {
            lunchbox_config::desktop::find_desktop_app(&wanted)
        })
        .await
        .map_err(|e| ManagementError::Internal(format!("desktop lookup failed: {e}")))?
        .ok_or_else(|| ManagementError::NotFound(format!("No application with id '{id}'")))?;

        let argv = if entry.app.terminal {
            lunchbox_config::desktop::terminal_command(&entry.argv).ok_or_else(|| {
                ManagementError::Unprocessable(format!(
                    "'{}' needs a terminal emulator and none is installed",
                    entry.app.name
                ))
            })?
        } else {
            entry.argv.clone()
        };

        // Audited before the spawn, and deliberately even if the spawn then
        // fails: an unsupervised launch leaves no other trace, so "it was
        // asked for" is worth more than "it definitely started".
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::AdminAppLaunched {
                id: entry.app.id.clone(),
                name: entry.app.name.clone(),
            }));

        self.host
            .launch_unsupervised(&argv)
            .await
            .map_err(|e| ManagementError::Internal(e.to_string()))?;
        info!(id = %entry.app.id, name = %entry.app.name, "Launched from the administrator picker");
        Ok(())
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

/// Map a credential-store failure onto the transport-agnostic error.
///
/// The distinctions that survive are the ones a caller can act on: a lockout
/// and a wrong password are both "no", but only one of them is worth waiting
/// out, so they do not collapse into the same status.
fn admin_roster_error(e: AdminRosterError) -> ManagementError {
    let message = e.to_string();
    match e {
        AdminRosterError::NoSuchRequest | AdminRosterError::NoSuchAdmin => {
            ManagementError::NotFound(message)
        }
        // Refusing the last administrator is the wrong *operation*, not a
        // caller without standing: `factory_reset` is the one that unclaims a
        // device, and it drops the bond too.
        AdminRosterError::LastAdmin => ManagementError::Conflict(message),
        AdminRosterError::EnrolmentDenied => ManagementError::Forbidden(message),
        AdminRosterError::Store(_) => ManagementError::Internal(message),
    }
}

fn web_auth_error(e: WebAuthError) -> ManagementError {
    match e {
        WebAuthError::NotConfigured => ManagementError::Conflict(e.to_string()),
        WebAuthError::AlreadyConfigured => ManagementError::Conflict(e.to_string()),
        WebAuthError::BadPassword | WebAuthError::BadEnrolmentCode => {
            ManagementError::Forbidden(e.to_string())
        }
        WebAuthError::LockedOut(_) => ManagementError::Forbidden(e.to_string()),
        WebAuthError::NoSuchRequest | WebAuthError::NoSuchSession => {
            ManagementError::NotFound(e.to_string())
        }
        WebAuthError::PasswordTooShort(_) => ManagementError::Unprocessable(e.to_string()),
        WebAuthError::Store(_) => ManagementError::Internal(e.to_string()),
    }
}

impl DefaultManagementService {
    /// The policy file's text, from wherever this device keeps it.
    ///
    /// One place, so a reload and a read can never disagree about which file
    /// decides what a child may do. Mirrors `lunchboxd`'s own
    /// `StateSource::load_policy`: the custodian when it holds a policy, the
    /// local path otherwise.
    fn policy_text(&self) -> ManagementResult<String> {
        match &self.policy_files {
            Some(files) => files
                .read(ProtectedFile::Config)
                .map_err(|e| {
                    ManagementError::Internal(format!(
                        "The state custodian would not read the policy: {e}"
                    ))
                })?
                .ok_or_else(|| {
                    ManagementError::NotFound("The state custodian holds no policy file".into())
                }),
            None => std::fs::read_to_string(&self.config_path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ManagementError::NotFound(format!(
                        "No policy file at {}",
                        self.config_path.display()
                    ))
                } else {
                    ManagementError::Internal(format!(
                        "Could not read {}: {e}",
                        self.config_path.display()
                    ))
                }
            }),
        }
    }

    /// Plug in the administrator roster, once the BLE server has built one.
    pub fn set_admin_roster(&self, roster: Option<Arc<dyn AdminRoster>>) {
        *self.admins.write().expect("admin roster lock poisoned") = roster;
    }

    fn require_admins(&self) -> ManagementResult<Arc<dyn AdminRoster>> {
        self.admins
            .read()
            .expect("admin roster lock poisoned")
            .clone()
            .ok_or_else(|| {
                ManagementError::Conflict(
                    "Bluetooth management is not enabled on this device, so it has no \
                     administrators to list"
                        .into(),
                )
            })
    }

    fn require_web_auth(&self) -> ManagementResult<&Arc<WebAuth>> {
        self.web_auth.as_ref().ok_or_else(|| {
            ManagementError::Conflict("the management API is not enabled on this device".into())
        })
    }

    /// Announce an administrator-mode transition (issue #154).
    ///
    /// Both the delta and a fresh full snapshot go out. The delta is what the
    /// shells react to; the snapshot is what keeps `admin_mode`, and the entry
    /// availability that changes with it, correct for a client that joined
    /// mid-transition — every entry becomes unavailable on entry and available
    /// again on exit, and nothing else would tell them so.
    fn broadcast_admin_mode(&self, event: CoreEvent, snap: ServiceStateSnapshot) {
        let CoreEvent::AdminModeChanged { active } = event else {
            debug_assert!(false, "broadcast_admin_mode called with {event:?}");
            return;
        };
        (self.broadcast_fn)(Event::new(EventPayload::AdminModeChanged { active }));
        (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
    }

    /// Leave administrator mode, whoever asked and for whatever reason, and
    /// log the desktop session out behind it (issue #154).
    ///
    /// **Leaving the mode ends the session.** Everything a caregiver starts in
    /// administrator mode is started deliberately outside supervision —
    /// `launch_unsupervised` even `setsid`s it, so a package install survives a
    /// daemon restart — which means nothing here can enumerate what is left
    /// running, let alone reap it. A signed-in Steam client, a dbus-activated
    /// service that was not there at boot, a package manager still holding its
    /// lock: each one changes how the *child's* next activity behaves, and none
    /// of them appears in the window list the HUD's "no windows left" gate
    /// reads. Ending the session is the only reset this daemon can actually
    /// promise, and the device comes straight back to a fresh kiosk.
    ///
    /// The logout is asked for only when the engine really did leave the mode.
    /// `exit_admin_mode` is idempotent and ungated on purpose — it is the
    /// rescue path for a window that will not close — and a second, racing call
    /// must not tear down whatever session the device has moved on to.
    ///
    /// Returns whether this call is the one that left.
    async fn leave_admin_mode(&self, timed_out: bool) -> bool {
        let (event, snap) = {
            let mut eng = self.engine.lock().await;
            (eng.exit_admin_mode(timed_out), eng.get_state())
        };
        // `CoreEngine::exit_admin_mode` clears the lock flag, because a lock
        // whose only exit is an administrator RPC cannot outlive the mode that
        // reaches it. The compositor has to be told the same thing or the
        // screen stays covered while every client reports it open — a device
        // that looks bricked, with no button anywhere offering to fix it.
        // Idempotent, so it is asked unconditionally.
        self.release_screen_lock().await;
        // Asked unconditionally too, so a compositor left in the admin binding
        // mode by a crash is recovered by pressing the button again.
        if let Err(e) = self.host.set_admin_mode(false).await {
            warn!(error = %e, "Left administrator mode, but the compositor did not switch binding mode");
        }

        let Some(event) = event else {
            return false;
        };

        // Announced before the logout is asked for: the remote clients are the
        // ones that outlive the session, and "the mode is off" is the last
        // thing they can be told before the device goes away underneath them.
        self.broadcast_admin_mode(event, snap);
        info!(
            timed_out,
            "Left administrator mode; logging the session out to clear anything it started"
        );
        // Only asks. lunchboxd's main loop stops the (impossible, in this mode)
        // session, drains the HTTP server and then tears sway down, which is
        // what keeps this call's own reply ahead of the compositor going away.
        self.request_logout();
        true
    }

    /// Ask lunchboxd to end the desktop session.
    ///
    /// The one place that flips the shutdown watch, so [`Self::logout`] and the
    /// administrator-mode exit cannot drift into meaning different things.
    fn request_logout(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Tell the host to uncover the screen, whatever it currently believes.
    ///
    /// Called on every path that leaves administrator mode. A failure is logged
    /// rather than propagated: leaving the mode must not be refusable, and the
    /// caller cannot do anything useful with the error anyway — pressing unlock
    /// retries it.
    async fn release_screen_lock(&self) {
        if let Err(e) = self.host.set_locked(false).await {
            warn!(error = %e, "Could not release the screen lock while leaving administrator mode");
        }
    }

    /// Lock or unlock, keeping the engine, the compositor and every client in
    /// step (issue #154).
    ///
    /// The order differs by direction, and both are deliberate. Locking tells
    /// the *host* first: if the lock client cannot start, the screen is not
    /// covered, and a daemon that had already announced `locked: true` would be
    /// telling every client something untrue about a safety control. Unlocking
    /// commits to the engine first, because the RPC must not be refusable —
    /// a host that fails to release the lock leaves a stuck screen that
    /// pressing the button again can retry.
    async fn set_locked(&self, locked: bool, timed_out: bool) -> ManagementResult<()> {
        if locked {
            if !self.engine.lock().await.admin_mode() {
                return Err(ManagementError::Conflict(
                    "The screen can only be locked from administrator mode".into(),
                ));
            }
            self.host.set_locked(true).await.map_err(|e| {
                ManagementError::Internal(format!("could not lock the screen: {e}"))
            })?;
        }

        let (event, snap) = {
            let mut eng = self.engine.lock().await;
            let event = if locked {
                eng.lock(timed_out).map_err(|_| {
                    ManagementError::Conflict(
                        "The screen can only be locked from administrator mode".into(),
                    )
                })?
            } else {
                eng.unlock()
            };
            (event, eng.get_state())
        };

        if !locked {
            self.release_screen_lock().await;
        }

        if let Some(CoreEvent::LockChanged { locked }) = event {
            (self.broadcast_fn)(Event::new(EventPayload::LockChanged { locked }));
            (self.broadcast_fn)(Event::new(EventPayload::StateChanged(snap)));
        }
        Ok(())
    }

    /// Close out a reset started by `CoreEngine::begin_restart`, handing the
    /// engine the replacement process's handle — or `None` when there isn't
    /// one, which ends the session.
    ///
    /// Must run on every path out of `reset_current`: while a restart is in
    /// flight the engine ignores the activity's exit, so skipping this would
    /// leave a session that outlives its own process.
    async fn finish_reset(
        &self,
        handle: Option<lunchbox_host_api::HostSessionHandle>,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) {
        let ended = {
            let mut eng = self.engine.lock().await;
            eng.finish_restart(handle, now_mono, now)
        };

        if let Some(lunchbox_core::CoreEvent::SessionEnded {
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
                    since: lunchbox_util::now(),
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
            // Still enforce on the first tick: lunchboxd may have just started
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
    async fn last_seen_active_output(&self) -> Option<lunchbox_api::AudioOutput> {
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
) -> (Option<lunchbox_api::EntryKind>, SpawnOptions, bool) {
    let entry = eng.policy().get_entry(id);
    let kind = entry.map(|e| e.kind.clone());
    let firewall =
        entry
            .and_then(|e| e.firewall.clone())
            .map(|fw| lunchbox_host_api::FirewallSpec {
                default_deny: fw.default_deny,
                allow: fw.allow,
                deny: fw.deny,
            });
    let browser = entry
        .and_then(|e| e.browser.clone())
        .map(|b| lunchbox_host_api::BrowserSpec {
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
    // The cache the activity writes to is the one lunchboxd prefetches
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
