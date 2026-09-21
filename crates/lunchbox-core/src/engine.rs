//! Core policy engine

use chrono::{DateTime, Local, NaiveDate};
use lunchbox_api::{
    API_VERSION, DiagnosticSet, EntryKindTag, EntryView, GroupView, InputDeviceType,
    InternetStatusView, ReasonCode, ServiceStateSnapshot, SessionEndReason, TokenStatus,
    WarningSeverity,
};
use lunchbox_config::{Entry, Group, InternetCheckTarget, Policy, TokensPolicy};
use lunchbox_host_api::{HostCapabilities, HostSessionHandle};
use lunchbox_store::{
    AuditEvent, AuditEventType, SNAPSHOT_FORMAT, SessionSnapshot, StateSnapshot, Store, TokenState,
};
use lunchbox_util::{EntryId, LimitSubject, MonotonicInstant, SessionId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::{ActiveSession, CoreEvent, RestartRequest, SessionPlan, StopResult};

/// Launch decision from the core engine
#[derive(Debug)]
pub enum LaunchDecision {
    Approved(SessionPlan),
    Denied { reasons: Vec<ReasonCode> },
}

/// Outcome of asking the core engine to begin a stop.
///
/// Teardown is two-phase — `begin_stop` then `finish_stop` — so the session
/// stays current (and the launcher stays out of the way) for as long as the
/// activity is actually still running. See issue #136.
#[derive(Debug)]
pub enum BeginStopDecision {
    Stopping {
        /// The host handle to act on, if the activity was ever spawned.
        handle: Option<HostSessionHandle>,
        /// True when a stop was already in flight and this call changed
        /// nothing — e.g. the close button pressed twice.
        already_stopping: bool,
    },
    NoActiveSession,
}

/// How often a running session is checkpointed to the store (issue #201).
///
/// This is the whole tamper budget, and it is a straight trade. Usage is only
/// settled when a session ends, so before this existed a child could hold the
/// power button and have the entire session refunded; now they can have at most
/// this much. Shortening it buys less free play per power cycle and costs an
/// fsync (and, on a device, a round trip to the state custodian) more often;
/// lengthening it does the reverse.
///
/// Thirty seconds is chosen against what the bypass actually costs to perform:
/// a power cycle is tens of seconds of boot before the child can play again, so
/// at this interval the attack does not come out ahead of simply waiting. It is
/// deliberately far longer than the 100 ms tick that carries it — this is not a
/// clock, it is a bound on loss.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(30);

/// A session the previous run of the daemon never got to settle, recovered from
/// the store's snapshot at startup (issue #201).
#[derive(Debug, Clone)]
pub struct RecoveredSession {
    pub session_id: SessionId,
    pub entry_id: EntryId,
    /// When the session started, and so the day its time was billed to.
    pub started_at: DateTime<Local>,
    /// The last moment the daemon is known to have been alive.
    pub last_seen: DateTime<Local>,
    /// What was charged: the last checkpoint's billable duration.
    pub billed: Duration,
}

/// Why a manual token adjustment couldn't be applied (issue #8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenAdjustError {
    /// No entry or group with that ID.
    UnknownSubject,
    /// The subject exists but has no `[tokens]` gate, so a balance on it would
    /// be written and never read.
    NotGated,
    /// The balance could not be persisted.
    Store(String),
}

/// The core policy engine
/// The screen was asked to lock while the device was not in administrator mode
/// (issue #154). Its own type rather than a bare unit error so the one thing it
/// can mean is written down where the caller reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAdministering;

pub struct CoreEngine {
    policy: Policy,
    store: Arc<dyn Store>,
    capabilities: HostCapabilities,
    current_session: Option<ActiveSession>,
    /// Tracks which entries were enabled on the last tick, to detect availability changes
    last_availability_set: HashSet<EntryId>,
    /// Latest known internet connectivity status per check target
    internet_status: HashMap<InternetCheckTarget, bool>,
    /// Per-activity-kind readiness (issue #76). A kind absent from this map is
    /// treated as ready; a kind mapped to `false` is warming up and its
    /// entries are neither shown nor launchable until it reports ready.
    kind_readiness: HashMap<EntryKindTag, bool>,
    /// Set of physical input device types currently connected (issue #96).
    /// `None` means detection has not reported yet — treated as "everything
    /// present" so a detection failure fails open (input-gated entries stay
    /// visible) rather than hiding activities. Once the host's input monitor
    /// runs its first scan this becomes `Some(set)` and gating reflects the
    /// real hardware.
    connected_inputs: Option<HashSet<InputDeviceType>>,

    /// Administrator-facing conditions currently true of this device (issue
    /// #143). Owned here only because [`Self::get_state`] is, and every one of
    /// the daemon's ~20 snapshot call sites would otherwise have to remember to
    /// patch it in. The engine never raises one: the registry lives in
    /// lunchboxd, which pushes the current set through
    /// [`Self::set_diagnostics`].
    diagnostics: DiagnosticSet,

    /// Whether per-entry firewall enforcement works on this host (issue #143),
    /// pushed in by the daemon's diagnostic sweep.
    ///
    /// `None` means "not probed yet" and **fails open**, exactly as
    /// `connected_inputs` does above: a probe that has not answered must not
    /// blank every firewalled activity during boot. Once it answers, an entry
    /// whose configured firewall cannot be applied stops launching rather than
    /// launching unprotected.
    firewall_enforceable: Option<bool>,
    /// Set while the current activity is being deliberately restarted in place
    /// (the HUD's reset button). Its teardown fires the same host `Exited`
    /// event a crash would, and [`Self::end_current_session`] cannot tell
    /// them apart — so without this the session would end halfway through its
    /// own reset. See [`Self::begin_restart`].
    restarting: bool,

    /// Whether the device is in administrator mode (issue #154).
    ///
    /// Lives here, rather than beside the compositor plumbing it exists to
    /// drive, for two reasons: [`Self::get_state`] has to report it, and it is
    /// genuinely a policy state — while it is set, no entry is launchable, on
    /// the same footing as an active session. Entering it and running an
    /// activity are mutually exclusive in both directions.
    admin_mode: bool,

    /// Whether the screen is locked (issue #154).
    ///
    /// Only ever true inside [`Self::admin_mode`]. Leaving the mode clears it,
    /// so the two cannot disagree — a locked device with no administrator mode
    /// behind it would have no button anywhere that could unlock it.
    locked: bool,

    /// When the running session was last checkpointed to the store (issue
    /// #201), or `None` if it has not been yet.
    ///
    /// Cleared whenever a session starts or ends, which is what makes the
    /// first tick of a new session write a snapshot immediately rather than
    /// [`SNAPSHOT_INTERVAL`] later: a power cut in the first half-minute
    /// should still leave a record that a session was open.
    last_snapshot_at: Option<MonotonicInstant>,
}

impl CoreEngine {
    /// Create a new core engine
    pub fn new(policy: Policy, store: Arc<dyn Store>, capabilities: HostCapabilities) -> Self {
        info!(
            entry_count = policy.entries.len(),
            "Core engine initialized"
        );

        // Log policy load
        let _ = store.append_audit(AuditEvent::new(AuditEventType::PolicyLoaded {
            entry_count: policy.entries.len(),
        }));

        Self {
            policy,
            store,
            capabilities,
            admin_mode: false,
            locked: false,
            current_session: None,
            last_availability_set: HashSet::new(),
            internet_status: HashMap::new(),
            kind_readiness: HashMap::new(),
            connected_inputs: None,
            diagnostics: DiagnosticSet::default(),
            firewall_enforceable: None,
            restarting: false,
            last_snapshot_at: None,
        }
    }

    /// Get current policy
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Reload policy
    pub fn reload_policy(&mut self, policy: Policy) -> CoreEvent {
        let entry_count = policy.entries.len();
        self.policy = policy;

        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::PolicyLoaded {
                entry_count,
            }));

        info!(entry_count, "Policy reloaded");

        CoreEvent::PolicyReloaded { entry_count }
    }

    /// Update internet connectivity status for a check target.
    pub fn set_internet_status(&mut self, target: InternetCheckTarget, available: bool) -> bool {
        let previous = self.internet_status.insert(target, available);
        previous != Some(available)
    }

    fn internet_available(&self, target: &InternetCheckTarget) -> bool {
        self.internet_status.get(target).copied().unwrap_or(false)
    }

    /// Update the readiness of an activity kind (issue #76). Kinds default to
    /// ready; a kind is only gated once it reports `false`. Returns true if the
    /// stored value changed (so callers can broadcast a fresh state snapshot).
    pub fn set_kind_readiness(&mut self, kind: EntryKindTag, ready: bool) -> bool {
        self.kind_readiness.insert(kind, ready) != Some(ready)
    }

    /// Whether entries of this kind may currently be shown or launched. A kind
    /// that has never reported readiness is treated as ready.
    fn kind_ready(&self, kind: EntryKindTag) -> bool {
        self.kind_readiness.get(&kind).copied().unwrap_or(true)
    }

    /// Update the set of currently-connected input device types (issue #96).
    /// Called by the host's input-device monitor on its initial scan and on
    /// every hotplug. Returns true if the stored set changed, so callers can
    /// broadcast a fresh state snapshot.
    pub fn set_connected_inputs(&mut self, connected: HashSet<InputDeviceType>) -> bool {
        if self.connected_inputs.as_ref() == Some(&connected) {
            return false;
        }
        self.connected_inputs = Some(connected);
        true
    }

    /// Replace the administrator-facing diagnostic set (issue #143). Called by
    /// lunchboxd's registry whenever a condition is raised or cleared. Returns
    /// true if the set changed, so callers can broadcast rather than
    /// re-broadcasting an identical snapshot on every probe sweep.
    pub fn set_diagnostics(&mut self, diagnostics: DiagnosticSet) -> bool {
        if self.diagnostics == diagnostics {
            return false;
        }
        self.diagnostics = diagnostics;
        true
    }

    /// Whether the device is in administrator mode (issue #154).
    pub fn admin_mode(&self) -> bool {
        self.admin_mode
    }

    /// Enter administrator mode.
    ///
    /// Refuses while an activity is running. The alternative — tearing the
    /// child's session down to make room — would end their activity from a
    /// button whose label says nothing about doing so, and the caregiver can
    /// stop it themselves in one more tap. `Err` carries the running entry so
    /// the caller can say which.
    ///
    /// Audited before the mode takes effect: admin mode bills no usage, so
    /// these records are the only evidence the device was in use.
    pub fn enter_admin_mode(&mut self) -> Result<CoreEvent, EntryId> {
        if let Some(session) = &self.current_session {
            return Err(session.plan.entry_id.clone());
        }
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::AdminModeEntered));
        self.admin_mode = true;
        info!("Entered administrator mode");
        Ok(CoreEvent::AdminModeChanged { active: true })
    }

    /// Whether the screen is locked (issue #154).
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Lock the screen.
    ///
    /// Refused outside administrator mode: the lock's only exit is a management
    /// RPC, so locking a device that a caregiver is not already administering
    /// would strand a child behind a screen with no way out of it.
    ///
    /// Idempotent — `None` when already locked, so a second press is not an
    /// error and does not re-announce.
    pub fn lock(&mut self, timed_out: bool) -> Result<Option<CoreEvent>, NotAdministering> {
        if !self.admin_mode {
            return Err(NotAdministering);
        }
        if self.locked {
            return Ok(None);
        }
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::ScreenLocked { timed_out }));
        self.locked = true;
        info!(timed_out, "Screen locked");
        Ok(Some(CoreEvent::LockChanged { locked: true }))
    }

    /// Unlock the screen. Idempotent, and never refused: this is the only way
    /// out, so it must work from whichever client reaches the device first.
    pub fn unlock(&mut self) -> Option<CoreEvent> {
        if !self.locked {
            return None;
        }
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::ScreenUnlocked));
        self.locked = false;
        info!("Screen unlocked");
        Some(CoreEvent::LockChanged { locked: false })
    }

    /// Leave administrator mode. Idempotent: leaving a mode that is not set is
    /// not an error, because the exit paths (a button, the phone, the idle
    /// timeout) can race each other and none of them should report a failure
    /// for arriving second.
    pub fn exit_admin_mode(&mut self, timed_out: bool) -> Option<CoreEvent> {
        if !self.admin_mode {
            return None;
        }
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::AdminModeExited {
                timed_out,
            }));
        self.admin_mode = false;
        // A lock outlives nothing: its only exit is an administrator RPC that
        // is reached through the mode, so leaving with the screen still locked
        // would be a device nobody could get back into.
        if self.locked {
            let _ = self
                .store
                .append_audit(AuditEvent::new(AuditEventType::ScreenUnlocked));
            self.locked = false;
            info!("Screen unlocked because administrator mode ended");
        }
        info!(timed_out, "Left administrator mode");
        Some(CoreEvent::AdminModeChanged { active: false })
    }

    /// The current administrator-facing diagnostic set (issue #143).
    ///
    /// A direct accessor rather than reading `get_state().diagnostics`, which
    /// rebuilds every `EntryView` to answer a question about none of them.
    pub fn diagnostics(&self) -> DiagnosticSet {
        self.diagnostics.clone()
    }

    /// Record whether per-entry firewall enforcement works on this host (issue
    /// #143). Returns true if the answer changed, so the caller can broadcast.
    pub fn set_firewall_enforceable(&mut self, enforceable: bool) -> bool {
        if self.firewall_enforceable == Some(enforceable) {
            return false;
        }
        self.firewall_enforceable = Some(enforceable);
        true
    }

    /// Whether this entry configures a protection that cannot currently be
    /// applied, and so must not launch.
    ///
    /// Steam entries are excluded: the adapter cannot firewall a Steam-launched
    /// process whatever the host supports, so blocking one would remove the
    /// activity permanently for a configuration mistake no host change could
    /// fix. Config validation rejects that combination instead.
    fn protection_unavailable(&self, entry: &Entry) -> bool {
        entry.firewall.is_some()
            && !matches!(entry.kind, lunchbox_api::EntryKind::Steam { .. })
            && self.firewall_enforceable == Some(false)
    }

    /// Device types an entry requires that are not currently connected, sorted
    /// and deduplicated. Empty when the entry has no requirement or all of its
    /// required devices are present. Before the first detection report
    /// (`connected_inputs` is `None`) nothing is considered missing, so the
    /// gate fails open.
    fn missing_inputs(&self, entry: &Entry) -> Vec<InputDeviceType> {
        if entry.requires_input.is_empty() {
            return Vec::new();
        }
        let Some(connected) = self.connected_inputs.as_ref() else {
            return Vec::new();
        };
        // `entry.requires_input` is already sorted and deduplicated by config.
        entry
            .requires_input
            .iter()
            .copied()
            .filter(|dev| !connected.contains(dev))
            .collect()
    }

    /// List the configured internet connectivity checks and their latest
    /// known status. Targets that have never been checked are reported as
    /// unavailable. The list is empty when no checks are configured.
    pub fn internet_status_views(&self) -> Vec<InternetStatusView> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut targets: Vec<&InternetCheckTarget> = Vec::new();

        if let Some(target) = self.policy.service.internet.check.as_ref()
            && seen.insert(target.original.as_str())
        {
            targets.push(target);
        }

        for entry in &self.policy.entries {
            if entry.internet.required
                && let Some(target) = entry.internet.check.as_ref()
                && seen.insert(target.original.as_str())
            {
                targets.push(target);
            }
        }

        targets
            .into_iter()
            .map(|target| InternetStatusView {
                target: target.original.clone(),
                available: self.internet_available(target),
            })
            .collect()
    }

    /// List all entries with availability status
    pub fn list_entries(&self, now: DateTime<Local>) -> Vec<EntryView> {
        self.policy
            .entries
            .iter()
            .map(|entry| self.evaluate_entry(entry, now))
            .collect()
    }

    /// List all groups with their shared state (issue #5).
    ///
    /// A group's limits are shared, so management UIs need the *category's*
    /// combined usage and restrictions rather than inferring them from a
    /// member that happens to be blocked.
    pub fn list_groups(&self, now: DateTime<Local>) -> Vec<GroupView> {
        let today = now.date_naive();
        self.policy
            .groups
            .iter()
            .map(|group| {
                let daily_override = self
                    .store
                    .get_daily_override(&group.subject(), today)
                    .ok()
                    .flatten();
                let availability = daily_override.as_ref().and_then(|o| o.availability);
                let quota_delta = daily_override.as_ref().and_then(|o| o.quota_delta_seconds);

                // A force-disable is the whole story; otherwise report the
                // same restrictions its members see.
                let reasons = if availability == Some(false) {
                    vec![ReasonCode::ManuallyDisabled { until: today }]
                } else {
                    self.group_reasons(group, now, availability == Some(true), quota_delta)
                };

                GroupView {
                    group_id: group.id.clone(),
                    label: group.label.clone(),
                    member_ids: self
                        .policy
                        .group_members(&group.id)
                        .map(|e| e.id.clone())
                        .collect(),
                    enabled: reasons.is_empty(),
                    reasons,
                    used_today: self.group_usage(group, today),
                    daily_quota: group
                        .limits
                        .daily_quota
                        .map(|q| apply_quota_delta(q, quota_delta)),
                    max_run_if_started_now: self.group_max_duration(
                        group,
                        now,
                        quota_delta,
                        availability == Some(true),
                    ),
                    tokens: group
                        .tokens
                        .as_ref()
                        .map(|t| self.token_status_of(&group.subject(), t, today)),
                    // Only meaningful while the category is inside a window:
                    // `remaining_in_window` answers None both for a category
                    // that is always open and for one that is shut, and neither
                    // has a closing time to print (issue #207).
                    window_closes_at: group
                        .availability
                        .remaining_in_window(&now)
                        .and_then(|d| chrono::Duration::from_std(d).ok())
                        .map(|d| now + d),
                    earns_tokens: self.policy.earns_tokens(&group.subject()),
                }
            })
            .collect()
    }

    /// The longest session a group's own limits would allow a member right
    /// now, ignoring the member's individual limits. None means the group
    /// imposes no cap.
    ///
    /// `manually_enabled` must match what `compute_max_duration` was told, or
    /// this reports a cap the members are not actually held to — a force-enabled
    /// category would read as "up to 0s per session" while its members run
    /// uncapped.
    fn group_max_duration(
        &self,
        group: &Group,
        now: DateTime<Local>,
        quota_delta: Option<i64>,
        manually_enabled: bool,
    ) -> Option<Duration> {
        let today = now.date_naive();
        let mut max = group.limits.max_run;
        let mut clamp = |limit: Duration| {
            max = Some(match max {
                Some(m) => m.min(limit),
                None => limit,
            });
        };

        // A force-enable lifts the window, quota and token caps but not the
        // per-session `max_run`, exactly as in `compute_max_duration`.
        if manually_enabled {
            return max;
        }

        if let Some(window_remaining) = group.availability.remaining_in_window(&now) {
            clamp(window_remaining);
        }
        if let Some(quota) = group.limits.daily_quota {
            let effective = apply_quota_delta(quota, quota_delta);
            clamp(effective.saturating_sub(self.group_usage(group, today)));
        }
        if let Some(tokens) = &group.tokens {
            clamp(self.token_balance_of(&group.subject(), tokens, today));
        }

        max
    }

    /// Evaluate a single entry for availability
    fn evaluate_entry(&self, entry: &Entry, now: DateTime<Local>) -> EntryView {
        let today = now.date_naive();
        let group = self.policy.group_of(entry);
        let daily_override = self
            .store
            .get_daily_override(&entry.subject(), today)
            .ok()
            .flatten();
        // A group override applies to every member (issue #5).
        let group_override = group.and_then(|g| {
            self.store
                .get_daily_override(&g.subject(), today)
                .ok()
                .flatten()
        });

        // If manually disabled by a parent override — on the entry or on its
        // group — short-circuit all other checks
        if daily_override.as_ref().and_then(|o| o.availability) == Some(false) {
            return self.manually_disabled_view(entry, today, None);
        }
        if group_override.as_ref().and_then(|o| o.availability) == Some(false) {
            return self.manually_disabled_view(entry, today, group);
        }

        // A force-enable on either the entry or its group lifts the entry's own
        // limits: enabling a whole category for the day means its activities
        // are on today, whatever their individual schedules say.
        let manually_enabled = daily_override.as_ref().and_then(|o| o.availability) == Some(true)
            || group_override.as_ref().and_then(|o| o.availability) == Some(true);
        let quota_delta = daily_override.as_ref().and_then(|o| o.quota_delta_seconds);
        let group_quota_delta = group_override.as_ref().and_then(|o| o.quota_delta_seconds);

        let mut reasons = Vec::new();
        let mut enabled = true;

        // Check the group's shared restrictions (issue #5). Each is the group
        // analogue of an entry-level check below and is bypassed by a
        // force-enable wherever its entry-level twin is.
        if let Some(group) = group {
            for reason in self.group_reasons(group, now, manually_enabled, group_quota_delta) {
                enabled = false;
                reasons.push(ReasonCode::GroupRestricted {
                    group: group.id.clone(),
                    label: group.label.clone(),
                    reason: Box::new(reason),
                });
            }
        }

        // Check if explicitly disabled (skipped when an enable-today override is set)
        if !manually_enabled && entry.disabled {
            enabled = false;
            reasons.push(ReasonCode::Disabled {
                reason: entry.disabled_reason.clone(),
            });
        }

        // Check host capabilities
        let kind_tag = entry.kind.tag();
        if !self.capabilities.supports_kind(kind_tag) {
            enabled = false;
            reasons.push(ReasonCode::UnsupportedKind { kind: kind_tag });
        }

        // Check per-kind readiness: a kind still warming up (e.g. Steam
        // finishing its initial load, issue #76) is neither shown nor
        // launchable until the host reports it ready.
        if !self.kind_ready(kind_tag) {
            enabled = false;
            reasons.push(ReasonCode::NotReady { kind: kind_tag });
        }

        // Check availability window (skipped when an enable-today override is set)
        if !manually_enabled && !entry.availability.is_available(&now) {
            enabled = false;
            reasons.push(ReasonCode::OutsideTimeWindow {
                next_window_start: entry.availability.next_start(&now),
            });
        }

        // Check internet requirement
        if entry.internet.required {
            let check = entry.internet.check.as_ref().or(self
                .policy
                .service
                .internet
                .check
                .as_ref());
            let available = check
                .map(|target| self.internet_available(target))
                .unwrap_or(false);

            if !available {
                enabled = false;
                reasons.push(ReasonCode::InternetUnavailable {
                    check: check.map(|target| target.original.clone()),
                });
            }
        }

        // Check required input devices (issue #96): a "learn to type" activity
        // requiring a keyboard is hidden until one is connected. Fails open
        // before the first detection report (see `missing_inputs`).
        let missing = self.missing_inputs(entry);
        if !missing.is_empty() {
            enabled = false;
            reasons.push(ReasonCode::RequiredInputUnavailable { devices: missing });
        }

        // A configured protection that cannot be applied (issue #143). The
        // config promises this activity is firewalled; if we cannot keep that
        // promise we do not run it, rather than running it unprotected and
        // logging about it.
        if self.protection_unavailable(entry) {
            enabled = false;
            reasons.push(ReasonCode::ProtectionUnavailable);
        }

        // Administrator mode owns the screen: nothing launches as an activity
        // until it is left. Checked before the session test because the two are
        // mutually exclusive, so at most one of them ever fires.
        if self.admin_mode {
            enabled = false;
            reasons.push(ReasonCode::AdminMode);
        }

        // Check if another session is active
        if let Some(session) = &self.current_session {
            enabled = false;
            reasons.push(ReasonCode::SessionActive {
                entry_id: session.plan.entry_id.clone(),
                remaining: session.time_remaining(MonotonicInstant::now()),
            });
        }

        // Check cooldown
        if let Ok(Some(until)) = self.store.get_cooldown_until(&entry.subject())
            && until > now
        {
            enabled = false;
            reasons.push(ReasonCode::CooldownActive {
                available_at: until,
            });
        }

        // Check daily quota (adjusted by any parent-set delta).
        // Skipped when an enable-today override is set: a force-enable bypasses
        // the daily limit entirely, just as it bypasses the availability window.
        if !manually_enabled
            && let Some(quota) = entry.limits.daily_quota
            && let Ok(used) = self.store.get_usage(&entry.id, today)
        {
            let effective_quota = apply_quota_delta(quota, quota_delta);
            if used >= effective_quota {
                enabled = false;
                reasons.push(ReasonCode::QuotaExhausted {
                    used,
                    quota: effective_quota,
                });
            }
        }

        // Check the token gate (issue #8): an entry whose time has to be earned
        // on other activities stays unavailable until enough is banked.
        // Skipped when an enable-today override is set, like the window and
        // daily-quota checks above.
        if !manually_enabled && let Some(tokens) = &entry.tokens {
            let state = self.token_state_of(&entry.subject(), tokens, today);
            if !tokens.unlocked(state.balance) {
                enabled = false;
                reasons.push(ReasonCode::TokensInsufficient {
                    balance: state.balance,
                    required: tokens.minimum,
                });
            }
        }

        // Calculate max run if enabled (None when disabled, Some(None) flattened for unlimited)
        let max_run_if_started_now = if enabled {
            self.compute_max_duration(entry, now, quota_delta, group_quota_delta, manually_enabled)
        } else {
            None
        };

        EntryView {
            entry_id: entry.id.clone(),
            label: entry.label.clone(),
            icon_ref: entry.icon_ref.clone(),
            kind_tag,
            enabled,
            group: entry.group.clone(),
            reasons,
            tokens: entry
                .tokens
                .as_ref()
                .map(|t| self.token_status_of(&entry.subject(), t, today)),
            earns_tokens: self.policy.earns_tokens(&entry.subject()),
            max_run_if_started_now,
        }
    }

    /// Compute maximum duration for an entry if started now.
    /// Returns None if the entry has no time limit (unlimited).
    fn compute_max_duration(
        &self,
        entry: &Entry,
        now: DateTime<Local>,
        quota_delta: Option<i64>,
        group_quota_delta: Option<i64>,
        manually_enabled: bool,
    ) -> Option<Duration> {
        let mut max = entry.limits.max_run;

        // Clamp helper: the tightest limit wins.
        fn clamp(max: &mut Option<Duration>, limit: Duration) {
            *max = Some(match *max {
                Some(m) => m.min(limit),
                None => limit,
            });
        }

        // Limit by time window remaining, unless an admin override bypasses the window.
        if !manually_enabled
            && let Some(window_remaining) = entry.availability.remaining_in_window(&now)
        {
            max = Some(match max {
                Some(m) => m.min(window_remaining),
                None => window_remaining,
            });
        }

        // Limit by daily quota remaining (adjusted by override delta), unless an
        // admin override bypasses the daily limit. A bare per-session max_run still
        // applies; only the daily quota cap is lifted.
        if !manually_enabled && let Some(quota) = entry.limits.daily_quota {
            let today = now.date_naive();
            if let Ok(used) = self.store.get_usage(&entry.id, today) {
                let effective_quota = apply_quota_delta(quota, quota_delta);
                let remaining = effective_quota.saturating_sub(used);
                max = Some(match max {
                    Some(m) => m.min(remaining),
                    None => remaining,
                });
            }
        }

        // Limit by banked tokens (issue #8), so a session can never spend more
        // than has been earned. Lifted by an enable-today override, matching the
        // daily quota.
        if !manually_enabled && entry.tokens.is_some() {
            clamp(&mut max, self.token_balance(entry, now.date_naive()));
        }

        // Apply the same four limits again at group level (issue #5), so a
        // member's session is capped by whichever of the two is tighter.
        if let Some(group) = self.policy.group_of(entry) {
            let today = now.date_naive();

            if let Some(group_max_run) = group.limits.max_run {
                clamp(&mut max, group_max_run);
            }

            if !manually_enabled {
                if let Some(window_remaining) = group.availability.remaining_in_window(&now) {
                    clamp(&mut max, window_remaining);
                }

                if let Some(quota) = group.limits.daily_quota {
                    let effective_quota = apply_quota_delta(quota, group_quota_delta);
                    clamp(
                        &mut max,
                        effective_quota.saturating_sub(self.group_usage(group, today)),
                    );
                }

                if let Some(tokens) = &group.tokens {
                    clamp(
                        &mut max,
                        self.token_balance_of(&group.subject(), tokens, today),
                    );
                }
            }
        }

        max
    }

    /// The view for an entry a parent has switched off for the day, either
    /// directly or via its group.
    fn manually_disabled_view(
        &self,
        entry: &Entry,
        today: NaiveDate,
        group: Option<&Group>,
    ) -> EntryView {
        let reason = ReasonCode::ManuallyDisabled { until: today };
        EntryView {
            entry_id: entry.id.clone(),
            label: entry.label.clone(),
            icon_ref: entry.icon_ref.clone(),
            kind_tag: entry.kind.tag(),
            enabled: false,
            group: entry.group.clone(),
            reasons: vec![match group {
                Some(group) => ReasonCode::GroupRestricted {
                    group: group.id.clone(),
                    label: group.label.clone(),
                    reason: Box::new(reason),
                },
                None => reason,
            }],
            // Still reported: a caregiver switching an activity back on wants
            // to know whether there is banked time waiting for it.
            tokens: entry
                .tokens
                .as_ref()
                .map(|t| self.token_status_of(&entry.subject(), t, today)),
            earns_tokens: self.policy.earns_tokens(&entry.subject()),
            max_run_if_started_now: None,
        }
    }

    /// The group-level restrictions currently blocking a member (issue #5),
    /// unwrapped — the caller wraps each in `GroupRestricted`.
    ///
    /// The daily quota here is the *combined* usage of every member, so one
    /// activity can spend the whole category's budget.
    fn group_reasons(
        &self,
        group: &Group,
        now: DateTime<Local>,
        manually_enabled: bool,
        quota_delta: Option<i64>,
    ) -> Vec<ReasonCode> {
        let mut reasons = Vec::new();
        let today = now.date_naive();

        if !manually_enabled && !group.availability.is_available(&now) {
            reasons.push(ReasonCode::OutsideTimeWindow {
                next_window_start: group.availability.next_start(&now),
            });
        }

        // The group cooldown is not bypassed by an override, matching the
        // entry-level cooldown.
        if let Ok(Some(until)) = self.store.get_cooldown_until(&group.subject())
            && until > now
        {
            reasons.push(ReasonCode::CooldownActive {
                available_at: until,
            });
        }

        if !manually_enabled && let Some(quota) = group.limits.daily_quota {
            let used = self.group_usage(group, today);
            let effective_quota = apply_quota_delta(quota, quota_delta);
            if used >= effective_quota {
                reasons.push(ReasonCode::QuotaExhausted {
                    used,
                    quota: effective_quota,
                });
            }
        }

        if !manually_enabled && let Some(tokens) = &group.tokens {
            let state = self.token_state_of(&group.subject(), tokens, today);
            if !tokens.unlocked(state.balance) {
                reasons.push(ReasonCode::TokensInsufficient {
                    balance: state.balance,
                    required: tokens.minimum,
                });
            }
        }

        reasons
    }

    /// Combined usage of every member of a group on `day`.
    fn group_usage(&self, group: &Group, day: NaiveDate) -> Duration {
        let Ok(all) = self.store.get_all_usage_for_date(day) else {
            return Duration::ZERO;
        };
        self.policy
            .group_members(&group.id)
            .filter_map(|member| {
                all.iter()
                    .find(|(id, _)| id == &member.id)
                    .map(|(_, used)| *used)
            })
            .sum()
    }

    /// A subject's banked token state under the given gate.
    ///
    /// A failed read is reported rather than swallowed silently: it degrades to
    /// a zero balance, which locks the gate, and a locked gate with no
    /// explanation is exactly the failure that is impossible to diagnose from
    /// the outside.
    fn token_state_of(
        &self,
        subject: &LimitSubject,
        tokens: &TokensPolicy,
        today: NaiveDate,
    ) -> TokenState {
        match self
            .store
            .get_token_state(subject, today, tokens.carry_over)
        {
            Ok(state) => state,
            Err(e) => {
                warn!(
                    subject = %subject,
                    error = %e,
                    "Failed to read token balance; treating the gate as locked"
                );
                TokenState::default()
            }
        }
    }

    /// A subject's gate as a caregiver-facing view (issue #8).
    fn token_status_of(
        &self,
        subject: &LimitSubject,
        tokens: &TokensPolicy,
        today: NaiveDate,
    ) -> TokenStatus {
        let state = self.token_state_of(subject, tokens, today);
        TokenStatus {
            balance: state.balance,
            minimum: tokens.minimum,
            unlocked: tokens.unlocked(state.balance),
            max_balance: tokens.max_balance,
            carry_over: tokens.carry_over,
        }
    }

    /// Grant or revoke banked time on a gate (issue #8).
    ///
    /// Granted time behaves exactly like earned time: it lands in the same
    /// balance, is capped by `max_balance`, is spent by the gated activity's
    /// sessions, and opens the gate only once the balance reaches
    /// `minimum_seconds`. A caregiver who wants an activity on regardless has
    /// the availability override for that.
    pub fn adjust_tokens(
        &self,
        subject: &LimitSubject,
        delta_seconds: i64,
        now: DateTime<Local>,
    ) -> Result<TokenStatus, TokenAdjustError> {
        let tokens = self
            .policy
            .tokens_of(subject)
            .ok_or_else(|| match subject {
                LimitSubject::Entry(id) if self.policy.get_entry(id).is_some() => {
                    TokenAdjustError::NotGated
                }
                LimitSubject::Group(id) if self.policy.get_group(id).is_some() => {
                    TokenAdjustError::NotGated
                }
                _ => TokenAdjustError::UnknownSubject,
            })?;

        let today = now.date_naive();
        let state = self
            .store
            .adjust_token_balance(subject, today, tokens.carry_over, delta_seconds)
            .map_err(|e| TokenAdjustError::Store(e.to_string()))?;

        // Claw back anything past the ceiling, the same as earning does.
        let mut balance = state.balance;
        if let Some(max_balance) = tokens.max_balance
            && balance > max_balance
        {
            let excess = (balance - max_balance).as_secs() as i64;
            match self
                .store
                .adjust_token_balance(subject, today, tokens.carry_over, -excess)
            {
                Ok(state) => balance = state.balance,
                Err(e) => warn!(subject = %subject, error = %e, "Failed to cap token balance"),
            }
        }

        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::TokensAdjusted {
                subject: subject.clone(),
                delta_seconds,
                balance,
            }));

        Ok(self.token_status_of(subject, tokens, today))
    }

    /// A subject's banked token balance under the given gate.
    fn token_balance_of(
        &self,
        subject: &LimitSubject,
        tokens: &TokensPolicy,
        today: NaiveDate,
    ) -> Duration {
        self.token_state_of(subject, tokens, today).balance
    }

    /// An entry's banked token balance, or zero if it is not token-gated.
    fn token_balance(&self, entry: &Entry, today: NaiveDate) -> Duration {
        let Some(tokens) = &entry.tokens else {
            return Duration::ZERO;
        };
        self.token_balance_of(&entry.subject(), tokens, today)
    }

    /// Whether a force-enable daily override is in effect for a subject.
    fn manually_enabled(&self, subject: &LimitSubject, today: NaiveDate) -> bool {
        self.store
            .get_daily_override(subject, today)
            .ok()
            .flatten()
            .and_then(|o| o.availability)
            == Some(true)
    }

    /// Settle token balances after a session on `ended` of `duration` (issue
    /// #8): every gate fed by that session banks time, and any gate on the
    /// activity itself spends its balance down.
    ///
    /// Both entries and groups can be gated, and a session settles against
    /// whichever apply — an entry that is itself gated *and* sits in a gated
    /// group pays both.
    ///
    /// Two dates, and they differ only for a session that ran across midnight
    /// (issue #170):
    ///
    /// - `billed_day` is the day the session belongs to — the day it started.
    ///   It answers the *ledger* questions: which day's force-enable override
    ///   granted this session, and whether the balance it drew on still exists.
    /// - `today` is the day the live balances belong to. A token balance is a
    ///   single row with one `updated_day` stamp, not a per-day ledger, so the
    ///   store is only ever told about the current day; writing a past date
    ///   would rewind that stamp over a balance a caregiver granted after
    ///   midnight.
    ///
    /// A gate that doesn't carry over is therefore skipped outright when the
    /// two disagree: the balance that session spent from reset at midnight, so
    /// there is nothing left to bill and nothing today that ought to pay for
    /// yesterday's play. Carry-over gates have one continuous balance and are
    /// settled as normal.
    fn settle_tokens(
        &self,
        ended: &Entry,
        duration: Duration,
        billed_day: NaiveDate,
        today: NaiveDate,
    ) {
        // Which subjects this session banks time for: the entry, and the group
        // it belongs to (issue #5).
        let ended_subjects: Vec<LimitSubject> = std::iter::once(ended.subject())
            .chain(ended.group.clone().map(LimitSubject::Group))
            .collect();

        // Whether a caregiver granted this session, on the entry or on its
        // group. `evaluate_entry` treats an override at *either* level as a
        // force-enable that lifts both the gate and the clamp, so the spend
        // exemption below has to be scoped the same way: billing a balance for
        // a session whose cap was lifted can drain it to zero. The override is
        // keyed by date, so it is the *billed* day that has to be asked: the
        // grant that approved a session started at 23:50 expired at midnight.
        let granted = ended_subjects
            .iter()
            .any(|subject| self.manually_enabled(subject, billed_day));

        // Every gate in the policy, on entries and on groups alike.
        let gates = self
            .policy
            .entries
            .iter()
            .filter_map(|e| e.tokens.as_ref().map(|t| (e.subject(), t)))
            .chain(
                self.policy
                    .groups
                    .iter()
                    .filter_map(|g| g.tokens.as_ref().map(|t| (g.subject(), t))),
            );

        for (target, tokens) in gates {
            // The balance this session earned and spent against belonged to
            // `billed_day`, and a gate that doesn't carry over threw it away at
            // midnight. There is nothing left to settle, and settling against
            // today's balance instead is exactly what issue #170 is about.
            if billed_day != today && !tokens.carry_over {
                debug!(
                    subject = %target,
                    billed_day = %billed_day,
                    "Session started on an earlier day; its token balance has since reset"
                );
                continue;
            }

            // Earn: the session was on one of this gate's source activities,
            // either directly or as a member of a source group.
            if tokens.from.iter().any(|src| ended_subjects.contains(src)) {
                self.earn_tokens(&target, tokens, duration, today);
            }

            // Spend: the gate is on the activity that just ran, or on the group
            // it belongs to. A session run under a force-enable override is
            // exempt — the caregiver granted that time, so the child shouldn't
            // be billed for it, consistent with the override bypassing the gate
            // in the first place.
            if ended_subjects.contains(&target)
                && !granted
                && let Err(e) = self.store.adjust_token_balance(
                    &target,
                    today,
                    tokens.carry_over,
                    -(duration.as_secs() as i64),
                )
            {
                warn!(subject = %target, error = %e, "Failed to spend token balance");
            }
        }
    }

    /// Everything that has to settle when a session ends: token balances, and
    /// the cooldowns for the entry and for its group.
    ///
    /// A group cooldown is started by *any* member's session and applies to
    /// every member, so a child can't hop between activities in a category to
    /// dodge it (issue #5).
    ///
    /// Sessions shorter than the subject's `cooldown_min_session` don't start
    /// its cooldown at all — a workaround for unstable activities, which would
    /// otherwise crash on launch and leave the child locked out of something
    /// they never got to play.
    ///
    /// `billed_day` is the day the session's time is charged to — its *start*
    /// day (issue #170), which is not `now.date_naive()` for a session that ran
    /// across midnight. Cooldowns are unaffected either way: they are stored as
    /// `now + delta` timestamps rather than keyed by date.
    fn settle_session_end(
        &self,
        ended_entry_id: &EntryId,
        duration: Duration,
        now: DateTime<Local>,
        billed_day: NaiveDate,
    ) {
        let Some(entry) = self.policy.get_entry(ended_entry_id) else {
            return;
        };

        self.settle_tokens(entry, duration, billed_day, now.date_naive());

        let cooldowns = [
            (
                entry.subject(),
                entry.limits.cooldown,
                entry.limits.cooldown_min_session,
            ),
            match self.policy.group_of(entry) {
                Some(group) => (
                    group.subject(),
                    group.limits.cooldown,
                    group.limits.cooldown_min_session,
                ),
                None => (entry.subject(), None, Duration::ZERO),
            },
        ];
        for (subject, cooldown, min_session) in cooldowns {
            let Some(cooldown) = cooldown else {
                continue;
            };
            // A session too short to count doesn't start the cooldown: an
            // activity that crashes on launch would otherwise lock the child
            // out of it without ever having run.
            if duration < min_session {
                info!(
                    subject = %subject,
                    duration_secs = duration.as_secs(),
                    min_session_secs = min_session.as_secs(),
                    "Session too short to start a cooldown"
                );
                continue;
            }
            if let Ok(delta) = chrono::Duration::from_std(cooldown) {
                let _ = self.store.set_cooldown_until(&subject, now + delta);
            }
        }
    }

    /// Bank time on a gate, applying its `max_balance` ceiling.
    fn earn_tokens(
        &self,
        target: &LimitSubject,
        tokens: &TokensPolicy,
        duration: Duration,
        today: NaiveDate,
    ) {
        let earned = tokens.earned(duration);
        if earned.is_zero() {
            return;
        }

        let balance = match self.store.adjust_token_balance(
            target,
            today,
            tokens.carry_over,
            earned.as_secs() as i64,
        ) {
            Ok(state) => state.balance,
            Err(e) => {
                warn!(subject = %target, error = %e, "Failed to bank earned tokens");
                return;
            }
        };

        // Apply the ceiling here rather than in the store, which has no view of
        // policy.
        if let Some(max_balance) = tokens.max_balance
            && balance > max_balance
        {
            let excess = (balance - max_balance).as_secs() as i64;
            if let Err(e) =
                self.store
                    .adjust_token_balance(target, today, tokens.carry_over, -excess)
            {
                warn!(subject = %target, error = %e, "Failed to cap token balance");
            }
        }
    }

    /// Request to launch an entry
    pub fn request_launch(&self, entry_id: &EntryId, now: DateTime<Local>) -> LaunchDecision {
        // Find entry
        let entry = match self.policy.get_entry(entry_id) {
            Some(e) => e,
            None => {
                return LaunchDecision::Denied {
                    reasons: vec![ReasonCode::Disabled {
                        reason: Some("Entry not found".into()),
                    }],
                };
            }
        };

        // Evaluate availability
        let view = self.evaluate_entry(entry, now);

        if !view.enabled {
            // Log denial
            let _ = self
                .store
                .append_audit(AuditEvent::new(AuditEventType::LaunchDenied {
                    entry_id: entry_id.clone(),
                    reasons: view.reasons.iter().map(|r| format!("{:?}", r)).collect(),
                }));

            return LaunchDecision::Denied {
                reasons: view.reasons,
            };
        }

        // Compute session plan
        let max_duration = view.max_run_if_started_now;
        let plan = SessionPlan {
            session_id: SessionId::new(),
            entry_id: entry_id.clone(),
            label: entry.label.clone(),
            max_duration,
            warnings: entry.warnings.clone(),
            confirm_on_close: entry.confirm_on_close,
            can_reset: entry.kind.supports_reset(),
            can_turn_pages: entry.kind.supports_page_turn(),
            hud_orientation: entry.hud_orientation,
        };

        if let Some(max_dur) = max_duration {
            debug!(
                entry_id = %entry_id,
                max_duration_secs = max_dur.as_secs(),
                "Launch approved"
            );
        } else {
            debug!(
                entry_id = %entry_id,
                "Launch approved (unlimited)"
            );
        }

        LaunchDecision::Approved(plan)
    }

    /// Start a session from an approved plan
    pub fn start_session(
        &mut self,
        plan: SessionPlan,
        now: DateTime<Local>,
        now_mono: MonotonicInstant,
    ) -> CoreEvent {
        let session = ActiveSession::new(plan.clone(), now, now_mono);

        let event = CoreEvent::SessionStarted {
            session_id: session.plan.session_id.clone(),
            entry_id: session.plan.entry_id.clone(),
            label: session.plan.label.clone(),
            deadline: session.deadline,
            confirm_on_close: session.plan.confirm_on_close,
            can_reset: session.plan.can_reset,
            can_turn_pages: session.plan.can_turn_pages,
        };

        // Log to audit
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::SessionStarted {
                session_id: session.plan.session_id.clone(),
                entry_id: session.plan.entry_id.clone(),
                label: session.plan.label.clone(),
                deadline: session.deadline,
            }));

        if let Some(deadline) = session.deadline {
            info!(
                session_id = %session.plan.session_id,
                entry_id = %session.plan.entry_id,
                deadline = %deadline,
                "Session started"
            );
        } else {
            info!(
                session_id = %session.plan.session_id,
                entry_id = %session.plan.entry_id,
                "Session started (unlimited)"
            );
        }

        self.current_session = Some(session);
        // So the next tick checkpoints this session straight away rather than
        // SNAPSHOT_INTERVAL into it (issue #201).
        self.last_snapshot_at = None;

        event
    }

    /// Note that the current activity's first window has appeared.
    ///
    /// Matched by handle payload like every other host event, and latched: only
    /// the *first* window counts, so an activity that opens more later does not
    /// restart its billing clock.
    pub fn notify_window_ready(&mut self, handle: &HostSessionHandle, now_mono: MonotonicInstant) {
        let Some(session) = self.current_session.as_mut() else {
            return;
        };
        if !session.owns_handle(handle) || session.window_ready_at_mono.is_some() {
            return;
        }
        let waited = session.duration_so_far(now_mono);
        session.window_ready_at_mono = Some(now_mono);
        info!(
            session_id = %session.plan.session_id,
            entry_id = %session.plan.entry_id,
            waited_secs = waited.as_secs(),
            "Activity window appeared; billing starts here"
        );
    }

    /// Attach host handle to current session
    pub fn attach_host_handle(&mut self, handle: HostSessionHandle) {
        if let Some(session) = &mut self.current_session {
            session.attach_handle(handle);
        }
    }

    /// Begin restarting the current activity in place.
    ///
    /// Returns what the caller needs to tear the activity down and bring it
    /// back — the entry to relaunch and the handle to stop — or `None` when
    /// there is no session, or the activity doesn't support being reset.
    ///
    /// Everything about the session is preserved: its id, its deadline, its
    /// warnings, and its usage accounting all keep running. Only the process
    /// underneath is replaced. That is the whole point — a reset is not a new
    /// session, so it must not restart the clock or spend a cooldown.
    ///
    /// Until [`Self::finish_restart`] is called, exits are ignored (see
    /// [`Self::restarting`]). **The caller must always call it**, including on
    /// failure, or the session becomes unkillable-by-exit until it expires.
    pub fn begin_restart(&mut self) -> Option<RestartRequest> {
        let session = self.current_session.as_ref()?;
        if !session.plan.can_reset {
            debug!(
                entry_id = %session.plan.entry_id,
                "Reset requested for an activity that does not support it"
            );
            return None;
        }

        let request = RestartRequest {
            session_id: session.plan.session_id.clone(),
            entry_id: session.plan.entry_id.clone(),
            host_handle: session.host_handle.clone(),
        };
        info!(
            session_id = %request.session_id,
            entry_id = %request.entry_id,
            "Restarting activity in place"
        );
        self.restarting = true;
        Some(request)
    }

    /// Finish a restart begun by [`Self::begin_restart`], attaching the handle
    /// of the replacement process.
    ///
    /// `handle` is `None` when the relaunch failed; the session is ended in
    /// that case, since there is no longer a process behind it.
    pub fn finish_restart(
        &mut self,
        handle: Option<HostSessionHandle>,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) -> Option<CoreEvent> {
        self.restarting = false;
        match handle {
            Some(handle) => {
                self.attach_host_handle(handle);
                None
            }
            None => {
                warn!("Relaunch after reset failed; ending the session");
                self.end_current_session(None, now_mono, now)
            }
        }
    }

    /// Whether a restart is in flight.
    pub fn is_restarting(&self) -> bool {
        self.restarting
    }

    /// Write the running session to the store, at most every
    /// [`SNAPSHOT_INTERVAL`] (issue #201).
    ///
    /// Hangs off the tick rather than a timer of its own for the same reason
    /// the custodian's heartbeat does (issue #172): this is the loop that
    /// decides whether a child's time is up, and a checkpoint that kept being
    /// written while *it* had stopped would attest to nothing.
    ///
    /// Failing to write is logged and otherwise ignored. A store that cannot be
    /// reached must not take the session down with it; the cost is that this
    /// particular power cut would be settled from an older checkpoint.
    fn checkpoint_session(&mut self, now: DateTime<Local>, now_mono: MonotonicInstant) {
        let Some(session) = self.current_session.as_ref() else {
            return;
        };
        if let Some(last) = self.last_snapshot_at
            && now_mono.duration_since(last) < SNAPSHOT_INTERVAL
        {
            return;
        }

        let snapshot = StateSnapshot::new(
            now,
            Some(SessionSnapshot {
                session_id: session.plan.session_id.clone(),
                entry_id: session.plan.entry_id.clone(),
                started_at: session.started_at,
                deadline: session.deadline,
                warnings_issued: session.warnings_issued.clone(),
                billable: session.billable_duration(now_mono),
            }),
        );

        if let Err(e) = self.store.save_snapshot(&snapshot) {
            warn!(error = %e, "Failed to checkpoint the running session");
            return;
        }
        self.last_snapshot_at = Some(now_mono);
    }

    /// Drop the session checkpoint, because the session has been settled the
    /// ordinary way and there is nothing left to recover (issue #201).
    ///
    /// Clearing is a *write*, not a delete: the snapshot is one row that is
    /// overwritten in place, and what recovery looks for is an
    /// `active_session`, not a row.
    fn clear_session_snapshot(&mut self, now: DateTime<Local>) {
        self.last_snapshot_at = None;
        let cleared = StateSnapshot::new(now, None);
        if let Err(e) = self.store.save_snapshot(&cleared) {
            // Left behind, the stale checkpoint would be recovered at the next
            // startup and its time charged a second time. Loud, because the
            // child pays for it.
            warn!(error = %e, "Failed to clear the session checkpoint; its usage may be billed twice");
        }
    }

    /// Settle a session the previous run of the daemon was killed in the middle
    /// of (issue #201), and return what was recovered.
    ///
    /// Usage is only written when a session ends, so a daemon that dies with one
    /// running — a held power button, a crash, an OOM kill — used to refund the
    /// whole session. This charges what the last checkpoint saw instead, and
    /// leaves a `SessionEnded { reason: Interrupted }` behind so the event is
    /// visible rather than silent.
    ///
    /// Must run before anything can launch: it settles tokens and cooldowns
    /// too, and a new session starting first would checkpoint over the very
    /// snapshot this reads.
    ///
    /// ## What it charges, and why not more
    ///
    /// The last checkpoint's `billable`, and nothing after it. That is a lower
    /// bound — up to [`SNAPSHOT_INTERVAL`] of real play is not in it — and it
    /// is deliberately the only honest number available. The wall clock could
    /// be asked how long ago the checkpoint was, but the answer is chosen by
    /// whoever decided when to switch the device back on, and it counts the
    /// time the device spent off as play. Charging a bound the child controls
    /// is worse than charging slightly too little.
    ///
    /// One case is charged where a live session would not be: a launch that was
    /// still failing when the power went. `end_current_session` exempts
    /// `LaunchFailed` (issue #135), and a snapshot cannot know that is what it
    /// was about to be. The exposure is bounded by the launch itself and errs
    /// toward charging, which is the right direction for a supervision device.
    pub fn recover_interrupted_session(
        &mut self,
        now: DateTime<Local>,
    ) -> Option<RecoveredSession> {
        let snapshot = match self.store.load_snapshot() {
            Ok(None) => return None,
            Ok(Some(snapshot)) => snapshot,
            Err(e) => {
                // A row this build cannot parse at all: a checkpoint from a
                // future format, or a corrupt one. Same answer as a version
                // mismatch, and it has to be *dropped* rather than left —
                // otherwise it is re-read and re-warned at every startup from
                // now on, and never settles into anything.
                warn!(error = %e, "Session checkpoint could not be read; dropping it unbilled");
                self.clear_session_snapshot(now);
                return None;
            }
        };

        // A checkpoint written by a different build of Lunchbox is not
        // something to guess at. `billable` is a number produced by this
        // version's billing rules, and charging a child for one produced by
        // rules we no longer run is worse than charging nothing: it is wrong in
        // an invisible way. Dropped rather than migrated, deliberately — the
        // cost is one interrupted session's time on the boot after an upgrade,
        // and the alternative is carrying migration code for a row that
        // normally exists for thirty seconds.
        if snapshot.version != SNAPSHOT_FORMAT {
            warn!(
                found = snapshot.version,
                understood = SNAPSHOT_FORMAT,
                "Session checkpoint was written by a different version of Lunchbox; dropping it \
                 unbilled"
            );
            self.clear_session_snapshot(now);
            return None;
        }

        let session = snapshot.active_session?;

        // The day the session started, exactly as a normal end would bill it
        // (issue #170) — a session that began at 23:50 and was cut short at
        // 00:10 is still yesterday's play.
        let billed_day = session.started_at.date_naive();
        if let Err(e) = self
            .store
            .add_usage(&session.entry_id, billed_day, session.billable)
        {
            warn!(entry_id = %session.entry_id, error = %e, "Failed to bill a recovered session");
        }
        self.settle_session_end(&session.entry_id, session.billable, now, billed_day);

        // Stamped when the session was last seen alive, not now. A device that
        // was off overnight would otherwise record the session as ending at
        // breakfast.
        let _ = self.store.append_audit(AuditEvent {
            id: 0,
            timestamp: snapshot.timestamp,
            event: AuditEventType::SessionEnded {
                session_id: session.session_id.clone(),
                entry_id: session.entry_id.clone(),
                reason: SessionEndReason::Interrupted,
                duration: session.billable,
            },
        });

        self.clear_session_snapshot(now);

        warn!(
            session_id = %session.session_id,
            entry_id = %session.entry_id,
            billed_secs = session.billable.as_secs(),
            last_seen = %snapshot.timestamp,
            "Recovered a session the last run never settled; charging what the last checkpoint saw"
        );

        Some(RecoveredSession {
            session_id: session.session_id,
            entry_id: session.entry_id,
            started_at: session.started_at,
            last_seen: snapshot.timestamp,
            billed: session.billable,
        })
    }

    /// Tick the engine - check for warnings, expiry, and availability changes
    pub fn tick(&mut self, now_mono: MonotonicInstant, now: DateTime<Local>) -> Vec<CoreEvent> {
        let mut events = Vec::new();

        // Before the session borrow below, which runs to the end of this
        // function.
        self.checkpoint_session(now, now_mono);

        // Check if the set of available entries has changed
        let current_availability: HashSet<EntryId> = self
            .policy
            .entries
            .iter()
            .filter(|e| self.evaluate_entry(e, now).enabled)
            .map(|e| e.id.clone())
            .collect();

        if current_availability != self.last_availability_set {
            debug!(
                previous = ?self.last_availability_set,
                current = ?current_availability,
                "Entry availability set changed"
            );
            self.last_availability_set = current_availability;
            events.push(CoreEvent::AvailabilitySetChanged);
        }

        let session = match &mut self.current_session {
            Some(s) => s,
            None => return events,
        };

        // Check for pending warnings
        for (threshold, remaining) in session.pending_warnings(now_mono) {
            let severity = session
                .plan
                .warnings
                .iter()
                .find(|w| w.seconds_before == threshold)
                .map(|w| w.severity)
                .unwrap_or(WarningSeverity::Warn);

            let message = session
                .plan
                .warnings
                .iter()
                .find(|w| w.seconds_before == threshold)
                .and_then(|w| w.message_template.clone());

            session.mark_warning_issued(threshold);

            // Log to audit
            let _ = self
                .store
                .append_audit(AuditEvent::new(AuditEventType::WarningIssued {
                    session_id: session.plan.session_id.clone(),
                    threshold_seconds: threshold,
                }));

            info!(
                session_id = %session.plan.session_id,
                threshold_seconds = threshold,
                remaining_secs = remaining.as_secs(),
                "Warning issued"
            );

            events.push(CoreEvent::Warning {
                session_id: session.plan.session_id.clone(),
                threshold_seconds: threshold,
                time_remaining: remaining,
                severity,
                message,
            });
        }

        // Check for expiry
        if session.is_expired(now_mono)
            && session.state != lunchbox_api::SessionState::Expiring
            && session.state != lunchbox_api::SessionState::Ended
        {
            session.mark_expiring();

            info!(
                session_id = %session.plan.session_id,
                "Session expiring"
            );

            events.push(CoreEvent::ExpireDue {
                session_id: session.plan.session_id.clone(),
            });
        }

        events
    }

    /// Reconcile the running session with the wall clock after a resume from
    /// sleep (issue #155).
    ///
    /// Two things go wrong across a suspend, and they pull in opposite
    /// directions:
    ///
    /// 1. **The displayed countdown is wrong.** Enforcement is monotonic and
    ///    correctly excludes the time asleep, but every countdown a human sees
    ///    is derived from the wall-clock `deadline`, which is not. The child's
    ///    HUD therefore loses the sleep time and can sit at 0:00 while the
    ///    session runs on — which reads, from the outside, as "the activity
    ///    never exits". [`ActiveSession::resync_deadline`] corrects it.
    ///
    /// 2. **The session may have outlived its schedule.** `compute_max_duration`
    ///    clamps a session to what is left of its window *at launch*, and
    ///    nothing re-checks it afterwards. Because the monotonic clock stops,
    ///    an N-second sleep moves the real end of the session N seconds past
    ///    the window that bounded it — unbounded for an overnight sleep. When
    ///    the machine wakes outside the activity's allowed hours the session is
    ///    clamped to the entry's save-progress grace and the child is warned,
    ///    rather than being cut off mid-sentence.
    ///
    /// Returns the events to broadcast. Callers should push the resulting state
    /// snapshot *before* these, so the corrected deadline lands first and the
    /// warning is not immediately overwritten by it.
    pub fn notify_resumed(
        &mut self,
        now: DateTime<Local>,
        now_mono: MonotonicInstant,
    ) -> Vec<CoreEvent> {
        let Some(entry_id) = self
            .current_session
            .as_ref()
            .map(|s| s.plan.entry_id.clone())
        else {
            return Vec::new();
        };

        let session = self
            .current_session
            .as_mut()
            .expect("a session is current: its entry id was just read");

        let drift = session.resync_deadline(now, now_mono);
        if !drift.is_zero() {
            info!(
                session_id = %session.plan.session_id,
                slept_secs = drift.as_secs(),
                deadline = ?session.deadline,
                "Resumed; corrected the displayed deadline for time spent asleep"
            );
        }

        // Stopping already, or already inside a grace this resume would only
        // reset. The latch is what stops a lid-switch loop from renewing the
        // grace forever — see `ActiveSession::save_grace_started`.
        if session.stopping.is_some() || session.save_grace_started {
            return Vec::new();
        }

        if !self.outside_allowed_hours(&entry_id, now) {
            return Vec::new();
        }

        let grace = match self.policy.get_entry(&entry_id) {
            Some(entry) => entry.limits.save_grace,
            None => return Vec::new(),
        };

        let session = self
            .current_session
            .as_mut()
            .expect("a session is current: its entry id was just read");
        let Some(remaining) = session.start_save_grace(grace, now, now_mono) else {
            info!(
                session_id = %session.plan.session_id,
                entry_id = %entry_id,
                "Resumed outside allowed hours; session already ends within its save grace"
            );
            return Vec::new();
        };

        let session_id = session.plan.session_id.clone();
        let threshold_seconds = remaining.as_secs();

        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::WarningIssued {
                session_id: session_id.clone(),
                threshold_seconds,
            }));

        info!(
            session_id = %session_id,
            entry_id = %entry_id,
            grace_secs = threshold_seconds,
            "Resumed outside allowed hours; granting save-progress grace"
        );

        vec![CoreEvent::Warning {
            session_id,
            threshold_seconds,
            time_remaining: remaining,
            severity: WarningSeverity::Critical,
            message: Some(format!(
                "Time is up for now. You have {} to save.",
                lunchbox_util::format_duration(remaining)
            )),
        }]
    }

    /// Whether `now` falls outside the hours this entry is allowed to run —
    /// its own window, or the window of the group it belongs to (issue #5).
    ///
    /// Deliberately narrower than [`Self::evaluate_entry`]: only the *schedule*
    /// is consulted, because only the schedule can be violated by the passage
    /// of time alone. A daily quota resets at midnight (so a long sleep can
    /// only leave more of it), and a cooldown is not a reason to stop an
    /// activity that is already running.
    ///
    /// A parent's force-enable for the day lifts the window here exactly as it
    /// does in `evaluate_entry` — a force-enabled activity has no allowed hours
    /// to be outside of. Note that the override is keyed by date, so a sleep
    /// across midnight correctly puts the entry back on its own schedule.
    fn outside_allowed_hours(&self, entry_id: &EntryId, now: DateTime<Local>) -> bool {
        let Some(entry) = self.policy.get_entry(entry_id) else {
            return false;
        };
        let today = now.date_naive();
        let group = self.policy.group_of(entry);

        let enabled_today = |subject: &LimitSubject| {
            self.store
                .get_daily_override(subject, today)
                .ok()
                .flatten()
                .and_then(|o| o.availability)
                == Some(true)
        };
        if enabled_today(&entry.subject()) || group.is_some_and(|g| enabled_today(&g.subject())) {
            return false;
        }

        !entry.availability.is_available(&now)
            || group.is_some_and(|g| !g.availability.is_available(&now))
    }

    /// Notify that the activity behind `handle` has exited.
    ///
    /// Returns `None` — leaving the current session untouched — when the exit
    /// belongs to something else. The process monitor reports exits with a
    /// fabricated session id and identifies the activity only by handle
    /// payload, so without this check a late reap from a *previous* activity
    /// ends whichever session happens to be current (issue #136: RetroArch's
    /// SIGKILL exit arrived 24ms after Bitwig's session started and ended
    /// Bitwig instead, at `duration 0s`).
    pub fn notify_activity_exited(
        &mut self,
        handle: &HostSessionHandle,
        exit_code: Option<i32>,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) -> Option<CoreEvent> {
        match self.current_session.as_ref() {
            Some(session) if session.owns_handle(handle) => {}
            Some(session) => {
                debug!(
                    current_session = %session.plan.session_id,
                    exited = ?handle.payload(),
                    "Ignoring exit for an activity that is not the current session"
                );
                return None;
            }
            None => return None,
        }
        self.end_current_session(exit_code, now_mono, now)
    }

    /// End the current session without a host handle to match against.
    ///
    /// For paths where the engine itself knows the activity is gone — a spawn
    /// that never produced a process, or a stop the host has confirmed.
    ///
    /// Returns `None` while a reset is in flight: both entry points funnel
    /// through here, so the guard belongs here rather than on either one.
    pub fn end_current_session(
        &mut self,
        exit_code: Option<i32>,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) -> Option<CoreEvent> {
        // A reset tears the activity down on purpose. The host reports that
        // exactly like a crash, so ignore it until the replacement is up;
        // `finish_restart` is what decides whether the session survives.
        if self.restarting {
            debug!("Activity exited during a reset; keeping the session");
            return None;
        }

        let session = self.current_session.take()?;

        // The child is charged for the time they could actually use, not for
        // the spinner in front of it. Reported here too, so the audit log,
        // the usage table and the broadcast all agree on one number.
        let duration = session.billable_duration(now_mono);
        let elapsed = session.duration_so_far(now_mono);
        if duration != elapsed {
            debug!(
                session_id = %session.plan.session_id,
                elapsed_secs = elapsed.as_secs(),
                billed_secs = duration.as_secs(),
                "Not billing the time before the activity's window appeared"
            );
        }
        let reason = if let Some(requested) = session.stopping.clone() {
            requested
        } else if session.state == lunchbox_api::SessionState::Expiring {
            SessionEndReason::Expired
        } else {
            SessionEndReason::ProcessExited { exit_code }
        };

        // A launch that never produced a running activity is not play time.
        // On `copernicus` two Stray launches timed out without the game ever
        // starting and were still billed 60s each (issue #135); the child paid
        // 120s of their budget for a spinner. The audit record is still
        // written, so the attempt is visible.
        let billable = !matches!(reason, SessionEndReason::LaunchFailed { .. });

        // Charge the session to the day it *started*, not the day it happened
        // to end (issue #170). A session from 23:50 to 00:10 is yesterday's
        // play: billing all twenty minutes to `now` spends a quota the child
        // has not touched yet, so a session run right up to bedtime eats into
        // the next morning. Splitting a session across the two days it spans
        // is explicitly out of scope; the whole session lands on its start day.
        let billed_day = session.started_at.date_naive();
        if billable {
            let _ = self
                .store
                .add_usage(&session.plan.entry_id, billed_day, duration);

            // Settle token balances (issue #8) and cooldowns, on the entry and
            // on its group (issue #5)
            self.settle_session_end(&session.plan.entry_id, duration, now, billed_day);
        } else {
            info!(
                session_id = %session.plan.session_id,
                entry_id = %session.plan.entry_id,
                duration_secs = duration.as_secs(),
                "Launch never produced an activity; not charging usage or tokens"
            );
        }

        // Log to audit
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::SessionEnded {
                session_id: session.plan.session_id.clone(),
                entry_id: session.plan.entry_id.clone(),
                reason: reason.clone(),
                duration,
            }));

        info!(
            session_id = %session.plan.session_id,
            entry_id = %session.plan.entry_id,
            duration_secs = duration.as_secs(),
            reason = ?reason,
            "Session ended"
        );

        // This session is settled, so its checkpoint must not outlive it —
        // startup would otherwise recover it and bill the time again (issue
        // #201). After the usage write above, so a crash *between* the two
        // leaves a checkpoint to recover from rather than nothing.
        self.clear_session_snapshot(now);

        Some(CoreEvent::SessionEnded {
            session_id: session.plan.session_id,
            entry_id: session.plan.entry_id,
            reason,
            duration,
        })
    }

    /// End the current session because its launch never produced a running
    /// activity. Skips usage and token settlement (see
    /// [`Self::end_current_session`]).
    ///
    /// `handle` is matched against the session exactly as
    /// [`Self::notify_activity_exited`] does, so a stale failure cannot end a
    /// session that has since moved on. Pass `None` when the caller *is* the
    /// launch path and no handle exists yet.
    pub fn notify_launch_failed(
        &mut self,
        handle: Option<&HostSessionHandle>,
        error: String,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) -> Option<CoreEvent> {
        match (self.current_session.as_mut(), handle) {
            (None, _) => return None,
            (Some(session), Some(h)) if !session.owns_handle(h) => {
                debug!(
                    current_session = %session.plan.session_id,
                    failed = ?h.payload(),
                    "Ignoring launch failure for an activity that is not the current session"
                );
                return None;
            }
            (Some(session), _) => {
                session.stopping = Some(SessionEndReason::LaunchFailed { error });
            }
        }
        self.end_current_session(None, now_mono, now)
    }

    /// Begin tearing the current session down.
    ///
    /// Marks the session stopping and hands back the host handle to act on, but
    /// deliberately **keeps it current**: the activity is still on screen until
    /// the host says otherwise, so the launcher must stay out of the way and no
    /// other entry may launch. Settle and clear with [`Self::finish_stop`] once
    /// teardown is confirmed.
    ///
    /// Idempotent: asking twice — a child pressing close again because nothing
    /// visibly happened — re-reports the same in-flight stop rather than
    /// double-settling usage.
    pub fn begin_stop(&mut self, reason: SessionEndReason) -> BeginStopDecision {
        let session = match self.current_session.as_mut() {
            Some(s) => s,
            None => return BeginStopDecision::NoActiveSession,
        };

        let already_stopping = session.stopping.is_some();
        if !already_stopping {
            session.stopping = Some(reason.clone());
            info!(
                session_id = %session.plan.session_id,
                reason = ?reason,
                "Session stopping; held current until teardown confirms"
            );
        }

        BeginStopDecision::Stopping {
            handle: session.host_handle.clone(),
            already_stopping,
        }
    }

    /// Settle and clear a session previously marked by [`Self::begin_stop`].
    ///
    /// Returns `None` if the session already went away on its own — its exit
    /// event can land while the host is still being asked to stop it.
    pub fn finish_stop(
        &mut self,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) -> Option<StopResult> {
        let session = self.current_session.as_ref()?;
        session.stopping.as_ref()?;

        let event = self.end_current_session(None, now_mono, now)?;
        match event {
            CoreEvent::SessionEnded {
                session_id,
                entry_id,
                reason,
                duration,
            } => Some(StopResult {
                session_id,
                entry_id,
                reason,
                duration,
            }),
            _ => None,
        }
    }

    /// Get current service state snapshot
    pub fn get_state(&self) -> ServiceStateSnapshot {
        let current_session = self
            .current_session
            .as_ref()
            .map(|s| s.to_session_info(MonotonicInstant::now()));

        // Build entry views for the snapshot
        let now = lunchbox_util::now();
        let entries = self.list_entries(now);
        // Evaluated at the same instant as the entries: the launcher draws the
        // two together, so a category whose window closed between the two
        // calls would contradict its own members (issue #207).
        let groups = self.list_groups(now);

        ServiceStateSnapshot {
            api_version: API_VERSION,
            policy_loaded: true,
            current_session,
            entry_count: self.policy.entries.len(),
            entries,
            groups,
            internet_status: self.internet_status_views(),
            diagnostics: self.diagnostics.clone(),
            admin_mode: self.admin_mode,
            locked: self.locked,
        }
    }

    /// The audit/usage store, for callers that need to record something the
    /// engine itself does not model (e.g. lunchboxd auditing an escaped
    /// activity reported by the host).
    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    /// Get current session reference
    pub fn current_session(&self) -> Option<&ActiveSession> {
        self.current_session.as_ref()
    }

    /// Get mutable current session reference
    pub fn current_session_mut(&mut self) -> Option<&mut ActiveSession> {
        self.current_session.as_mut()
    }

    /// Check if a session is active
    pub fn has_active_session(&self) -> bool {
        self.current_session.is_some()
    }

    /// Extend current session (admin action)
    /// Only works for sessions with a deadline (not unlimited sessions).
    pub fn extend_current(
        &mut self,
        by: Duration,
        now_mono: MonotonicInstant,
        _now: DateTime<Local>,
    ) -> Option<DateTime<Local>> {
        let session = self.current_session.as_mut()?;

        // Can't extend unlimited sessions - they don't have a deadline
        let deadline_mono = session.deadline_mono?;
        let deadline = session.deadline?;

        let new_deadline_mono = deadline_mono + by;
        let new_deadline = deadline + chrono::Duration::from_std(by).unwrap();

        session.deadline_mono = Some(new_deadline_mono);
        session.deadline = Some(new_deadline);

        // Re-arm any previously-issued warnings whose threshold is now in the
        // future relative to the new deadline. They will fire again as the new
        // remaining time drops back below the threshold.
        let new_remaining = new_deadline_mono.saturating_duration_until(now_mono);
        session
            .warnings_issued
            .retain(|&threshold| new_remaining <= Duration::from_secs(threshold));

        // Log to audit
        let _ = self
            .store
            .append_audit(AuditEvent::new(AuditEventType::SessionExtended {
                session_id: session.plan.session_id.clone(),
                extended_by: by,
                new_deadline,
            }));

        info!(
            session_id = %session.plan.session_id,
            extended_by_secs = by.as_secs(),
            new_deadline = %new_deadline,
            "Session extended"
        );

        Some(new_deadline)
    }

    /// Reduce current session time (admin action).
    /// Clamps the new deadline to at least 5 seconds from now to avoid
    /// immediately expiring the session.
    pub fn reduce_current(
        &mut self,
        by: Duration,
        now_mono: MonotonicInstant,
        _now: DateTime<Local>,
    ) -> Option<DateTime<Local>> {
        let session = self.current_session.as_mut()?;

        let deadline_mono = session.deadline_mono?;
        let deadline = session.deadline?;

        // Remaining time until deadline (zero if already expired)
        let remaining = deadline_mono.saturating_duration_until(now_mono);
        let min_remaining = Duration::from_secs(5);
        let new_remaining = remaining.saturating_sub(by).max(min_remaining);
        let actual_reduction = remaining.saturating_sub(new_remaining);
        let new_deadline_mono = now_mono + new_remaining;
        let new_deadline =
            deadline - chrono::Duration::from_std(actual_reduction).unwrap_or_default();

        session.deadline_mono = Some(new_deadline_mono);
        session.deadline = Some(new_deadline);

        info!(
            session_id = %session.plan.session_id,
            reduced_by_secs = actual_reduction.as_secs(),
            new_deadline = %new_deadline,
            "Session time reduced"
        );

        Some(new_deadline)
    }
}

/// Apply a signed quota delta to a base duration, clamping to zero from below.
fn apply_quota_delta(quota: Duration, delta: Option<i64>) -> Duration {
    match delta {
        None | Some(0) => quota,
        Some(d) if d > 0 => quota + Duration::from_secs(d as u64),
        Some(d) => quota.saturating_sub(Duration::from_secs((-d) as u64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_api::EntryKind;
    use lunchbox_config::{AvailabilityPolicy, Entry, Group, LimitsPolicy, TokensPolicy};
    use lunchbox_store::SqliteStore;
    use lunchbox_util::GroupId;
    use std::collections::HashMap;

    fn make_test_policy() -> Policy {
        Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![Entry {
                id: EntryId::new("test-game"),
                label: "Test Game".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "game".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy {
                    windows: vec![],
                    always: true,
                },
                limits: LimitsPolicy {
                    max_run: Some(Duration::from_secs(300)),
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![],
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        }
    }

    #[test]
    fn test_list_entries() {
        let policy = make_test_policy();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let engine = CoreEngine::new(policy, store, caps);

        let entries = engine.list_entries(lunchbox_util::now());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].enabled);
    }

    /// Administrator mode and a running activity are mutually exclusive, and
    /// this is the half that keeps a child's session from being ended by a
    /// button whose label says nothing about doing so (issue #154).
    #[test]
    fn administrator_mode_is_refused_while_an_activity_is_running() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_test_policy(), store, HostCapabilities::minimal());
        let now = lunchbox_util::now();
        let now_mono = MonotonicInstant::now();

        let plan = match engine.request_launch(&EntryId::new("test-game"), now) {
            LaunchDecision::Approved(plan) => plan,
            other => panic!("expected the launch to be approved, got {other:?}"),
        };
        engine.start_session(plan, now, now_mono);

        assert_eq!(
            engine.enter_admin_mode().unwrap_err(),
            EntryId::new("test-game"),
            "the refusal names the activity, so the caller can say which to stop"
        );
        assert!(!engine.admin_mode(), "a refused entry must not half-apply");
    }

    /// The other half of the exclusion: nothing launches while the mode is on.
    /// Done by disabling every entry with a reason rather than by a special
    /// case in the launch path, so the child's grid greys itself out and says
    /// why without the launcher knowing the mode exists.
    #[test]
    fn administrator_mode_makes_every_entry_unavailable_and_gives_it_back() {
        use lunchbox_api::ReasonCode;

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_test_policy(), store, HostCapabilities::minimal());
        let now = lunchbox_util::now();

        assert!(engine.list_entries(now)[0].enabled);

        engine.enter_admin_mode().expect("nothing is running");
        assert!(engine.admin_mode());
        assert!(
            engine.get_state().admin_mode,
            "shells read the mode from here"
        );

        let view = &engine.list_entries(now)[0];
        assert!(!view.enabled);
        assert!(
            view.reasons.contains(&ReasonCode::AdminMode),
            "the child is told a grown-up is setting things up, not just \"unavailable\""
        );
        assert!(
            matches!(
                engine.request_launch(&EntryId::new("test-game"), now),
                LaunchDecision::Denied { .. }
            ),
            "the same gate must stop a launch that did not come from the grid"
        );

        engine.exit_admin_mode(false).expect("the mode was on");
        assert!(engine.list_entries(now)[0].enabled, "and it all comes back");
    }

    /// Three things can leave the mode — the HUD, the phone, and the idle
    /// timeout — and they can race. Arriving second is not a failure.
    #[test]
    fn leaving_administrator_mode_twice_is_not_an_error() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_test_policy(), store, HostCapabilities::minimal());

        assert!(engine.exit_admin_mode(false).is_none(), "was never in it");
        engine.enter_admin_mode().unwrap();
        assert!(
            engine.exit_admin_mode(false).is_some(),
            "the transition happened"
        );
        assert!(
            engine.exit_admin_mode(true).is_none(),
            "and does not happen twice"
        );
    }

    /// A firewalled entry on a host where enforcement is unavailable must not
    /// launch — the config promises the activity is filtered, and running it
    /// unfiltered would break that promise silently (issue #143).
    #[test]
    fn an_unenforceable_firewall_gates_the_entry_it_was_configured_on() {
        use lunchbox_api::ReasonCode;
        use lunchbox_config::FirewallPolicy;

        let mut policy = make_test_policy();
        policy.entries[0].firewall = Some(FirewallPolicy {
            default_deny: true,
            allow: vec![],
            deny: vec![],
        });
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let entry_id = EntryId::new("test-game");
        let now = lunchbox_util::now();

        // Before the probe answers the gate fails open, so a device still
        // booting does not blank every firewalled tile.
        assert!(
            engine.list_entries(now)[0].enabled,
            "an unprobed host must not hide activities"
        );

        assert!(engine.set_firewall_enforceable(false));
        let entries = engine.list_entries(now);
        assert!(!entries[0].enabled);
        assert!(
            entries[0]
                .reasons
                .iter()
                .any(|r| matches!(r, ReasonCode::ProtectionUnavailable)),
            "expected ProtectionUnavailable, got: {:?}",
            entries[0].reasons
        );
        assert!(matches!(
            engine.request_launch(&entry_id, now),
            LaunchDecision::Denied { .. }
        ));

        // Fixing the host un-gates it without a restart, which is the whole
        // point of the probe being refreshable.
        assert!(engine.set_firewall_enforceable(true));
        assert!(engine.list_entries(now)[0].enabled);
        assert!(!engine.set_firewall_enforceable(true), "no spurious change");
    }

    /// An entry with no firewall configured is unaffected by a host that
    /// cannot enforce one.
    #[test]
    fn an_unfirewalled_entry_is_untouched_by_an_unenforceable_host() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_test_policy(), store, HostCapabilities::minimal());
        engine.set_firewall_enforceable(false);
        assert!(engine.list_entries(lunchbox_util::now())[0].enabled);
    }

    /// A firewall on a Steam entry is ignored whatever the host supports, so
    /// gating it would remove the activity permanently for a config mistake no
    /// host change could fix. Config validation rejects that combination
    /// instead.
    #[test]
    fn a_steam_entry_is_not_gated_by_firewall_enforceability() {
        use lunchbox_config::FirewallPolicy;

        let mut policy = make_test_policy();
        policy.entries[0].kind = EntryKind::Steam {
            app_id: 504230,
            args: vec![],
            env: HashMap::new(),
        };
        policy.entries[0].firewall = Some(FirewallPolicy {
            default_deny: true,
            allow: vec![],
            deny: vec![],
        });
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        engine.set_firewall_enforceable(false);

        // Asserted on the reason rather than on `enabled`: a minimal host does
        // not support the Steam kind at all, so the entry is gated for that
        // reason regardless. What matters here is that the firewall gate is not
        // one of the reasons.
        assert!(
            !engine.list_entries(lunchbox_util::now())[0]
                .reasons
                .iter()
                .any(|r| matches!(r, lunchbox_api::ReasonCode::ProtectionUnavailable)),
            "a Steam entry must not be gated by firewall enforceability"
        );
    }

    #[test]
    fn test_kind_readiness_gates_show_and_launch() {
        use lunchbox_api::{EntryKindTag, ReasonCode};

        let policy = make_test_policy();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test-game");
        let now = lunchbox_util::now();

        // Kinds default to ready: the entry is enabled and launchable.
        assert!(engine.list_entries(now)[0].enabled);
        assert!(matches!(
            engine.request_launch(&entry_id, now),
            LaunchDecision::Approved(_)
        ));

        // Mark the entry's kind not ready: it should be hidden (disabled with a
        // NotReady reason) and no longer launchable.
        assert!(engine.set_kind_readiness(EntryKindTag::Process, false));
        let entries = engine.list_entries(now);
        assert!(!entries[0].enabled, "should be gated while not ready");
        assert!(
            entries[0].reasons.iter().any(|r| matches!(
                r,
                ReasonCode::NotReady {
                    kind: EntryKindTag::Process
                }
            )),
            "expected NotReady reason, got: {:?}",
            entries[0].reasons
        );
        assert!(matches!(
            engine.request_launch(&entry_id, now),
            LaunchDecision::Denied { .. }
        ));

        // Setting the same value again reports no change; flipping back to
        // ready un-gates the entry.
        assert!(!engine.set_kind_readiness(EntryKindTag::Process, false));
        assert!(engine.set_kind_readiness(EntryKindTag::Process, true));
        assert!(engine.list_entries(now)[0].enabled);
        assert!(engine.list_entries(now)[0].reasons.is_empty());
    }

    #[test]
    fn test_required_input_gates_show_and_launch() {
        use lunchbox_api::{InputDeviceType, ReasonCode};

        let mut policy = make_test_policy();
        policy.entries[0].requires_input = vec![InputDeviceType::Keyboard];
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test-game");
        let now = lunchbox_util::now();

        // Before any detection report the gate fails open: the entry is shown
        // and launchable so a detection failure never hides activities.
        assert!(engine.list_entries(now)[0].enabled);
        assert!(matches!(
            engine.request_launch(&entry_id, now),
            LaunchDecision::Approved(_)
        ));

        // Report a scan with no keyboard: the entry is gated with a
        // RequiredInputUnavailable reason naming the missing device.
        assert!(engine.set_connected_inputs(HashSet::from([InputDeviceType::Mouse])));
        let entries = engine.list_entries(now);
        assert!(!entries[0].enabled, "should be gated without a keyboard");
        assert!(
            entries[0].reasons.iter().any(|r| matches!(
                r,
                ReasonCode::RequiredInputUnavailable { devices }
                    if devices == &[InputDeviceType::Keyboard]
            )),
            "expected RequiredInputUnavailable reason, got: {:?}",
            entries[0].reasons
        );
        assert!(matches!(
            engine.request_launch(&entry_id, now),
            LaunchDecision::Denied { .. }
        ));

        // Reporting the same set again is a no-op; plugging in a keyboard
        // un-gates the entry.
        assert!(!engine.set_connected_inputs(HashSet::from([InputDeviceType::Mouse])));
        assert!(engine.set_connected_inputs(HashSet::from([
            InputDeviceType::Mouse,
            InputDeviceType::Keyboard,
        ])));
        assert!(engine.list_entries(now)[0].enabled);
        assert!(engine.list_entries(now)[0].reasons.is_empty());
    }

    #[test]
    fn test_launch_approval() {
        let policy = make_test_policy();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test-game");
        let decision = engine.request_launch(&entry_id, lunchbox_util::now());

        assert!(matches!(decision, LaunchDecision::Approved(_)));
    }

    #[test]
    fn test_session_blocks_new_launch() {
        let policy = make_test_policy();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test-game");
        let now = lunchbox_util::now();
        let now_mono = MonotonicInstant::now();

        // Launch first session
        if let LaunchDecision::Approved(plan) = engine.request_launch(&entry_id, now) {
            engine.start_session(plan, now, now_mono);
        }

        // Try to launch again - should be denied
        let decision = engine.request_launch(&entry_id, now);
        assert!(matches!(decision, LaunchDecision::Denied { .. }));
    }

    #[test]
    fn test_tick_warnings() {
        let policy = Policy {
            groups: vec![],
            entries: vec![Entry {
                id: EntryId::new("test"),
                label: "Test".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "test".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy {
                    windows: vec![],
                    always: true,
                },
                limits: LimitsPolicy {
                    max_run: Some(Duration::from_secs(120)), // 2 minutes
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![lunchbox_api::WarningThreshold {
                    seconds_before: 60,
                    severity: WarningSeverity::Warn,
                    message_template: Some("1 minute left".into()),
                }],
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            service: Default::default(),
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test");
        let now = lunchbox_util::now();
        let now_mono = MonotonicInstant::now();

        // Start session
        if let LaunchDecision::Approved(plan) = engine.request_launch(&entry_id, now) {
            engine.start_session(plan, now, now_mono);
        }

        // No warnings initially (first tick may emit AvailabilitySetChanged)
        let events = engine.tick(now_mono, now);
        // Filter to just warning events for this test
        let warning_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, CoreEvent::Warning { .. }))
            .collect();
        assert!(warning_events.is_empty());

        // At 70 seconds (10 seconds past warning threshold), warning should fire
        let later = now_mono + Duration::from_secs(70);
        let events = engine.tick(later, now);
        let warning_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, CoreEvent::Warning { .. }))
            .collect();
        assert_eq!(warning_events.len(), 1);
        assert!(matches!(
            warning_events[0],
            CoreEvent::Warning {
                threshold_seconds: 60,
                ..
            }
        ));

        // Warning shouldn't fire twice
        let events = engine.tick(later, now);
        let warning_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, CoreEvent::Warning { .. }))
            .collect();
        assert!(warning_events.is_empty());
    }

    #[test]
    fn test_extend_reschedules_warning() {
        let policy = Policy {
            groups: vec![],
            entries: vec![Entry {
                id: EntryId::new("test"),
                label: "Test".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "test".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy {
                    windows: vec![],
                    always: true,
                },
                limits: LimitsPolicy {
                    max_run: Some(Duration::from_secs(120)),
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![lunchbox_api::WarningThreshold {
                    seconds_before: 60,
                    severity: WarningSeverity::Warn,
                    message_template: None,
                }],
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            service: Default::default(),
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test");
        let now = lunchbox_util::now();
        let now_mono = MonotonicInstant::now();

        if let LaunchDecision::Approved(plan) = engine.request_launch(&entry_id, now) {
            engine.start_session(plan, now, now_mono);
        }

        // At 70s elapsed (50s remaining), 60s warning fires.
        let t1 = now_mono + Duration::from_secs(70);
        let warning_events: Vec<_> = engine
            .tick(t1, now)
            .into_iter()
            .filter(|e| matches!(e, CoreEvent::Warning { .. }))
            .collect();
        assert_eq!(warning_events.len(), 1);

        // Extend by 120s. New deadline is at 240s; remaining is now 170s,
        // which is well above the 60s threshold, so the warning must re-arm.
        engine.extend_current(Duration::from_secs(120), t1, now);

        // 30s after extension (100s elapsed, 140s remaining): still no warning.
        let t2 = t1 + Duration::from_secs(30);
        let warning_events: Vec<_> = engine
            .tick(t2, now)
            .into_iter()
            .filter(|e| matches!(e, CoreEvent::Warning { .. }))
            .collect();
        assert!(warning_events.is_empty());

        // At 190s elapsed (50s remaining against new deadline), warning fires again.
        let t3 = now_mono + Duration::from_secs(190);
        let warning_events: Vec<_> = engine
            .tick(t3, now)
            .into_iter()
            .filter(|e| matches!(e, CoreEvent::Warning { .. }))
            .collect();
        assert_eq!(warning_events.len(), 1);
        assert!(matches!(
            warning_events[0],
            CoreEvent::Warning {
                threshold_seconds: 60,
                ..
            }
        ));

        // And it still doesn't fire twice after re-arming.
        let warning_events: Vec<_> = engine
            .tick(t3, now)
            .into_iter()
            .filter(|e| matches!(e, CoreEvent::Warning { .. }))
            .collect();
        assert!(warning_events.is_empty());
    }

    #[test]
    fn test_session_expiry() {
        let policy = Policy {
            groups: vec![],
            entries: vec![Entry {
                id: EntryId::new("test"),
                label: "Test".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "test".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy {
                    windows: vec![],
                    always: true,
                },
                limits: LimitsPolicy {
                    max_run: Some(Duration::from_secs(60)),
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![],
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            service: Default::default(),
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test");
        let now = lunchbox_util::now();
        let now_mono = MonotonicInstant::now();

        // Start session
        if let LaunchDecision::Approved(plan) = engine.request_launch(&entry_id, now) {
            engine.start_session(plan, now, now_mono);
        }

        // At 61 seconds, should be expired
        let later = now_mono + Duration::from_secs(61);
        let events = engine.tick(later, now);
        // Filter to just expiry events for this test
        let expiry_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, CoreEvent::ExpireDue { .. }))
            .collect();
        assert_eq!(expiry_events.len(), 1);
        assert!(matches!(expiry_events[0], CoreEvent::ExpireDue { .. }));
    }

    #[test]
    fn test_enable_override_bypasses_time_window() {
        use chrono::TimeZone;
        use lunchbox_api::ReasonCode;
        use lunchbox_util::{DaysOfWeek, TimeWindow, WallClock};

        let entry_id = EntryId::new("time-restricted");
        let policy = Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![Entry {
                id: entry_id.clone(),
                label: "Time Restricted".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "game".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy {
                    windows: vec![TimeWindow::new(
                        DaysOfWeek::ALL_DAYS,
                        WallClock::new(23, 55).unwrap(),
                        WallClock::new(23, 59).unwrap(),
                    )],
                    always: false,
                },
                limits: LimitsPolicy {
                    max_run: None,
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![],
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let engine = CoreEngine::new(policy, store.clone(), caps);

        // Use noon as test time — well outside the 23:55–23:59 window
        let noon = chrono::Local
            .with_ymd_and_hms(2026, 4, 27, 12, 0, 0)
            .unwrap();
        let today = noon.date_naive();

        // Without override: disabled due to time window
        let entries = engine.list_entries(noon);
        assert!(!entries[0].enabled, "should be disabled outside window");
        assert!(
            entries[0]
                .reasons
                .iter()
                .any(|r| matches!(r, ReasonCode::OutsideTimeWindow { .. }))
        );
        assert!(matches!(
            engine.request_launch(&entry_id, noon),
            LaunchDecision::Denied { .. }
        ));

        // Set availability=true override for today
        store
            .upsert_daily_override(
                &LimitSubject::Entry(entry_id.clone()),
                today,
                Some(true),
                None,
            )
            .unwrap();

        // With override: should be enabled even outside the window
        let entries = engine.list_entries(noon);
        assert!(entries[0].enabled, "should be enabled with override");
        assert!(
            entries[0].reasons.is_empty(),
            "no reasons when enabled: {:?}",
            entries[0].reasons
        );
        assert!(matches!(
            engine.request_launch(&entry_id, noon),
            LaunchDecision::Approved(_)
        ));
    }

    #[test]
    fn test_disable_override_blocks_during_window() {
        use chrono::TimeZone;
        use lunchbox_api::ReasonCode;
        use lunchbox_util::{DaysOfWeek, TimeWindow, WallClock};

        let entry_id = EntryId::new("time-restricted");
        let policy = Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![Entry {
                id: entry_id.clone(),
                label: "Time Restricted".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "game".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy {
                    windows: vec![TimeWindow::new(
                        DaysOfWeek::ALL_DAYS,
                        WallClock::new(10, 0).unwrap(),
                        WallClock::new(14, 0).unwrap(),
                    )],
                    always: false,
                },
                limits: LimitsPolicy {
                    max_run: None,
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![],
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let engine = CoreEngine::new(policy, store.clone(), caps);

        // Use noon (12:00) — inside the 10:00–14:00 window
        let noon = chrono::Local
            .with_ymd_and_hms(2026, 4, 27, 12, 0, 0)
            .unwrap();
        let today = noon.date_naive();

        // Without override: enabled (inside window)
        let entries = engine.list_entries(noon);
        assert!(entries[0].enabled, "should be enabled inside window");

        // Set availability=false override for today
        store
            .upsert_daily_override(
                &LimitSubject::Entry(entry_id.clone()),
                today,
                Some(false),
                None,
            )
            .unwrap();

        // With override: disabled even though we're inside the window
        let entries = engine.list_entries(noon);
        assert!(!entries[0].enabled, "should be disabled with override");
        assert!(
            entries[0]
                .reasons
                .iter()
                .any(|r| matches!(r, ReasonCode::ManuallyDisabled { .. }))
        );
        assert!(matches!(
            engine.request_launch(&entry_id, noon),
            LaunchDecision::Denied { .. }
        ));
    }

    #[test]
    fn test_enable_override_bypasses_config_disabled() {
        use chrono::TimeZone;
        use lunchbox_api::ReasonCode;

        let entry_id = EntryId::new("config-disabled");
        let policy = Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![Entry {
                id: entry_id.clone(),
                label: "Config Disabled".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "game".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy::default(),
                limits: LimitsPolicy {
                    max_run: None,
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![],
                volume: None,
                brightness: None,
                disabled: true,
                disabled_reason: Some("under review".into()),
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let engine = CoreEngine::new(policy, store.clone(), caps);

        let noon = chrono::Local
            .with_ymd_and_hms(2026, 4, 27, 12, 0, 0)
            .unwrap();
        let today = noon.date_naive();

        // Without override: disabled by config
        let entries = engine.list_entries(noon);
        assert!(!entries[0].enabled, "should be disabled by config");
        assert!(
            entries[0]
                .reasons
                .iter()
                .any(|r| matches!(r, ReasonCode::Disabled { .. }))
        );
        assert!(matches!(
            engine.request_launch(&entry_id, noon),
            LaunchDecision::Denied { .. }
        ));

        // Set availability=true override for today
        store
            .upsert_daily_override(
                &LimitSubject::Entry(entry_id.clone()),
                today,
                Some(true),
                None,
            )
            .unwrap();

        // With override: should be enabled even though disabled by config
        let entries = engine.list_entries(noon);
        assert!(
            entries[0].enabled,
            "should be enabled with override despite config-disabled"
        );
        assert!(
            entries[0].reasons.is_empty(),
            "no reasons when enabled: {:?}",
            entries[0].reasons
        );
        assert!(matches!(
            engine.request_launch(&entry_id, noon),
            LaunchDecision::Approved(_)
        ));
    }

    #[test]
    fn test_enable_override_bypasses_daily_quota() {
        use chrono::TimeZone;
        use lunchbox_api::ReasonCode;

        let entry_id = EntryId::new("quota-limited");
        let policy = Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![Entry {
                id: entry_id.clone(),
                label: "Quota Limited".into(),
                icon_ref: None,
                kind: EntryKind::Process {
                    command: "game".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                },
                availability: AvailabilityPolicy::default(),
                limits: LimitsPolicy {
                    max_run: None,
                    daily_quota: Some(Duration::from_secs(3600)),
                    cooldown: None,
                    cooldown_min_session: Duration::ZERO,
                    save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                },
                warnings: vec![],
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: Default::default(),
                firewall: None,
                browser: None,
                input_compat: vec![],
                input_compat_options: Default::default(),
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: true,
                hud_orientation: None,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let engine = CoreEngine::new(policy, store.clone(), caps);

        let noon = chrono::Local
            .with_ymd_and_hms(2026, 4, 27, 12, 0, 0)
            .unwrap();
        let today = noon.date_naive();

        // Burn the full daily quota.
        store
            .add_usage(&entry_id, today, Duration::from_secs(3600))
            .unwrap();

        // Without override: disabled by exhausted quota.
        let entries = engine.list_entries(noon);
        assert!(!entries[0].enabled, "should be disabled by exhausted quota");
        assert!(
            entries[0]
                .reasons
                .iter()
                .any(|r| matches!(r, ReasonCode::QuotaExhausted { .. }))
        );
        assert!(matches!(
            engine.request_launch(&entry_id, noon),
            LaunchDecision::Denied { .. }
        ));

        // Set availability=true override for today.
        store
            .upsert_daily_override(
                &LimitSubject::Entry(entry_id.clone()),
                today,
                Some(true),
                None,
            )
            .unwrap();

        // With override: enabled despite the exhausted quota, and the daily cap
        // no longer limits the session (unlimited, since max_run is None).
        let entries = engine.list_entries(noon);
        assert!(
            entries[0].enabled,
            "should be enabled with override despite exhausted quota"
        );
        assert!(
            entries[0].reasons.is_empty(),
            "no reasons when enabled: {:?}",
            entries[0].reasons
        );
        assert!(
            entries[0].max_run_if_started_now.is_none(),
            "daily quota should not cap the session when overridden: {:?}",
            entries[0].max_run_if_started_now
        );
        assert!(matches!(
            engine.request_launch(&entry_id, noon),
            LaunchDecision::Approved(_)
        ));
    }

    // --- Token system (issue #8) ------------------------------------------

    fn token_entry(id: &str, tokens: Option<TokensPolicy>) -> Entry {
        Entry {
            id: EntryId::new(id),
            label: id.into(),
            icon_ref: None,
            kind: EntryKind::Process {
                command: "game".into(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            },
            availability: AvailabilityPolicy::default(),
            limits: LimitsPolicy {
                max_run: None,
                daily_quota: None,
                cooldown: None,
                cooldown_min_session: Duration::ZERO,
                save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
            },
            tokens,
            warnings: vec![],
            volume: None,
            brightness: None,
            disabled: false,
            disabled_reason: None,
            internet: Default::default(),
            firewall: None,
            browser: None,
            input_compat: vec![],
            input_compat_options: Default::default(),
            requires_input: vec![],
            group: None,
            xwayland_native_resolution: false,
            confirm_on_close: true,
            hud_orientation: None,
        }
    }

    /// Policy with two source activities ("scratch", "typing") and a
    /// token-gated target ("minecraft").
    fn make_token_policy(tokens: TokensPolicy) -> Policy {
        Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![
                token_entry("scratch", None),
                token_entry("typing", None),
                token_entry("minecraft", Some(tokens)),
            ],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        }
    }

    fn tokens_from(sources: &[&str]) -> TokensPolicy {
        TokensPolicy {
            from: sources.iter().map(|s| LimitSubject::entry(*s)).collect(),
            earn_ratio: 1.0,
            minimum: Duration::ZERO,
            max_balance: None,
            carry_over: false,
        }
    }

    fn view<'a>(entries: &'a [EntryView], id: &str) -> &'a EntryView {
        entries
            .iter()
            .find(|e| e.entry_id.as_str() == id)
            .expect("entry should be listed")
    }

    /// Run a complete session on `entry_id` lasting `duration`, bypassing the
    /// host so the engine's accounting is exercised end to end.
    fn run_session(
        engine: &mut CoreEngine,
        entry_id: &str,
        duration: Duration,
        now: DateTime<Local>,
    ) {
        let entry_id = EntryId::new(entry_id);
        let plan = match engine.request_launch(&entry_id, now) {
            LaunchDecision::Approved(plan) => plan,
            LaunchDecision::Denied { reasons } => {
                panic!("launch of {entry_id} denied: {reasons:?}")
            }
        };
        let started = MonotonicInstant::now();
        engine.start_session(plan, now, started);
        engine.end_current_session(Some(0), started + duration, now);
    }

    fn balance_of(engine: &CoreEngine, id: &str, now: DateTime<Local>) -> Duration {
        let entry = engine
            .policy
            .entries
            .iter()
            .find(|e| e.id.as_str() == id)
            .unwrap();
        engine.token_balance(entry, now.date_naive())
    }

    fn group_balance_of(engine: &CoreEngine, id: &str, now: DateTime<Local>) -> Duration {
        let group = engine
            .policy
            .groups
            .iter()
            .find(|g| g.id.as_str() == id)
            .unwrap();
        let tokens = group.tokens.as_ref().expect("group should be token-gated");
        engine.token_balance_of(&group.subject(), tokens, now.date_naive())
    }

    fn noon() -> DateTime<Local> {
        use chrono::TimeZone;
        chrono::Local
            .with_ymd_and_hms(2026, 4, 27, 12, 0, 0)
            .unwrap()
    }

    #[test]
    fn test_token_gate_locks_until_time_is_earned() {
        use lunchbox_api::ReasonCode;

        let policy = make_token_policy(tokens_from(&["scratch", "typing"]));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // Nothing banked: the target is gated, the sources are not.
        let entries = engine.list_entries(now);
        assert!(view(&entries, "scratch").enabled);
        let minecraft = view(&entries, "minecraft");
        assert!(
            !minecraft.enabled,
            "target should be locked with no balance"
        );
        assert!(
            minecraft.reasons.iter().any(|r| matches!(
                r,
                ReasonCode::TokensInsufficient {
                    balance: Duration::ZERO,
                    ..
                }
            )),
            "expected TokensInsufficient, got: {:?}",
            minecraft.reasons
        );
        assert!(matches!(
            engine.request_launch(&EntryId::new("minecraft"), now),
            LaunchDecision::Denied { .. }
        ));

        // Half an hour of Scratch banks half an hour of Minecraft.
        run_session(&mut engine, "scratch", Duration::from_secs(1800), now);

        let entries = engine.list_entries(now);
        let minecraft = view(&entries, "minecraft");
        assert!(
            minecraft.enabled,
            "target should unlock once time is banked"
        );
        assert!(minecraft.reasons.is_empty());
        assert_eq!(
            minecraft.max_run_if_started_now,
            Some(Duration::from_secs(1800)),
            "session should be capped at the banked balance"
        );
    }

    #[test]
    fn test_token_sources_accumulate_toward_one_gate() {
        let policy = make_token_policy(tokens_from(&["scratch", "typing"]));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "scratch", Duration::from_secs(600), now);
        run_session(&mut engine, "typing", Duration::from_secs(300), now);

        assert_eq!(
            balance_of(&engine, "minecraft", now),
            Duration::from_secs(900),
            "any combination of sources should add up"
        );
    }

    #[test]
    fn test_token_minimum_balance_gates_unlock() {
        let mut tokens = tokens_from(&["scratch"]);
        tokens.minimum = Duration::from_secs(1800);

        let policy = make_token_policy(tokens);
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // Some banked time, but under the minimum: still locked.
        run_session(&mut engine, "scratch", Duration::from_secs(600), now);
        assert!(
            !view(&engine.list_entries(now), "minecraft").enabled,
            "balance below the minimum should not unlock the entry"
        );

        // Crossing the minimum unlocks it.
        run_session(&mut engine, "scratch", Duration::from_secs(1200), now);
        assert!(view(&engine.list_entries(now), "minecraft").enabled);
    }

    #[test]
    fn test_token_earn_ratio_and_balance_ceiling() {
        let mut tokens = tokens_from(&["scratch"]);
        tokens.earn_ratio = 0.5;
        tokens.max_balance = Some(Duration::from_secs(900));

        let policy = make_token_policy(tokens);
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // Half rate: 20 minutes of Scratch banks 10.
        run_session(&mut engine, "scratch", Duration::from_secs(1200), now);
        assert_eq!(
            balance_of(&engine, "minecraft", now),
            Duration::from_secs(600)
        );

        // A long stretch is capped, so a whole Saturday can't bank a week.
        run_session(&mut engine, "scratch", Duration::from_secs(7200), now);
        assert_eq!(
            balance_of(&engine, "minecraft", now),
            Duration::from_secs(900),
            "balance should be capped at max_balance"
        );
    }

    #[test]
    fn test_token_session_spends_balance_and_relocks() {
        let policy = make_token_policy(tokens_from(&["scratch"]));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "scratch", Duration::from_secs(1800), now);

        // Spending part of the balance leaves the rest banked.
        run_session(&mut engine, "minecraft", Duration::from_secs(600), now);
        assert_eq!(
            balance_of(&engine, "minecraft", now),
            Duration::from_secs(1200)
        );
        assert!(view(&engine.list_entries(now), "minecraft").enabled);

        // Spending the rest re-locks it: the time has to be earned again.
        run_session(&mut engine, "minecraft", Duration::from_secs(1200), now);
        assert_eq!(balance_of(&engine, "minecraft", now), Duration::ZERO);
        assert!(
            !view(&engine.list_entries(now), "minecraft").enabled,
            "entry should re-lock once its balance is spent"
        );
    }

    #[test]
    fn test_enable_override_bypasses_token_gate_without_spending() {
        let policy = make_token_policy(tokens_from(&["scratch"]));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();
        let entry_id = EntryId::new("minecraft");

        // Bank a little, then force-enable for the day.
        run_session(&mut engine, "scratch", Duration::from_secs(300), now);
        store
            .upsert_daily_override(
                &LimitSubject::Entry(entry_id.clone()),
                now.date_naive(),
                Some(true),
                None,
            )
            .unwrap();

        let entries = engine.list_entries(now);
        let minecraft = view(&entries, "minecraft");
        assert!(minecraft.enabled);
        assert!(
            minecraft.reasons.is_empty(),
            "no reasons when overridden: {:?}",
            minecraft.reasons
        );
        assert!(
            minecraft.max_run_if_started_now.is_none(),
            "the banked balance should not cap an overridden session: {:?}",
            minecraft.max_run_if_started_now
        );

        // The caregiver granted this time, so it isn't billed to the balance.
        run_session(&mut engine, "minecraft", Duration::from_secs(1800), now);
        assert_eq!(
            balance_of(&engine, "minecraft", now),
            Duration::from_secs(300),
            "an overridden session should not spend banked tokens"
        );
    }

    // --- Grouped time limits (issue #5) -----------------------------------

    fn group(id: &str, limits: LimitsPolicy, tokens: Option<TokensPolicy>) -> Group {
        Group {
            id: GroupId::new(id),
            label: format!("{id} group"),
            availability: AvailabilityPolicy::default(),
            limits,
            tokens,
        }
    }

    fn no_limits() -> LimitsPolicy {
        LimitsPolicy {
            max_run: None,
            daily_quota: None,
            cooldown: None,
            cooldown_min_session: Duration::ZERO,
            save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
        }
    }

    /// Two activities in one group, plus an ungrouped control.
    fn make_group_policy(group: Group) -> Policy {
        let mut member_a = token_entry("game-a", None);
        let mut member_b = token_entry("game-b", None);
        member_a.group = Some(group.id.clone());
        member_b.group = Some(group.id.clone());

        Policy {
            service: Default::default(),
            groups: vec![group],
            entries: vec![member_a, member_b, token_entry("ungrouped", None)],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        }
    }

    fn group_reason(view: &EntryView) -> Option<&ReasonCode> {
        view.reasons.iter().find_map(|r| match r {
            ReasonCode::GroupRestricted { reason, .. } => Some(&**reason),
            _ => None,
        })
    }

    #[test]
    fn test_group_quota_is_shared_across_members() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                daily_quota: Some(Duration::from_secs(1800)),
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // Playing one member spends the *category* budget, so the other
        // member's remaining session shrinks too.
        run_session(&mut engine, "game-a", Duration::from_secs(1200), now);
        let entries = engine.list_entries(now);
        assert_eq!(
            view(&entries, "game-b").max_run_if_started_now,
            Some(Duration::from_secs(600)),
            "a sibling's usage should eat into this member's session"
        );

        // Spending the rest removes every member at once.
        run_session(&mut engine, "game-b", Duration::from_secs(600), now);
        let entries = engine.list_entries(now);
        for id in ["game-a", "game-b"] {
            let v = view(&entries, id);
            assert!(
                !v.enabled,
                "{id} should be gone once the group quota is spent"
            );
            assert!(
                matches!(group_reason(v), Some(ReasonCode::QuotaExhausted { .. })),
                "expected a group QuotaExhausted for {id}, got: {:?}",
                v.reasons
            );
        }
        // An activity outside the group is untouched.
        assert!(view(&entries, "ungrouped").enabled);
    }

    #[test]
    fn test_group_max_run_caps_each_member() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                max_run: Some(Duration::from_secs(900)),
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let engine = CoreEngine::new(policy, store, HostCapabilities::minimal());

        let entries = engine.list_entries(noon());
        assert_eq!(
            view(&entries, "game-a").max_run_if_started_now,
            Some(Duration::from_secs(900)),
            "the group's short-burst cap should apply to a member with no cap of its own"
        );
        assert_eq!(
            view(&entries, "ungrouped").max_run_if_started_now,
            None,
            "an ungrouped entry keeps its own (unlimited) cap"
        );
    }

    #[test]
    fn test_group_cooldown_blocks_a_different_member() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                cooldown: Some(Duration::from_secs(600)),
                cooldown_min_session: Duration::ZERO,
                save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "game-a", Duration::from_secs(300), now);

        // Hopping to a sibling must not dodge the category's cooldown.
        let entries = engine.list_entries(now);
        let sibling = view(&entries, "game-b");
        assert!(
            !sibling.enabled,
            "the cooldown should cover the whole group"
        );
        assert!(matches!(
            group_reason(sibling),
            Some(ReasonCode::CooldownActive { .. })
        ));
        assert!(view(&entries, "ungrouped").enabled);
    }

    // --- Cooldown grace for unstable activities ---------------------------

    /// One entry with a cooldown and a two-minute grace period, plus an
    /// ungrouped control.
    fn make_cooldown_grace_policy(cooldown_min_session: Duration) -> Policy {
        let mut flaky = token_entry("flaky", None);
        flaky.limits = LimitsPolicy {
            cooldown: Some(Duration::from_secs(600)),
            cooldown_min_session,
            ..no_limits()
        };

        Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![flaky, token_entry("ungrouped", None)],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        }
    }

    #[test]
    fn test_short_session_does_not_start_the_cooldown() {
        let policy = make_cooldown_grace_policy(Duration::from_secs(120));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // An activity that crashes seconds after launch shouldn't lock the
        // child out of something they never got to play.
        run_session(&mut engine, "flaky", Duration::from_secs(20), now);
        let entries = engine.list_entries(now);
        assert!(
            view(&entries, "flaky").enabled,
            "a session below the grace period should leave the cooldown alone: {:?}",
            view(&entries, "flaky").reasons
        );

        // Right at the threshold the cooldown starts as usual.
        run_session(&mut engine, "flaky", Duration::from_secs(120), now);
        let entries = engine.list_entries(now);
        let flaky = view(&entries, "flaky");
        assert!(!flaky.enabled, "a full session should start the cooldown");
        assert!(
            flaky
                .reasons
                .iter()
                .any(|r| matches!(r, ReasonCode::CooldownActive { .. })),
            "expected CooldownActive, got: {:?}",
            flaky.reasons
        );
    }

    #[test]
    fn test_cooldown_grace_of_zero_keeps_the_old_behaviour() {
        let policy = make_cooldown_grace_policy(Duration::ZERO);
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "flaky", Duration::from_secs(20), now);
        let entries = engine.list_entries(now);
        assert!(
            !view(&entries, "flaky").enabled,
            "with no grace period even a moment's session starts the cooldown"
        );
    }

    #[test]
    fn test_short_session_does_not_start_the_group_cooldown() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                cooldown: Some(Duration::from_secs(600)),
                cooldown_min_session: Duration::from_secs(120),
                save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "game-a", Duration::from_secs(20), now);
        let entries = engine.list_entries(now);
        for id in ["game-a", "game-b"] {
            assert!(
                view(&entries, id).enabled,
                "a crashed member shouldn't cool down the whole category: {:?}",
                view(&entries, id).reasons
            );
        }
    }

    #[test]
    fn test_group_cooldown_grace_is_independent_of_the_entry_grace() {
        // The member has no grace of its own, the category has two minutes:
        // a crash starts the entry's cooldown but leaves siblings playable.
        let mut policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                cooldown: Some(Duration::from_secs(600)),
                cooldown_min_session: Duration::from_secs(120),
                save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
                ..no_limits()
            },
            None,
        ));
        policy.entries[0].limits = LimitsPolicy {
            cooldown: Some(Duration::from_secs(600)),
            cooldown_min_session: Duration::ZERO,
            save_grace: lunchbox_config::DEFAULT_SAVE_GRACE,
            ..no_limits()
        };
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "game-a", Duration::from_secs(20), now);
        let entries = engine.list_entries(now);
        assert!(
            !view(&entries, "game-a").enabled,
            "the entry's own cooldown has no grace period here"
        );
        assert!(
            view(&entries, "game-b").enabled,
            "the group's grace period should still spare its siblings: {:?}",
            view(&entries, "game-b").reasons
        );
    }

    #[test]
    fn test_group_token_gate_unlocks_every_member() {
        let policy = make_group_policy(group(
            "games",
            no_limits(),
            Some(TokensPolicy {
                from: vec![LimitSubject::entry("ungrouped")],
                earn_ratio: 1.0,
                minimum: Duration::ZERO,
                max_balance: None,
                carry_over: false,
            }),
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // Locked until earned.
        let entries = engine.list_entries(now);
        for id in ["game-a", "game-b"] {
            assert!(!view(&entries, id).enabled, "{id} should start locked");
        }

        // Earning on the source unlocks the whole category at once.
        run_session(&mut engine, "ungrouped", Duration::from_secs(900), now);
        let entries = engine.list_entries(now);
        for id in ["game-a", "game-b"] {
            let v = view(&entries, id);
            assert!(v.enabled, "{id} should unlock with the group");
            assert_eq!(v.max_run_if_started_now, Some(Duration::from_secs(900)));
        }

        // Either member's session spends the shared balance.
        run_session(&mut engine, "game-a", Duration::from_secs(400), now);
        assert_eq!(
            view(&engine.list_entries(now), "game-b").max_run_if_started_now,
            Some(Duration::from_secs(500)),
            "a sibling's play should spend the group's banked time"
        );
    }

    #[test]
    fn test_group_source_banks_time_from_any_member() {
        // A gate fed by a whole category: playing any member earns.
        let mut reward = token_entry(
            "reward",
            Some(TokensPolicy {
                from: vec![LimitSubject::group("games")],
                earn_ratio: 1.0,
                minimum: Duration::ZERO,
                max_balance: None,
                carry_over: false,
            }),
        );
        reward.group = None;

        let mut policy = make_group_policy(group("games", no_limits(), None));
        policy.entries.push(reward);

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        assert!(!view(&engine.list_entries(now), "reward").enabled);

        run_session(&mut engine, "game-b", Duration::from_secs(600), now);
        let entries = engine.list_entries(now);
        let v = view(&entries, "reward");
        assert!(v.enabled, "any member of a source group should bank time");
        assert_eq!(v.max_run_if_started_now, Some(Duration::from_secs(600)));
    }

    #[test]
    fn test_group_override_enables_and_disables_every_member() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                daily_quota: Some(Duration::from_secs(600)),
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();
        let today = now.date_naive();
        let games = LimitSubject::group("games");

        // Spend the category's quota so both members are gone.
        run_session(&mut engine, "game-a", Duration::from_secs(600), now);
        assert!(!view(&engine.list_entries(now), "game-b").enabled);

        // One override re-enables the whole category, uncapped.
        store
            .upsert_daily_override(&games, today, Some(true), None)
            .unwrap();
        let entries = engine.list_entries(now);
        for id in ["game-a", "game-b"] {
            let v = view(&entries, id);
            assert!(v.enabled, "{id} should be enabled by the group override");
            assert!(v.reasons.is_empty(), "no reasons: {:?}", v.reasons);
            assert_eq!(v.max_run_if_started_now, None);
        }

        // And a force-disable switches the whole category off.
        store
            .upsert_daily_override(&games, today, Some(false), None)
            .unwrap();
        let entries = engine.list_entries(now);
        for id in ["game-a", "game-b"] {
            assert!(
                !view(&entries, id).enabled,
                "{id} should be disabled by the group override"
            );
        }
        assert!(view(&entries, "ungrouped").enabled);
    }

    #[test]
    fn test_group_quota_delta_extends_the_category() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                daily_quota: Some(Duration::from_secs(600)),
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "game-a", Duration::from_secs(600), now);
        assert!(!view(&engine.list_entries(now), "game-b").enabled);

        // Granting the category more time brings every member back.
        store
            .upsert_daily_override(
                &LimitSubject::group("games"),
                now.date_naive(),
                None,
                Some(300),
            )
            .unwrap();
        let entries = engine.list_entries(now);
        let v = view(&entries, "game-b");
        assert!(v.enabled);
        assert_eq!(v.max_run_if_started_now, Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_list_groups_reports_shared_state() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                max_run: Some(Duration::from_secs(900)),
                daily_quota: Some(Duration::from_secs(1800)),
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();

        let groups = engine.list_groups(now);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.group_id.as_str(), "games");
        assert_eq!(g.label, "games group");
        assert_eq!(
            g.member_ids.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
            vec!["game-a", "game-b"],
            "members are listed so a UI can show what shares the budget"
        );
        assert!(g.enabled);
        assert_eq!(g.used_today, Duration::ZERO);
        assert_eq!(g.daily_quota, Some(Duration::from_secs(1800)));
        // Capped by max_run, which is tighter than the remaining quota.
        assert_eq!(g.max_run_if_started_now, Some(Duration::from_secs(900)));

        // Usage from any member rolls up into the category's total.
        run_session(&mut engine, "game-a", Duration::from_secs(900), now);
        let g = &engine.list_groups(now)[0];
        assert_eq!(g.used_today, Duration::from_secs(900));
        assert_eq!(g.max_run_if_started_now, Some(Duration::from_secs(900)));

        // Spending the rest reports the category as restricted, with the
        // reason unwrapped rather than buried in GroupRestricted.
        run_session(&mut engine, "game-b", Duration::from_secs(900), now);
        let g = &engine.list_groups(now)[0];
        assert!(!g.enabled);
        assert_eq!(g.used_today, Duration::from_secs(1800));
        assert!(
            g.reasons
                .iter()
                .any(|r| matches!(r, ReasonCode::QuotaExhausted { .. })),
            "expected a bare QuotaExhausted, got: {:?}",
            g.reasons
        );
    }

    #[test]
    fn test_list_groups_reflects_overrides() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                daily_quota: Some(Duration::from_secs(600)),
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();
        let today = now.date_naive();
        let games = LimitSubject::group("games");

        run_session(&mut engine, "game-a", Duration::from_secs(600), now);
        assert!(!engine.list_groups(now)[0].enabled);

        // A quota delta raises the category's effective budget.
        store
            .upsert_daily_override(&games, today, None, Some(300))
            .unwrap();
        let g = &engine.list_groups(now)[0];
        assert!(g.enabled);
        assert_eq!(g.daily_quota, Some(Duration::from_secs(900)));
        assert_eq!(g.max_run_if_started_now, Some(Duration::from_secs(300)));

        // A force-disable is reported as such, not as an exhausted quota.
        store
            .upsert_daily_override(&games, today, Some(false), None)
            .unwrap();
        let g = &engine.list_groups(now)[0];
        assert!(!g.enabled);
        assert!(matches!(
            g.reasons.as_slice(),
            [ReasonCode::ManuallyDisabled { .. }]
        ));
    }

    #[test]
    fn test_entry_view_reports_group_membership() {
        let policy = make_group_policy(group("games", no_limits(), None));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let engine = CoreEngine::new(policy, store, HostCapabilities::minimal());

        let entries = engine.list_entries(noon());
        assert_eq!(
            view(&entries, "game-a").group.as_ref().map(|g| g.as_str()),
            Some("games")
        );
        assert!(view(&entries, "ungrouped").group.is_none());
    }

    /// A force-enable on the *group* lifts a member's own token gate and clamp,
    /// so it must exempt that member's balance from being spent too. Billing it
    /// while the clamp is lifted drains a balance the child never chose to
    /// spend.
    #[test]
    fn test_group_override_exempts_a_members_own_token_balance() {
        let mut policy = make_group_policy(group("games", no_limits(), None));
        let gated = policy
            .entries
            .iter_mut()
            .find(|e| e.id.as_str() == "game-a")
            .unwrap();
        gated.tokens = Some(TokensPolicy {
            from: vec![LimitSubject::entry("ungrouped")],
            earn_ratio: 1.0,
            minimum: Duration::from_secs(300),
            max_balance: None,
            carry_over: false,
        });

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "ungrouped", Duration::from_secs(600), now);
        store
            .upsert_daily_override(
                &LimitSubject::group("games"),
                now.date_naive(),
                Some(true),
                None,
            )
            .unwrap();

        // The category is on for the day, so the member's own gate and clamp
        // are lifted...
        assert!(
            view(&engine.list_entries(now), "game-a")
                .max_run_if_started_now
                .is_none(),
            "a group force-enable should lift the member's token clamp"
        );

        // ...and a session under it is not billed to that member's balance.
        run_session(&mut engine, "game-a", Duration::from_secs(1800), now);
        assert_eq!(
            balance_of(&engine, "game-a", now),
            Duration::from_secs(600),
            "a session granted at group level should not spend the entry's tokens"
        );
    }

    /// The mirror case: a force-enable on the *member* lifts the group's token
    /// gate, so the category's shared balance must not be spent either — the
    /// siblings would otherwise pay for a session a caregiver granted.
    #[test]
    fn test_member_override_exempts_the_groups_token_balance() {
        let policy = make_group_policy(group(
            "games",
            no_limits(),
            Some(TokensPolicy {
                from: vec![LimitSubject::entry("ungrouped")],
                earn_ratio: 1.0,
                minimum: Duration::from_secs(300),
                max_balance: None,
                carry_over: false,
            }),
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "ungrouped", Duration::from_secs(600), now);
        store
            .upsert_daily_override(
                &LimitSubject::entry("game-a"),
                now.date_naive(),
                Some(true),
                None,
            )
            .unwrap();

        run_session(&mut engine, "game-a", Duration::from_secs(1800), now);
        assert_eq!(
            group_balance_of(&engine, "games", now),
            Duration::from_secs(600),
            "a session granted on one member should not spend the category's tokens"
        );
        assert!(
            view(&engine.list_entries(now), "game-b").enabled,
            "the sibling should still be unlocked by the untouched balance"
        );
    }

    /// `minimum_seconds` has to be banked every time the gate opens, not just
    /// the first (issue #193): it is what guarantees a session long enough to
    /// be worth starting, so a partial spend that leaves less than it re-locks
    /// the activity until more is earned — without losing what was left.
    #[test]
    fn test_token_gate_relocks_when_a_spend_leaves_less_than_the_minimum() {
        let policy = make_token_policy(TokensPolicy {
            from: vec![LimitSubject::entry("scratch")],
            earn_ratio: 1.0,
            minimum: Duration::from_secs(600),
            max_balance: None,
            carry_over: false,
        });
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // Just short of the threshold: still locked.
        run_session(&mut engine, "scratch", Duration::from_secs(500), now);
        assert!(!view(&engine.list_entries(now), "minecraft").enabled);

        // Crossing it opens the gate, onto the whole balance rather than only
        // the part above the threshold.
        run_session(&mut engine, "scratch", Duration::from_secs(200), now);
        let entries = engine.list_entries(now);
        let minecraft = view(&entries, "minecraft");
        assert!(minecraft.enabled);
        assert_eq!(
            minecraft.max_run_if_started_now,
            Some(Duration::from_secs(700)),
            "a session should not be cut off at the threshold"
        );

        // A session that leaves less than the minimum re-locks the entry...
        run_session(&mut engine, "minecraft", Duration::from_secs(300), now);
        let entries = engine.list_entries(now);
        let minecraft = view(&entries, "minecraft");
        assert!(
            !minecraft.enabled,
            "a balance below the minimum should lock the entry again"
        );
        assert_eq!(
            minecraft.reasons,
            vec![ReasonCode::TokensInsufficient {
                balance: Duration::from_secs(400),
                required: Duration::from_secs(600),
            }]
        );
        assert!(
            matches!(
                engine.request_launch(&EntryId::new("minecraft"), now),
                LaunchDecision::Denied { .. }
            ),
            "a locked gate should refuse a launch, not only hide the tile"
        );

        // ...but keeps the remainder banked, and it counts toward the next
        // unlock, which again opens onto the whole balance.
        assert_eq!(
            balance_of(&engine, "minecraft", now),
            Duration::from_secs(400)
        );
        run_session(&mut engine, "scratch", Duration::from_secs(200), now);
        let entries = engine.list_entries(now);
        let minecraft = view(&entries, "minecraft");
        assert!(minecraft.enabled, "{:?}", minecraft.reasons);
        assert_eq!(
            minecraft.max_run_if_started_now,
            Some(Duration::from_secs(600))
        );
    }

    /// The same at group level, where the issue was reported (issue #193): a
    /// member's session that leaves the category's shared balance below its
    /// minimum locks every member, not just the one that was played.
    #[test]
    fn test_group_token_gate_relocks_every_member_below_the_minimum() {
        let policy = make_group_policy(group(
            "games",
            no_limits(),
            Some(TokensPolicy {
                from: vec![LimitSubject::entry("ungrouped")],
                earn_ratio: 1.0,
                minimum: Duration::from_secs(300),
                max_balance: None,
                carry_over: false,
            }),
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        run_session(&mut engine, "ungrouped", Duration::from_secs(400), now);
        let entries = engine.list_entries(now);
        assert!(view(&entries, "game-a").enabled);
        assert!(view(&entries, "game-b").enabled);

        run_session(&mut engine, "game-a", Duration::from_secs(200), now);
        assert_eq!(
            group_balance_of(&engine, "games", now),
            Duration::from_secs(200)
        );
        let entries = engine.list_entries(now);
        for member in ["game-a", "game-b"] {
            assert!(
                !view(&entries, member).enabled,
                "{member} should lock with the category below its minimum"
            );
        }

        run_session(&mut engine, "ungrouped", Duration::from_secs(100), now);
        let entries = engine.list_entries(now);
        assert!(view(&entries, "game-a").enabled);
        assert!(view(&entries, "game-b").enabled);
    }

    /// A caregiver's grant behaves exactly like earned time: banked, capped,
    /// spendable, and opening the gate only at `minimum_seconds` (issue #8).
    #[test]
    fn test_manual_grant_banks_time_and_opens_at_the_minimum() {
        let policy = make_token_policy(TokensPolicy {
            from: vec![LimitSubject::entry("scratch")],
            earn_ratio: 1.0,
            minimum: Duration::from_secs(600),
            max_balance: Some(Duration::from_secs(900)),
            carry_over: false,
        });
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();
        let minecraft = LimitSubject::entry("minecraft");

        // Short of the threshold: banked, still locked. A grant is not a
        // bypass — the availability override is the tool for that.
        let status = engine.adjust_tokens(&minecraft, 300, now).unwrap();
        assert_eq!(status.balance, Duration::from_secs(300));
        assert!(!status.unlocked);
        assert!(!view(&engine.list_entries(now), "minecraft").enabled);

        // Crossing it opens the gate.
        let status = engine.adjust_tokens(&minecraft, 300, now).unwrap();
        assert!(status.unlocked);
        let entries = engine.list_entries(now);
        let v = view(&entries, "minecraft");
        assert!(v.enabled);
        assert_eq!(v.max_run_if_started_now, Some(Duration::from_secs(600)));

        // Granted time is spent by a session like any other, and what that
        // leaves below the minimum is locked again (issue #193).
        run_session(&mut engine, "minecraft", Duration::from_secs(200), now);
        assert_eq!(
            balance_of(&engine, "minecraft", now),
            Duration::from_secs(400)
        );
        assert!(!view(&engine.list_entries(now), "minecraft").enabled);

        // The ceiling applies to grants too, and revoking saturates at zero.
        assert_eq!(
            engine.adjust_tokens(&minecraft, 5000, now).unwrap().balance,
            Duration::from_secs(900),
        );
        assert_eq!(
            engine
                .adjust_tokens(&minecraft, -5000, now)
                .unwrap()
                .balance,
            Duration::ZERO,
        );
        assert!(!view(&engine.list_entries(now), "minecraft").enabled);
    }

    #[test]
    fn test_manual_grant_rejects_subjects_with_no_gate() {
        let policy = make_token_policy(tokens_from(&["scratch"]));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        // An ungated entry has no balance anything would read, so writing one
        // would be a silent no-op rather than a grant.
        assert_eq!(
            engine
                .adjust_tokens(&LimitSubject::entry("scratch"), 300, now)
                .unwrap_err(),
            TokenAdjustError::NotGated,
        );
        assert_eq!(
            engine
                .adjust_tokens(&LimitSubject::entry("nope"), 300, now)
                .unwrap_err(),
            TokenAdjustError::UnknownSubject,
        );
        assert_eq!(
            engine
                .adjust_tokens(&LimitSubject::group("nope"), 300, now)
                .unwrap_err(),
            TokenAdjustError::UnknownSubject,
        );
    }

    /// A grant on a category unlocks every member, and the views report it.
    #[test]
    fn test_manual_grant_on_a_group_unlocks_its_members() {
        let policy = make_group_policy(group(
            "games",
            no_limits(),
            Some(TokensPolicy {
                from: vec![LimitSubject::entry("ungrouped")],
                earn_ratio: 1.0,
                minimum: Duration::from_secs(600),
                max_balance: None,
                carry_over: false,
            }),
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let engine = CoreEngine::new(policy, store, HostCapabilities::minimal());
        let now = noon();

        let status = engine
            .adjust_tokens(&LimitSubject::group("games"), 600, now)
            .unwrap();
        assert!(status.unlocked);

        let entries = engine.list_entries(now);
        for id in ["game-a", "game-b"] {
            assert!(
                view(&entries, id).enabled,
                "{id} should unlock with its category"
            );
        }

        // And the caregiver-facing status rides along on the views.
        let groups = engine.list_groups(now);
        let tokens = groups[0]
            .tokens
            .as_ref()
            .expect("group gate should be reported");
        assert_eq!(tokens.balance, Duration::from_secs(600));
        assert_eq!(tokens.minimum, Duration::from_secs(600));
        assert!(tokens.unlocked);
        assert!(
            view(&entries, "game-a").tokens.is_none(),
            "a member has no gate of its own; the category's is on the GroupView"
        );
    }

    /// `list_groups` must report the cap the members are actually held to.
    #[test]
    fn test_group_view_max_run_honours_a_force_enable() {
        let policy = make_group_policy(group(
            "games",
            LimitsPolicy {
                max_run: Some(Duration::from_secs(900)),
                daily_quota: Some(Duration::from_secs(600)),
                ..no_limits()
            },
            None,
        ));
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store.clone(), HostCapabilities::minimal());
        let now = noon();

        // Spend the category's quota, then switch it on for the day anyway.
        run_session(&mut engine, "game-a", Duration::from_secs(600), now);
        store
            .upsert_daily_override(
                &LimitSubject::group("games"),
                now.date_naive(),
                Some(true),
                None,
            )
            .unwrap();

        let groups = engine.list_groups(now);
        let games = groups.first().expect("the group should be listed");
        assert_eq!(
            games.max_run_if_started_now,
            Some(Duration::from_secs(900)),
            "an overridden group should report its per-session cap, not a spent quota"
        );
        assert_eq!(
            view(&engine.list_entries(now), "game-b").max_run_if_started_now,
            games.max_run_if_started_now,
            "the group's reported cap should match what its members get"
        );
    }

    // --- Reset / restart in place (issue #125) ----------------------------

    /// A policy whose one entry is a RetroArch activity, which is the only
    /// kind that can be reset.
    fn make_resettable_policy() -> Policy {
        let mut policy = make_test_policy();
        policy.entries[0].kind = EntryKind::Retroarch {
            core: Some("mgba".into()),
            core_path: None,
            content: "/roms/game.gba".into(),
            save_state: lunchbox_api::RetroarchSaveState::Auto,
            command: "retroarch".into(),
            args: vec![],
            env: HashMap::new(),
            kiosk: true,
            reset: true,
        };
        policy
    }

    fn start_session(engine: &mut CoreEngine, id: &str) -> lunchbox_util::SessionId {
        let now = lunchbox_util::now();
        let entry_id = EntryId::new(id);
        let LaunchDecision::Approved(plan) = engine.request_launch(&entry_id, now) else {
            panic!("launch should be approved");
        };
        let session_id = plan.session_id.clone();
        engine.start_session(plan, now, MonotonicInstant::now());
        session_id
    }

    /// The exit a reset causes must not end the session — that is the whole
    /// difference between resetting an activity and closing it.
    #[test]
    fn restart_keeps_the_session_across_the_activitys_exit() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // linux_full, not minimal: the engine gates launches on the host
        // supporting the entry's kind, and minimal only spawns processes.
        let mut engine = CoreEngine::new(
            make_resettable_policy(),
            store,
            HostCapabilities::linux_full(),
        );
        let session_id = start_session(&mut engine, "test-game");

        let request = engine.begin_restart().expect("entry supports reset");
        assert_eq!(request.session_id, session_id);
        assert!(engine.is_restarting());

        // The teardown's exit arrives looking exactly like a crash.
        let ended =
            engine.end_current_session(Some(0), MonotonicInstant::now(), lunchbox_util::now());
        assert!(ended.is_none(), "a reset's exit must not end the session");
        assert!(engine.has_active_session());

        // The replacement process takes over the same session.
        engine.finish_restart(
            Some(HostSessionHandle::new(
                session_id.clone(),
                lunchbox_host_api::HostHandlePayload::Linux { pid: 42, pgid: 42 },
            )),
            MonotonicInstant::now(),
            lunchbox_util::now(),
        );
        assert!(!engine.is_restarting());
        let session = engine.current_session().expect("session should survive");
        assert_eq!(
            session.plan.session_id, session_id,
            "a reset replaces the process, not the session"
        );
    }

    /// Once the restart is over, a real exit ends the session as usual —
    /// the suppression must not outlive the operation that needed it.
    #[test]
    fn a_later_exit_still_ends_the_session() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // linux_full, not minimal: the engine gates launches on the host
        // supporting the entry's kind, and minimal only spawns processes.
        let mut engine = CoreEngine::new(
            make_resettable_policy(),
            store,
            HostCapabilities::linux_full(),
        );
        start_session(&mut engine, "test-game");

        engine.begin_restart().expect("entry supports reset");
        engine.finish_restart(
            Some(HostSessionHandle::new(
                SessionId::new(),
                lunchbox_host_api::HostHandlePayload::Linux { pid: 42, pgid: 42 },
            )),
            MonotonicInstant::now(),
            lunchbox_util::now(),
        );

        let ended =
            engine.end_current_session(Some(0), MonotonicInstant::now(), lunchbox_util::now());
        assert!(ended.is_some(), "the next real exit should end the session");
        assert!(!engine.has_active_session());
    }

    /// A relaunch that fails leaves nothing behind the session, so it ends
    /// rather than lingering as a session with no process.
    #[test]
    fn a_failed_relaunch_ends_the_session() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // linux_full, not minimal: the engine gates launches on the host
        // supporting the entry's kind, and minimal only spawns processes.
        let mut engine = CoreEngine::new(
            make_resettable_policy(),
            store,
            HostCapabilities::linux_full(),
        );
        start_session(&mut engine, "test-game");

        engine.begin_restart().expect("entry supports reset");
        let ended = engine.finish_restart(None, MonotonicInstant::now(), lunchbox_util::now());
        assert!(
            matches!(ended, Some(CoreEvent::SessionEnded { .. })),
            "a failed relaunch should end the session"
        );
        assert!(!engine.has_active_session());
        assert!(!engine.is_restarting());
    }

    /// Activities that don't support reset are refused before anything is
    /// torn down — otherwise the button would kill an activity it can't
    /// bring back to a meaningful state.
    #[test]
    fn restart_is_refused_for_activities_that_do_not_support_it() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // make_test_policy's entry is a plain process.
        let mut engine = CoreEngine::new(make_test_policy(), store, HostCapabilities::minimal());
        start_session(&mut engine, "test-game");

        assert!(engine.begin_restart().is_none());
        assert!(!engine.is_restarting());
        assert!(engine.has_active_session());
    }

    /// The exit of the process a reset replaced can arrive *after* the
    /// replacement is attached — the host reports it through a channel the
    /// reset does not wait on. That stale event must not end a session whose
    /// activity is running fine. (Found end-to-end: the reset worked, then the
    /// old process's exit ended the session a moment later.)
    #[test]
    fn a_replaced_processs_late_exit_does_not_end_the_session() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_resettable_policy(),
            store,
            HostCapabilities::linux_full(),
        );
        let session_id = start_session(&mut engine, "test-game");

        let old = HostSessionHandle::new(
            session_id.clone(),
            lunchbox_host_api::HostHandlePayload::Linux {
                pid: 100,
                pgid: 100,
            },
        );
        engine.attach_host_handle(old.clone());

        // Reset: the old process goes away, a new one takes its place.
        engine.begin_restart().expect("entry supports reset");
        let new = HostSessionHandle::new(
            session_id.clone(),
            lunchbox_host_api::HostHandlePayload::Linux {
                pid: 200,
                pgid: 200,
            },
        );
        engine.finish_restart(
            Some(new.clone()),
            MonotonicInstant::now(),
            lunchbox_util::now(),
        );

        // Now the old process's exit finally arrives.
        let ended = engine.notify_activity_exited(
            &old,
            Some(0),
            MonotonicInstant::now(),
            lunchbox_util::now(),
        );
        assert!(
            ended.is_none(),
            "an exit from the replaced process must not end the session"
        );
        assert!(engine.has_active_session());

        // The replacement's own exit still ends it.
        let ended = engine.notify_activity_exited(
            &new,
            Some(0),
            MonotonicInstant::now(),
            lunchbox_util::now(),
        );
        assert!(ended.is_some(), "the live process's exit should end it");
        assert!(!engine.has_active_session());
    }

    #[test]
    fn restart_is_refused_with_no_session() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // linux_full, not minimal: the engine gates launches on the host
        // supporting the entry's kind, and minimal only spawns processes.
        let mut engine = CoreEngine::new(
            make_resettable_policy(),
            store,
            HostCapabilities::linux_full(),
        );
        assert!(engine.begin_restart().is_none());
        assert!(!engine.is_restarting());
    }

    // --- Resume from sleep (issue #155) ------------------------------------

    /// Two hour-capped entries: one playable only 19:00-20:00, one with no
    /// hours at all, so a test can pick whether the resume lands outside the
    /// schedule or merely later than the display believes.
    fn make_bedtime_policy() -> Policy {
        use lunchbox_api::WarningThreshold;
        use lunchbox_util::{DaysOfWeek, TimeWindow, WallClock};

        let mut entry = token_entry("bedtime-game", None);
        entry.availability = AvailabilityPolicy {
            windows: vec![TimeWindow::new(
                DaysOfWeek::ALL_DAYS,
                WallClock::new(19, 0).unwrap(),
                WallClock::new(20, 0).unwrap(),
            )],
            always: false,
        };
        entry.limits = LimitsPolicy {
            max_run: Some(Duration::from_secs(3600)),
            ..no_limits()
        };
        entry.warnings = vec![
            WarningThreshold {
                seconds_before: 300,
                severity: WarningSeverity::Warn,
                message_template: None,
            },
            WarningThreshold {
                seconds_before: 60,
                severity: WarningSeverity::Critical,
                message_template: None,
            },
        ];

        let mut anytime = token_entry("anytime-game", None);
        anytime.limits = LimitsPolicy {
            max_run: Some(Duration::from_secs(3600)),
            ..no_limits()
        };

        Policy {
            service: Default::default(),
            groups: vec![],
            entries: vec![entry, anytime],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
            hud_orientation: Default::default(),
        }
    }

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        use chrono::TimeZone;
        chrono::Local
            .with_ymd_and_hms(2026, 4, 27, hour, minute, 0)
            .unwrap()
    }

    /// Launch `entry_id` at `now` and hand back the monotonic instant the
    /// session started from, so a test can advance the two clocks separately —
    /// which is the whole point: a sleep advances the wall clock and not the
    /// monotonic one.
    fn launch_at(
        engine: &mut CoreEngine,
        entry_id: &str,
        now: DateTime<Local>,
    ) -> MonotonicInstant {
        let entry_id = EntryId::new(entry_id);
        let plan = match engine.request_launch(&entry_id, now) {
            LaunchDecision::Approved(plan) => plan,
            LaunchDecision::Denied { reasons } => panic!("launch denied: {reasons:?}"),
        };
        let started = MonotonicInstant::now();
        engine.start_session(plan, now, started);
        started
    }

    /// The bug the issue actually describes: the enforcement clock correctly
    /// ignores the time asleep, but the wall-clock deadline every UI counts
    /// down from does not, so the HUD reads low by the length of the sleep and
    /// can sit at 0:00 while the session runs on.
    #[test]
    fn a_resume_corrects_the_displayed_deadline_for_time_asleep() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_bedtime_policy(), store, HostCapabilities::minimal());

        // An activity with no hours of its own, so the resume is late but not
        // out of bounds: this test is only about the displayed deadline.
        let started = launch_at(&mut engine, "anytime-game", at(19, 0));
        assert_eq!(engine.current_session().unwrap().deadline, Some(at(20, 0)));

        // Five minutes of play, then three hours asleep. Monotonic time only
        // advanced by the five minutes.
        let awake = Duration::from_secs(5 * 60);
        let events = engine.notify_resumed(at(22, 5), started + awake);

        let session = engine.current_session().unwrap();
        assert_eq!(
            session.time_remaining(started + awake),
            Some(Duration::from_secs(55 * 60)),
            "the monotonic budget must not be spent by sleeping"
        );
        assert_eq!(
            session.deadline,
            Some(at(23, 0)),
            "the displayed deadline must be re-derived from the monotonic one"
        );
        assert!(
            events.is_empty(),
            "an activity with no hours cannot be woken outside them: {events:?}"
        );
    }

    /// The safety hole underneath it: the window is checked once, at launch.
    /// A sleep pushes the real end of the session past it, so waking outside
    /// the activity's hours has to end the session — with time to save first.
    #[test]
    fn a_resume_outside_allowed_hours_clamps_the_session_to_its_save_grace() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_bedtime_policy(), store, HostCapabilities::minimal());

        // 19:30 leaves half an hour of window, which is what clamps the session.
        let started = launch_at(&mut engine, "bedtime-game", at(19, 30));
        assert_eq!(engine.current_session().unwrap().deadline, Some(at(20, 0)));

        let resumed_at = started + Duration::from_secs(5 * 60);
        let events = engine.notify_resumed(at(22, 35), resumed_at);

        let grace = lunchbox_config::DEFAULT_SAVE_GRACE;
        let session = engine.current_session().unwrap();
        assert_eq!(
            session.time_remaining(resumed_at),
            Some(grace),
            "25 minutes of budget survived the sleep, but bedtime has passed"
        );
        assert_eq!(session.deadline, Some(at(22, 37)));
        assert!(session.save_grace_started);

        let [
            CoreEvent::Warning {
                time_remaining,
                severity,
                message,
                ..
            },
        ] = events.as_slice()
        else {
            panic!("expected exactly one warning, got {events:?}");
        };
        assert_eq!(*time_remaining, grace);
        assert_eq!(*severity, WarningSeverity::Critical);
        assert!(
            message.as_ref().is_some_and(|m| m.contains("save")),
            "the child needs to be told why, not just that: {message:?}"
        );

        // The five-minute threshold would otherwise fire on the very next tick
        // and overwrite that explanation with a bare countdown.
        assert!(session.warnings_issued.contains(&300));
        assert!(!session.warnings_issued.contains(&60));

        // And the grace really is a deadline.
        let expiry = engine.tick(resumed_at + grace, at(22, 37));
        assert!(
            expiry
                .iter()
                .any(|e| matches!(e, CoreEvent::ExpireDue { .. })),
            "expected the session to expire when the grace ran out: {expiry:?}"
        );
    }

    /// Suspend/resume is a loop a child can drive from the lid switch, and the
    /// grace does not burn down while the machine is asleep. Renewing it on
    /// every wake would make the session unbounded.
    #[test]
    fn a_second_resume_does_not_renew_the_save_grace() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_bedtime_policy(), store, HostCapabilities::minimal());

        let started = launch_at(&mut engine, "bedtime-game", at(19, 30));
        let first = started + Duration::from_secs(5 * 60);
        assert_eq!(engine.notify_resumed(at(22, 35), first).len(), 1);

        // Thirty seconds of the grace spent, then asleep again.
        let second = first + Duration::from_secs(30);
        let events = engine.notify_resumed(at(23, 40), second);

        assert!(events.is_empty(), "the grace is granted once: {events:?}");
        assert_eq!(
            engine.current_session().unwrap().time_remaining(second),
            Some(Duration::from_secs(90)),
            "the second wake must not hand back the thirty seconds already spent"
        );
    }

    /// A parent who force-enabled the activity for the day has said the
    /// schedule does not apply, exactly as `evaluate_entry` reads it.
    #[test]
    fn a_force_enabled_entry_has_no_allowed_hours_to_be_outside_of() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        store
            .upsert_daily_override(
                &LimitSubject::Entry(EntryId::new("bedtime-game")),
                at(19, 30).date_naive(),
                Some(true),
                None,
            )
            .unwrap();
        let mut engine = CoreEngine::new(make_bedtime_policy(), store, HostCapabilities::minimal());

        let started = launch_at(&mut engine, "bedtime-game", at(19, 30));
        let resumed_at = started + Duration::from_secs(5 * 60);
        let events = engine.notify_resumed(at(22, 35), resumed_at);

        assert!(
            events.is_empty(),
            "a force-enable lifts the window: {events:?}"
        );
        let session = engine.current_session().unwrap();
        assert!(!session.save_grace_started);
        assert_eq!(
            session.time_remaining(resumed_at),
            Some(Duration::from_secs(55 * 60)),
            "the force-enabled session keeps its full remaining budget"
        );
    }

    /// A shut activity says when it comes back, not merely that it is shut.
    ///
    /// The compartment floor in the launcher is the reader: "Opens 7:00 PM"
    /// rather than a dimmed category with nothing to say for itself. It had
    /// been a `None` and a TODO since the reason code was written.
    #[test]
    fn an_activity_outside_its_hours_says_when_it_opens() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let engine = CoreEngine::new(make_bedtime_policy(), store, HostCapabilities::minimal());

        let view = |now| {
            engine
                .list_entries(now)
                .into_iter()
                .find(|e| e.entry_id.as_str() == "bedtime-game")
                .unwrap()
        };

        // The window is 19:00-20:00 every day.
        let morning = view(at(9, 0));
        assert_eq!(
            morning.reasons,
            vec![ReasonCode::OutsideTimeWindow {
                next_window_start: Some(at(19, 0)),
            }],
            "asked in the morning, it opens this evening"
        );

        let after = view(at(21, 0));
        let tomorrow = at(19, 0) + chrono::Duration::days(1);
        assert_eq!(
            after.reasons,
            vec![ReasonCode::OutsideTimeWindow {
                next_window_start: Some(tomorrow),
            }],
            "asked after it shuts, it opens tomorrow"
        );
    }

    /// The group carries the same schedule an entry does (issue #5), so its
    /// window has to close a member's session too.
    #[test]
    fn a_group_window_that_has_closed_ends_a_members_session() {
        use lunchbox_util::{DaysOfWeek, TimeWindow, WallClock};

        let mut group = group("evening", no_limits(), None);
        group.availability = AvailabilityPolicy {
            windows: vec![TimeWindow::new(
                DaysOfWeek::ALL_DAYS,
                WallClock::new(19, 0).unwrap(),
                WallClock::new(20, 0).unwrap(),
            )],
            always: false,
        };
        let mut policy = make_group_policy(group);
        // The member itself is always available; only the category has hours.
        policy.entries[0].limits = LimitsPolicy {
            max_run: Some(Duration::from_secs(3600)),
            ..no_limits()
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());

        let started = launch_at(&mut engine, "game-a", at(19, 30));
        let resumed_at = started + Duration::from_secs(5 * 60);
        let events = engine.notify_resumed(at(22, 35), resumed_at);

        assert_eq!(
            events.len(),
            1,
            "expected the category's hours to bind: {events:?}"
        );
        assert_eq!(
            engine.current_session().unwrap().time_remaining(resumed_at),
            Some(lunchbox_config::DEFAULT_SAVE_GRACE)
        );
    }

    /// A session that was already ending sooner than the grace is left alone:
    /// there is nothing to clamp, and warning about a limit that is not
    /// binding would only confuse the child.
    #[test]
    fn a_session_ending_sooner_than_the_grace_is_not_extended_by_it() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_bedtime_policy(), store, HostCapabilities::minimal());

        let started = launch_at(&mut engine, "bedtime-game", at(19, 30));
        // Wake with 30 seconds of the half-hour window budget left.
        let resumed_at = started + Duration::from_secs(30 * 60 - 30);
        let events = engine.notify_resumed(at(22, 35), resumed_at);

        assert!(events.is_empty(), "{events:?}");
        assert_eq!(
            engine.current_session().unwrap().time_remaining(resumed_at),
            Some(Duration::from_secs(30)),
            "the grace is a ceiling, never a floor"
        );
    }

    /// `save_grace_seconds = 0` is the documented way to say "cut it off on
    /// wake", so it has to actually expire rather than quietly do nothing.
    #[test]
    fn a_zero_save_grace_ends_the_session_on_the_next_tick() {
        let mut policy = make_bedtime_policy();
        policy.entries[0].limits.save_grace = Duration::ZERO;

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(policy, store, HostCapabilities::minimal());

        let started = launch_at(&mut engine, "bedtime-game", at(19, 30));
        let resumed_at = started + Duration::from_secs(5 * 60);
        engine.notify_resumed(at(22, 35), resumed_at);

        assert_eq!(
            engine.current_session().unwrap().time_remaining(resumed_at),
            Some(Duration::ZERO)
        );
        let events = engine.tick(resumed_at, at(22, 35));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, CoreEvent::ExpireDue { .. })),
            "{events:?}"
        );
    }

    /// Nothing to reconcile with no session, and nothing to crash on either —
    /// most resumes happen with the launcher on screen.
    #[test]
    fn a_resume_with_no_session_is_a_no_op() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(make_bedtime_policy(), store, HostCapabilities::minimal());

        assert!(
            engine
                .notify_resumed(at(22, 35), MonotonicInstant::now())
                .is_empty()
        );
    }

    // ---- Billing to the day a session started (issue #170) ----------------

    /// The same fixed April 2026 week as [`at`], but with the day spelled out
    /// so a test can straddle midnight.
    fn on_day(day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        use chrono::TimeZone;
        chrono::Local
            .with_ymd_and_hms(2026, 4, day, hour, minute, 0)
            .unwrap()
    }

    /// Run a complete session that starts and ends at the given wall-clock
    /// times, advancing the monotonic clock by the gap between them.
    fn run_session_between(
        engine: &mut CoreEngine,
        entry_id: &str,
        start: DateTime<Local>,
        end: DateTime<Local>,
    ) {
        let entry_id = EntryId::new(entry_id);
        let plan = match engine.request_launch(&entry_id, start) {
            LaunchDecision::Approved(plan) => plan,
            LaunchDecision::Denied { reasons } => {
                panic!("launch of {entry_id} denied: {reasons:?}")
            }
        };
        let started = MonotonicInstant::now();
        engine.start_session(plan, start, started);
        let elapsed = (end - start)
            .to_std()
            .expect("the session ends after it starts");
        engine.end_current_session(Some(0), started + elapsed, end);
    }

    fn make_quota_policy(quota: Duration) -> Policy {
        let mut policy = make_test_policy();
        policy.entries[0].limits.max_run = None;
        policy.entries[0].limits.daily_quota = Some(quota);
        policy
    }

    /// The whole of issue #170: a session run up to bedtime is yesterday's
    /// play, however far past midnight it happened to end.
    #[test]
    fn usage_is_billed_to_the_day_the_session_started() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );

        let start = on_day(27, 23, 50);
        let end = on_day(28, 0, 10);
        run_session_between(&mut engine, "test-game", start, end);

        let entry_id = EntryId::new("test-game");
        assert_eq!(
            store.get_usage(&entry_id, start.date_naive()).unwrap(),
            Duration::from_secs(20 * 60),
            "the whole session belongs to the day it started"
        );
        assert_eq!(
            store.get_usage(&entry_id, end.date_naive()).unwrap(),
            Duration::ZERO,
            "and none of it to the day it ended"
        );
    }

    /// What the child actually notices: the new day's budget is whole.
    #[test]
    fn a_session_across_midnight_leaves_the_new_days_quota_untouched() {
        let quota = Duration::from_secs(30 * 60);
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine =
            CoreEngine::new(make_quota_policy(quota), store, HostCapabilities::minimal());

        run_session_between(
            &mut engine,
            "test-game",
            on_day(27, 23, 50),
            on_day(28, 0, 10),
        );

        let before_midnight = engine.list_entries(on_day(27, 23, 55));
        assert_eq!(
            before_midnight[0].max_run_if_started_now,
            Some(Duration::from_secs(10 * 60)),
            "the 20 minutes came out of the day the session started on"
        );

        let after_midnight = engine.list_entries(on_day(28, 0, 15));
        assert_eq!(
            after_midnight[0].max_run_if_started_now,
            Some(quota),
            "the new day starts with its whole quota"
        );
    }

    /// A launch that never produced an activity is still not billed, whichever
    /// day it is asked about.
    #[test]
    fn a_failed_launch_across_midnight_is_billed_to_neither_day() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );

        let entry_id = EntryId::new("test-game");
        let start = on_day(27, 23, 50);
        let LaunchDecision::Approved(plan) = engine.request_launch(&entry_id, start) else {
            panic!("launch should be approved");
        };
        let started = MonotonicInstant::now();
        engine.start_session(plan, start, started);
        engine.notify_launch_failed(
            None,
            "never started".into(),
            started + Duration::from_secs(20 * 60),
            on_day(28, 0, 10),
        );

        assert_eq!(
            store
                .get_usage(&entry_id, on_day(27, 0, 0).date_naive())
                .unwrap(),
            Duration::ZERO
        );
        assert_eq!(
            store
                .get_usage(&entry_id, on_day(28, 0, 0).date_naive())
                .unwrap(),
            Duration::ZERO
        );
    }

    /// A carry-over gate has one continuous balance, so a session that crossed
    /// midnight still spends it down.
    #[test]
    fn a_carry_over_gate_is_still_spent_across_midnight() {
        let mut tokens = tokens_from(&["scratch"]);
        tokens.carry_over = true;

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_token_policy(tokens),
            store,
            HostCapabilities::minimal(),
        );

        run_session(
            &mut engine,
            "scratch",
            Duration::from_secs(30 * 60),
            on_day(27, 20, 0),
        );
        run_session_between(
            &mut engine,
            "minecraft",
            on_day(27, 23, 50),
            on_day(28, 0, 10),
        );

        assert_eq!(
            balance_of(&engine, "minecraft", on_day(28, 0, 15)),
            Duration::from_secs(10 * 60),
            "30 minutes banked, 20 spent"
        );
    }

    /// A gate that doesn't carry over threw its balance away at midnight, so
    /// there is nothing left for the session to settle against — and in
    /// particular it must not reach into what a caregiver granted afterwards.
    #[test]
    fn a_grant_after_midnight_survives_a_session_that_started_yesterday() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_token_policy(tokens_from(&["scratch"])),
            store,
            HostCapabilities::minimal(),
        );

        // Yesterday: half an hour earned, and a session started on it at 23:50.
        run_session(
            &mut engine,
            "scratch",
            Duration::from_secs(30 * 60),
            on_day(27, 20, 0),
        );
        let entry_id = EntryId::new("minecraft");
        let LaunchDecision::Approved(plan) = engine.request_launch(&entry_id, on_day(27, 23, 50))
        else {
            panic!("launch should be approved");
        };
        let started = MonotonicInstant::now();
        engine.start_session(plan, on_day(27, 23, 50), started);

        // A caregiver banks ten minutes after midnight, while it is still running.
        let subject = LimitSubject::entry("minecraft");
        engine
            .adjust_tokens(&subject, 10 * 60, on_day(28, 0, 5))
            .expect("the gate accepts a grant");

        engine.end_current_session(
            Some(0),
            started + Duration::from_secs(20 * 60),
            on_day(28, 0, 10),
        );

        assert_eq!(
            balance_of(&engine, "minecraft", on_day(28, 0, 15)),
            Duration::from_secs(10 * 60),
            "the session spent yesterday's balance, not today's grant"
        );
    }

    /// The force-enable exemption is keyed by date too: the grant that approved
    /// a session started at 23:50 expired at midnight, so it has to be looked up
    /// on the billed day or the child pays for time that was given to them.
    #[test]
    fn a_force_enable_from_the_start_day_still_exempts_the_spend() {
        let mut tokens = tokens_from(&["scratch"]);
        tokens.carry_over = true;

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_token_policy(tokens),
            store.clone(),
            HostCapabilities::minimal(),
        );

        run_session(
            &mut engine,
            "scratch",
            Duration::from_secs(30 * 60),
            on_day(27, 20, 0),
        );
        store
            .upsert_daily_override(
                &LimitSubject::entry("minecraft"),
                on_day(27, 0, 0).date_naive(),
                Some(true),
                None,
            )
            .unwrap();

        run_session_between(
            &mut engine,
            "minecraft",
            on_day(27, 23, 50),
            on_day(28, 0, 10),
        );

        assert_eq!(
            balance_of(&engine, "minecraft", on_day(28, 0, 15)),
            Duration::from_secs(30 * 60),
            "a session the caregiver granted is not billed to the gate"
        );
    }

    // ---- Surviving a power cut mid-session (issue #201) --------------------

    /// Run `entry_id` from `start` for `played`, then drop the engine the way a
    /// power cut does: no end, no settlement, nothing but whatever reached the
    /// store. Hands back a fresh engine on the same store, as the next boot
    /// would see it.
    fn power_cut_after(
        store: Arc<SqliteStore>,
        policy: Policy,
        entry_id: &str,
        start: DateTime<Local>,
        played: Duration,
    ) -> CoreEngine {
        let mut engine =
            CoreEngine::new(policy.clone(), store.clone(), HostCapabilities::minimal());
        let started = launch_at(&mut engine, entry_id, start);

        // Tick the way lunchboxd does, so the checkpoint lands exactly when it
        // would in production rather than because the test asked for it.
        let mut elapsed = Duration::ZERO;
        while elapsed <= played {
            engine.tick(
                started + elapsed,
                start + chrono::Duration::from_std(elapsed).unwrap(),
            );
            elapsed += Duration::from_secs(1);
        }

        drop(engine);
        CoreEngine::new(policy, store, HostCapabilities::minimal())
    }

    /// The bypass in issue #201: hold the power button and the whole session is
    /// refunded. What survives is the last checkpoint, not nothing.
    #[test]
    fn a_session_cut_short_by_a_power_cut_is_still_billed() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 16, 0);
        let mut next_boot = power_cut_after(
            store.clone(),
            make_test_policy(),
            "test-game",
            start,
            // Two checkpoints in, plus change that is lost with the power.
            SNAPSHOT_INTERVAL * 2 + Duration::from_secs(7),
        );

        let entry_id = EntryId::new("test-game");
        assert_eq!(
            store.get_usage(&entry_id, start.date_naive()).unwrap(),
            Duration::ZERO,
            "nothing is billed until the next start reconciles it"
        );

        let recovered = next_boot
            .recover_interrupted_session(on_day(27, 16, 5))
            .expect("the interrupted session is found at startup");

        assert_eq!(recovered.entry_id, entry_id);
        assert_eq!(
            recovered.billed,
            SNAPSHOT_INTERVAL * 2,
            "charged to the last checkpoint, and no further"
        );
        assert_eq!(
            store.get_usage(&entry_id, start.date_naive()).unwrap(),
            SNAPSHOT_INTERVAL * 2,
            "and that is what the ledger holds"
        );
    }

    /// The quota is what the child actually feels, so assert it directly: a
    /// power cycle must not hand the day's budget back.
    #[test]
    fn a_power_cut_does_not_refund_the_days_quota() {
        let quota = Duration::from_secs(30 * 60);
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 16, 0);
        let mut next_boot = power_cut_after(
            store,
            make_quota_policy(quota),
            "test-game",
            start,
            Duration::from_secs(10 * 60),
        );
        next_boot.recover_interrupted_session(on_day(27, 16, 20));

        assert_eq!(
            next_boot.list_entries(on_day(27, 16, 20))[0].max_run_if_started_now,
            Some(quota - Duration::from_secs(10 * 60)),
            "the ten minutes played before the power cut are gone from today"
        );
    }

    /// Recovery runs once. A second startup — the child power-cycling again,
    /// hoping the charge is re-applied to a session that no longer exists —
    /// finds nothing, and the ledger does not move.
    #[test]
    fn a_recovered_session_is_not_recovered_twice() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 16, 0);
        let mut next_boot = power_cut_after(
            store.clone(),
            make_test_policy(),
            "test-game",
            start,
            Duration::from_secs(90),
        );
        next_boot.recover_interrupted_session(on_day(27, 16, 5));
        let billed = store
            .get_usage(&EntryId::new("test-game"), start.date_naive())
            .unwrap();

        let mut third_boot = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );
        assert!(
            third_boot
                .recover_interrupted_session(on_day(27, 16, 10))
                .is_none(),
            "the checkpoint was cleared when it was settled"
        );
        assert_eq!(
            store
                .get_usage(&EntryId::new("test-game"), start.date_naive())
                .unwrap(),
            billed,
            "and nothing is billed a second time"
        );
    }

    /// A session that ended properly leaves no checkpoint behind, so a later
    /// start has nothing to recover and cannot double-bill it.
    #[test]
    fn a_session_that_ended_cleanly_leaves_nothing_to_recover() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );

        let start = on_day(27, 16, 0);
        let started = launch_at(&mut engine, "test-game", start);
        engine.tick(started, start);
        engine.end_current_session(
            Some(0),
            started + Duration::from_secs(60),
            start + chrono::Duration::seconds(60),
        );

        assert!(
            store
                .load_snapshot()
                .unwrap()
                .unwrap()
                .active_session
                .is_none(),
            "settling clears the checkpoint"
        );

        let mut next_boot = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );
        assert!(
            next_boot
                .recover_interrupted_session(on_day(27, 16, 5))
                .is_none()
        );
        assert_eq!(
            store
                .get_usage(&EntryId::new("test-game"), start.date_naive())
                .unwrap(),
            Duration::from_secs(60),
            "the clean end is billed exactly once"
        );
    }

    /// A power cut in the first seconds — before any interval has elapsed —
    /// still leaves a record that a session was open. Billing zero is the
    /// honest answer; leaving no trace at all is not.
    #[test]
    fn a_power_cut_in_the_first_seconds_still_leaves_a_record() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 16, 0);
        let mut next_boot = power_cut_after(
            store,
            make_test_policy(),
            "test-game",
            start,
            Duration::from_secs(2),
        );

        let recovered = next_boot
            .recover_interrupted_session(on_day(27, 16, 1))
            .expect("the session is recorded from its very first tick");
        assert_eq!(recovered.billed, Duration::ZERO);
    }

    /// Recovery bills the *start* day, like every other settlement (issue
    /// #170): a session cut off at 00:05 is still yesterday's play.
    #[test]
    fn a_recovered_session_is_billed_to_the_day_it_started() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 23, 50);
        let mut next_boot = power_cut_after(
            store.clone(),
            make_test_policy(),
            "test-game",
            start,
            SNAPSHOT_INTERVAL * 2,
        );
        // Booted after midnight, as it would be if the device stayed off.
        next_boot.recover_interrupted_session(on_day(28, 8, 0));

        let entry_id = EntryId::new("test-game");
        assert_eq!(
            store.get_usage(&entry_id, start.date_naive()).unwrap(),
            SNAPSHOT_INTERVAL * 2,
            "charged to the day the session started"
        );
        assert_eq!(
            store
                .get_usage(&entry_id, on_day(28, 8, 0).date_naive())
                .unwrap(),
            Duration::ZERO,
            "and not to the day the device came back"
        );
    }

    /// The recovery is visible, not silent: the audit log gets an end for the
    /// session, stamped when it was last seen alive rather than at boot.
    #[test]
    fn a_recovered_session_is_written_to_the_audit_log() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 16, 0);
        let mut next_boot = power_cut_after(
            store.clone(),
            make_test_policy(),
            "test-game",
            start,
            SNAPSHOT_INTERVAL,
        );
        next_boot.recover_interrupted_session(on_day(28, 8, 0));

        let ended = store
            .get_recent_audits(20)
            .unwrap()
            .into_iter()
            .find_map(|e| match e.event {
                AuditEventType::SessionEnded {
                    reason, duration, ..
                } => Some((reason, duration, e.timestamp)),
                _ => None,
            })
            .expect("the recovered session is recorded as an end");

        assert!(
            matches!(ended.0, SessionEndReason::Interrupted),
            "and as an interrupted one: {:?}",
            ended.0
        );
        assert_eq!(ended.1, SNAPSHOT_INTERVAL);
        assert!(
            ended.2 < on_day(28, 8, 0),
            "stamped when the session was last seen, not when the device came back"
        );
    }

    /// Checkpointing is a bound on loss, not a clock: it writes on the
    /// interval, not on every 100 ms tick.
    #[test]
    fn checkpoints_are_written_on_the_interval_not_every_tick() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );

        let start = on_day(27, 16, 0);
        let started = launch_at(&mut engine, "test-game", start);

        engine.tick(started, start);
        assert_eq!(
            engine_billable(&store),
            Some(Duration::ZERO),
            "the first tick of a session checkpoints it immediately"
        );

        // Ten seconds of ticks, well inside the interval, change nothing.
        for secs in 1..=10 {
            engine.tick(
                started + Duration::from_secs(secs),
                start + chrono::Duration::seconds(secs as i64),
            );
        }
        assert_eq!(
            engine_billable(&store),
            Some(Duration::ZERO),
            "no write until the interval is up"
        );

        engine.tick(
            started + SNAPSHOT_INTERVAL,
            start + chrono::Duration::from_std(SNAPSHOT_INTERVAL).unwrap(),
        );
        assert_eq!(
            engine_billable(&store),
            Some(SNAPSHOT_INTERVAL),
            "and one when it is"
        );
    }

    /// What lunchboxd's shutdown path now does with a live session (issue
    /// #201). It used to stop the activity and walk away, leaving the time
    /// unbilled — and since `Mod4+Shift+Escape` is `pkill -TERM lunchboxd` and
    /// sway's exit SIGHUPs the daemon into the same arm, that was reachable
    /// without touching the power button at all.
    #[test]
    fn a_shutdown_settles_the_session_it_stops() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut engine = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );

        let start = on_day(27, 16, 0);
        let started = launch_at(&mut engine, "test-game", start);

        assert!(matches!(
            engine.begin_stop(SessionEndReason::ServiceShutdown),
            BeginStopDecision::Stopping { .. }
        ));
        let settled = engine
            .finish_stop(
                started + Duration::from_secs(90),
                start + chrono::Duration::seconds(90),
            )
            .expect("the shutdown settles the session");

        assert!(matches!(settled.reason, SessionEndReason::ServiceShutdown));
        assert_eq!(
            store
                .get_usage(&EntryId::new("test-game"), start.date_naive())
                .unwrap(),
            Duration::from_secs(90),
            "a clean shutdown bills the time the child actually played"
        );
        assert!(
            store
                .load_snapshot()
                .unwrap()
                .unwrap()
                .active_session
                .is_none(),
            "and leaves no checkpoint for the next start to bill again"
        );
    }

    /// A checkpoint written by a different build is dropped, not guessed at.
    ///
    /// `billable` is a number produced by one version's billing rules. Charging
    /// a child for one produced by rules this build no longer runs would be
    /// wrong in a way nothing could see, so the row goes unbilled — and it has
    /// to actually *go*, or every future startup re-reads it.
    #[test]
    fn a_checkpoint_from_another_version_is_dropped_unbilled() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 16, 0);

        // A checkpoint that is perfectly readable and simply is not ours.
        let mut stale = StateSnapshot::new(
            start + chrono::Duration::minutes(5),
            Some(SessionSnapshot {
                session_id: SessionId::new(),
                entry_id: EntryId::new("test-game"),
                started_at: start,
                deadline: None,
                warnings_issued: vec![],
                billable: Duration::from_secs(5 * 60),
            }),
        );
        stale.version = SNAPSHOT_FORMAT + 1;
        store.save_snapshot(&stale).unwrap();

        let mut next_boot = CoreEngine::new(
            make_test_policy(),
            store.clone(),
            HostCapabilities::minimal(),
        );
        assert!(
            next_boot
                .recover_interrupted_session(on_day(27, 16, 10))
                .is_none(),
            "a format this build does not understand is not settled"
        );
        assert_eq!(
            store
                .get_usage(&EntryId::new("test-game"), start.date_naive())
                .unwrap(),
            Duration::ZERO,
            "and nothing it claimed is charged"
        );
        assert!(
            store
                .load_snapshot()
                .unwrap()
                .unwrap()
                .active_session
                .is_none(),
            "and it is dropped, so the next startup does not read it again"
        );
    }

    /// The version this build writes is the one it reads back, so an ordinary
    /// recovery is never mistaken for a foreign format.
    #[test]
    fn a_checkpoint_this_build_wrote_is_accepted() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let start = on_day(27, 16, 0);
        let mut next_boot = power_cut_after(
            store,
            make_test_policy(),
            "test-game",
            start,
            SNAPSHOT_INTERVAL,
        );

        let written = next_boot.store.load_snapshot().unwrap().unwrap();
        assert_eq!(written.version, SNAPSHOT_FORMAT);
        assert!(
            next_boot
                .recover_interrupted_session(on_day(27, 16, 5))
                .is_some()
        );
    }

    /// What the store currently believes the running session has billed.
    fn engine_billable(store: &SqliteStore) -> Option<Duration> {
        store
            .load_snapshot()
            .unwrap()
            .and_then(|s| s.active_session)
            .map(|s| s.billable)
    }
}
