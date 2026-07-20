//! Core policy engine

use chrono::{DateTime, Local, NaiveDate};
use shepherd_api::{
    API_VERSION, EntryKindTag, EntryView, InputDeviceType, InternetStatusView, ReasonCode,
    ServiceStateSnapshot, SessionEndReason, WarningSeverity,
};
use shepherd_config::{Entry, InternetCheckTarget, Policy};
use shepherd_host_api::{HostCapabilities, HostSessionHandle};
use shepherd_store::{AuditEvent, AuditEventType, Store};
use shepherd_util::{EntryId, MonotonicInstant, SessionId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

use crate::{ActiveSession, CoreEvent, SessionPlan, StopResult};

/// Launch decision from the core engine
#[derive(Debug)]
pub enum LaunchDecision {
    Approved(SessionPlan),
    Denied { reasons: Vec<ReasonCode> },
}

/// Stop decision from the core engine
#[derive(Debug)]
pub enum StopDecision {
    Stopped(StopResult),
    NoActiveSession,
}

/// The core policy engine
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
            current_session: None,
            last_availability_set: HashSet::new(),
            internet_status: HashMap::new(),
            kind_readiness: HashMap::new(),
            connected_inputs: None,
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

    /// Evaluate a single entry for availability
    fn evaluate_entry(&self, entry: &Entry, now: DateTime<Local>) -> EntryView {
        let today = now.date_naive();
        let daily_override = self
            .store
            .get_daily_override(&entry.id, today)
            .ok()
            .flatten();

        // If manually disabled by a parent override, short-circuit all other checks
        if daily_override.as_ref().and_then(|o| o.availability) == Some(false) {
            return EntryView {
                entry_id: entry.id.clone(),
                label: entry.label.clone(),
                icon_ref: entry.icon_ref.clone(),
                kind_tag: entry.kind.tag(),
                enabled: false,
                reasons: vec![ReasonCode::ManuallyDisabled { until: today }],
                max_run_if_started_now: None,
            };
        }

        let manually_enabled = daily_override.as_ref().and_then(|o| o.availability) == Some(true);
        let quota_delta = daily_override.as_ref().and_then(|o| o.quota_delta_seconds);

        let mut reasons = Vec::new();
        let mut enabled = true;

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
                next_window_start: None, // TODO: compute next window
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

        // Check if another session is active
        if let Some(session) = &self.current_session {
            enabled = false;
            reasons.push(ReasonCode::SessionActive {
                entry_id: session.plan.entry_id.clone(),
                remaining: session.time_remaining(MonotonicInstant::now()),
            });
        }

        // Check cooldown
        if let Ok(Some(until)) = self.store.get_cooldown_until(&entry.id)
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
            let balance = self.token_balance(entry, today);
            if !tokens.unlocked(balance) {
                enabled = false;
                reasons.push(ReasonCode::TokensInsufficient {
                    balance,
                    required: tokens.minimum,
                });
            }
        }

        // Calculate max run if enabled (None when disabled, Some(None) flattened for unlimited)
        let max_run_if_started_now = if enabled {
            self.compute_max_duration(entry, now, quota_delta, manually_enabled)
        } else {
            None
        };

        EntryView {
            entry_id: entry.id.clone(),
            label: entry.label.clone(),
            icon_ref: entry.icon_ref.clone(),
            kind_tag,
            enabled,
            reasons,
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
        manually_enabled: bool,
    ) -> Option<Duration> {
        let mut max = entry.limits.max_run;

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
            let balance = self.token_balance(entry, now.date_naive());
            max = Some(match max {
                Some(m) => m.min(balance),
                None => balance,
            });
        }

        max
    }

    /// An entry's banked token balance, or zero if it is not token-gated.
    fn token_balance(&self, entry: &Entry, today: NaiveDate) -> Duration {
        let Some(tokens) = &entry.tokens else {
            return Duration::ZERO;
        };
        self.store
            .get_token_balance(&entry.id, today, tokens.carry_over)
            .unwrap_or(Duration::ZERO)
    }

    /// Whether a force-enable daily override is in effect for an entry.
    fn manually_enabled(&self, entry_id: &EntryId, today: NaiveDate) -> bool {
        self.store
            .get_daily_override(entry_id, today)
            .ok()
            .flatten()
            .and_then(|o| o.availability)
            == Some(true)
    }

    /// Settle token balances after a session on `ended_entry_id` of `duration`
    /// (issue #8): every entry gated on it banks time, and if it is itself
    /// token-gated it spends its own balance down.
    ///
    /// An entry can legitimately be both a source and a target, in which case
    /// both halves apply.
    fn settle_tokens(&self, ended_entry_id: &EntryId, duration: Duration, today: NaiveDate) {
        for target in &self.policy.entries {
            let Some(tokens) = &target.tokens else {
                continue;
            };

            // Earn: this session was on one of the target's source activities.
            if tokens.from.contains(ended_entry_id) {
                let earned = tokens.earned(duration);
                if !earned.is_zero() {
                    let balance = self
                        .store
                        .adjust_token_balance(
                            &target.id,
                            today,
                            tokens.carry_over,
                            earned.as_secs() as i64,
                        )
                        .unwrap_or(Duration::ZERO);

                    // Apply the ceiling here rather than in the store, which has
                    // no view of policy.
                    if let Some(max_balance) = tokens.max_balance
                        && balance > max_balance
                    {
                        let excess = (balance - max_balance).as_secs() as i64;
                        let _ = self.store.adjust_token_balance(
                            &target.id,
                            today,
                            tokens.carry_over,
                            -excess,
                        );
                    }
                }
            }

            // Spend: this session was on the gated entry itself. A session run
            // under a force-enable override is exempt — the caregiver granted
            // that time, so the child shouldn't be billed for it, consistent
            // with the override bypassing the gate in the first place.
            if &target.id == ended_entry_id && !self.manually_enabled(ended_entry_id, today) {
                let _ = self.store.adjust_token_balance(
                    &target.id,
                    today,
                    tokens.carry_over,
                    -(duration.as_secs() as i64),
                );
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

        event
    }

    /// Attach host handle to current session
    pub fn attach_host_handle(&mut self, handle: HostSessionHandle) {
        if let Some(session) = &mut self.current_session {
            session.attach_handle(handle);
        }
    }

    /// Tick the engine - check for warnings, expiry, and availability changes
    pub fn tick(&mut self, now_mono: MonotonicInstant, now: DateTime<Local>) -> Vec<CoreEvent> {
        let mut events = Vec::new();

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
            && session.state != shepherd_api::SessionState::Expiring
            && session.state != shepherd_api::SessionState::Ended
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

    /// Notify that a session has exited
    pub fn notify_session_exited(
        &mut self,
        exit_code: Option<i32>,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) -> Option<CoreEvent> {
        let session = self.current_session.take()?;

        let duration = session.duration_so_far(now_mono);
        let reason = if session.state == shepherd_api::SessionState::Expiring {
            SessionEndReason::Expired
        } else {
            SessionEndReason::ProcessExited { exit_code }
        };

        // Update usage accounting
        let today = now.date_naive();
        let _ = self
            .store
            .add_usage(&session.plan.entry_id, today, duration);

        // Settle token balances (issue #8)
        self.settle_tokens(&session.plan.entry_id, duration, today);

        // Set cooldown if configured
        if let Some(entry) = self.policy.get_entry(&session.plan.entry_id)
            && let Some(cooldown) = entry.limits.cooldown
        {
            let until = now + chrono::Duration::from_std(cooldown).unwrap();
            let _ = self.store.set_cooldown_until(&session.plan.entry_id, until);
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

        Some(CoreEvent::SessionEnded {
            session_id: session.plan.session_id,
            entry_id: session.plan.entry_id,
            reason,
            duration,
        })
    }

    /// Stop the current session
    pub fn stop_current(
        &mut self,
        reason: SessionEndReason,
        now_mono: MonotonicInstant,
        now: DateTime<Local>,
    ) -> StopDecision {
        let session = match self.current_session.take() {
            Some(s) => s,
            None => return StopDecision::NoActiveSession,
        };

        let duration = session.duration_so_far(now_mono);

        // Update usage accounting
        let today = now.date_naive();
        let _ = self
            .store
            .add_usage(&session.plan.entry_id, today, duration);

        // Settle token balances (issue #8)
        self.settle_tokens(&session.plan.entry_id, duration, today);

        // Set cooldown if configured
        if let Some(entry) = self.policy.get_entry(&session.plan.entry_id)
            && let Some(cooldown) = entry.limits.cooldown
        {
            let until = now + chrono::Duration::from_std(cooldown).unwrap();
            let _ = self.store.set_cooldown_until(&session.plan.entry_id, until);
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
            reason = ?reason,
            "Session stopped"
        );

        StopDecision::Stopped(StopResult {
            session_id: session.plan.session_id,
            entry_id: session.plan.entry_id,
            reason,
            duration,
        })
    }

    /// Get current service state snapshot
    pub fn get_state(&self) -> ServiceStateSnapshot {
        let current_session = self
            .current_session
            .as_ref()
            .map(|s| s.to_session_info(MonotonicInstant::now()));

        // Build entry views for the snapshot
        let entries = self.list_entries(shepherd_util::now());

        ServiceStateSnapshot {
            api_version: API_VERSION,
            policy_loaded: true,
            current_session,
            entry_count: self.policy.entries.len(),
            entries,
            internet_status: self.internet_status_views(),
        }
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
    use shepherd_api::EntryKind;
    use shepherd_config::{AvailabilityPolicy, Entry, LimitsPolicy, TokensPolicy};
    use shepherd_store::SqliteStore;
    use std::collections::HashMap;

    fn make_test_policy() -> Policy {
        Policy {
            service: Default::default(),
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
        }
    }

    #[test]
    fn test_list_entries() {
        let policy = make_test_policy();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let engine = CoreEngine::new(policy, store, caps);

        let entries = engine.list_entries(shepherd_util::now());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].enabled);
    }

    #[test]
    fn test_kind_readiness_gates_show_and_launch() {
        use shepherd_api::{EntryKindTag, ReasonCode};

        let policy = make_test_policy();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test-game");
        let now = shepherd_util::now();

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
        use shepherd_api::{InputDeviceType, ReasonCode};

        let mut policy = make_test_policy();
        policy.entries[0].requires_input = vec![InputDeviceType::Keyboard];
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test-game");
        let now = shepherd_util::now();

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
        let decision = engine.request_launch(&entry_id, shepherd_util::now());

        assert!(matches!(decision, LaunchDecision::Approved(_)));
    }

    #[test]
    fn test_session_blocks_new_launch() {
        let policy = make_test_policy();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test-game");
        let now = shepherd_util::now();
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
                },
                warnings: vec![shepherd_api::WarningThreshold {
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            service: Default::default(),
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test");
        let now = shepherd_util::now();
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
                },
                warnings: vec![shepherd_api::WarningThreshold {
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            service: Default::default(),
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test");
        let now = shepherd_util::now();
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            service: Default::default(),
            default_warnings: vec![],
            default_max_run: Some(Duration::from_secs(3600)),
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
        };

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let caps = HostCapabilities::minimal();
        let mut engine = CoreEngine::new(policy, store, caps);

        let entry_id = EntryId::new("test");
        let now = shepherd_util::now();
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
        use shepherd_api::ReasonCode;
        use shepherd_util::{DaysOfWeek, TimeWindow, WallClock};

        let entry_id = EntryId::new("time-restricted");
        let policy = Policy {
            service: Default::default(),
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
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
            .upsert_daily_override(&entry_id, today, Some(true), None)
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
        use shepherd_api::ReasonCode;
        use shepherd_util::{DaysOfWeek, TimeWindow, WallClock};

        let entry_id = EntryId::new("time-restricted");
        let policy = Policy {
            service: Default::default(),
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
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
            .upsert_daily_override(&entry_id, today, Some(false), None)
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
        use shepherd_api::ReasonCode;

        let entry_id = EntryId::new("config-disabled");
        let policy = Policy {
            service: Default::default(),
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
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
            .upsert_daily_override(&entry_id, today, Some(true), None)
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
        use shepherd_api::ReasonCode;

        let entry_id = EntryId::new("quota-limited");
        let policy = Policy {
            service: Default::default(),
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
                xwayland_native_resolution: false,
                confirm_on_close: true,
            }],
            default_warnings: vec![],
            default_max_run: None,
            volume: Default::default(),
            brightness: Default::default(),
            auto_brightness: Default::default(),
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
            .upsert_daily_override(&entry_id, today, Some(true), None)
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
            xwayland_native_resolution: false,
            confirm_on_close: true,
        }
    }

    /// Policy with two source activities ("scratch", "typing") and a
    /// token-gated target ("minecraft").
    fn make_token_policy(tokens: TokensPolicy) -> Policy {
        Policy {
            service: Default::default(),
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
        }
    }

    fn tokens_from(sources: &[&str]) -> TokensPolicy {
        TokensPolicy {
            from: sources.iter().map(|s| EntryId::new(*s)).collect(),
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
        engine.notify_session_exited(Some(0), started + duration, now);
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

    fn noon() -> DateTime<Local> {
        use chrono::TimeZone;
        chrono::Local
            .with_ymd_and_hms(2026, 4, 27, 12, 0, 0)
            .unwrap()
    }

    #[test]
    fn test_token_gate_locks_until_time_is_earned() {
        use shepherd_api::ReasonCode;

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
            .upsert_daily_override(&entry_id, now.date_naive(), Some(true), None)
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
}
