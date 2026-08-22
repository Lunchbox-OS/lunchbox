//! The diagnostic registry: what is currently wrong with this device, for an
//! administrator rather than for the child (issue #143).
//!
//! Two kinds of condition live here, and the difference is whether the daemon
//! can ask again.
//!
//! **Probed** conditions are a pure function of the environment — is `yt-dlp`
//! installed, is there free disk, can the firewall helper run. The registry
//! recomputes the whole probed set on a sweep and replaces it wholesale, so a
//! condition that stops being true simply stops appearing. No clear() call has
//! to be remembered anywhere, which is the point: the log-only warnings this
//! replaces were computed once at daemon construction and never retracted, so
//! installing `yt-dlp` left the warning standing until the next restart.
//!
//! **Observed** conditions are raised at the moment something happens and are
//! not re-derivable — they are raised and cleared by hand. Phase 3.
//!
//! [`evaluate`] is deliberately pure and takes already-gathered facts. Every
//! probe rule is therefore testable without a filesystem, a subprocess, or a
//! policy — which matters, because the rules encode judgements ("only warn
//! about missing yt-dlp if something actually references YouTube") that are
//! easy to get subtly wrong and invisible when they are.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use chrono::{DateTime, Local};
use shepherd_api::EntryKind;
use shepherd_api::{
    Diagnostic, DiagnosticCode, DiagnosticSet, DiagnosticSeverity, DiagnosticSubject,
};
use shepherd_config::Policy;
use shepherd_host_linux::{FirewallEnforcementStatus, refresh_firewall_enforcement};
use shepherd_util::EntryId;

/// Identity of a raised condition. Mirrors [`Diagnostic::key`] but owned, so it
/// can key a map.
type Key = (DiagnosticCode, DiagnosticSubject);

/// Whether per-entry firewall enforcement works on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallFact {
    Enforceable,
    /// Carries the probe's own explanation, which already names the fix
    /// (install the helper, add the polkit rule).
    Unenforceable {
        reason: String,
    },
}

/// Free space against the configured floor for the media cache volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFact {
    pub free_bytes: u64,
    pub floor_bytes: u64,
    pub path: String,
}

/// Everything the probe rules need, gathered by the daemon so the rules
/// themselves stay pure.
#[derive(Debug, Clone, Default)]
pub struct ProbeFacts {
    /// `None` before the first probe — treated as "no opinion", not as
    /// "broken", so a probe that has not run yet raises nothing.
    pub firewall: Option<FirewallFact>,
    /// Entries that configure a firewall *and* whose kind can actually be
    /// firewalled. Steam entries are excluded by the caller: a firewall on a
    /// Steam entry is unsupported by design, which is a config error rather
    /// than a host problem (see `validation.rs`).
    pub firewalled_entries: Vec<EntryId>,
    /// Media entries referencing YouTube, whether by playlist URL or by a
    /// source inside their library.
    pub youtube_entries: Vec<EntryId>,
    /// Whether `yt-dlp` can be run. Only consulted when `youtube_entries` is
    /// non-empty.
    pub ytdlp_available: bool,
    /// `None` when there is no media cache to check.
    pub media_cache_disk: Option<DiskFact>,
    /// Whether a sound backend was detected.
    pub sound_backend_available: bool,
    /// Whether anything under `/dev/input` could be read. `None` before the
    /// first scan, same fail-quiet reasoning as `firewall`.
    pub input_devices_readable: Option<bool>,
    /// Whether any entry declares `requires_input`. Without one, unreadable
    /// input devices affect nothing and are not worth an administrator's
    /// attention.
    pub any_entry_requires_input: bool,
    /// Entries that set a browser policy their kind cannot apply.
    pub browser_policy_ignored_entries: Vec<EntryId>,
}

/// Compute the probed diagnostics implied by `facts`.
///
/// Pure: same facts, same diagnostics. `now` is passed rather than read so the
/// caller controls time and the rules stay testable.
pub fn evaluate(facts: &ProbeFacts, now: DateTime<Local>) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    // Firewall. The host-wide cause and the per-activity consequence are
    // separate diagnostics on purpose: the first tells an admin what to fix,
    // the second names the activities their child has lost meanwhile.
    if let Some(FirewallFact::Unenforceable { reason }) = &facts.firewall {
        out.push(Diagnostic {
            code: DiagnosticCode::FirewallUnenforceable,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Critical,
            message: "Per-entry firewall enforcement is not available on this host".to_string(),
            remedy: Some(reason.clone()),
            since: now,
        });
        for entry_id in &facts.firewalled_entries {
            out.push(Diagnostic {
                code: DiagnosticCode::FirewallNotApplied,
                subject: DiagnosticSubject::Entry {
                    entry_id: entry_id.clone(),
                },
                severity: DiagnosticSeverity::Critical,
                message: "This activity configures a firewall that cannot be applied, so it \
                          will not launch"
                    .to_string(),
                remedy: Some(
                    "Fix firewall enforcement on this host, or remove `[entries.firewall]` \
                     from this activity."
                        .to_string(),
                ),
                since: now,
            });
        }
    }

    // yt-dlp. Only a problem if something actually references YouTube —
    // otherwise every device without it would report a fault it does not have.
    if !facts.youtube_entries.is_empty() && !facts.ytdlp_available {
        out.push(Diagnostic {
            code: DiagnosticCode::YtDlpMissing,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "yt-dlp is not installed, but {} media {} reference YouTube and will fail \
                 to load or play",
                facts.youtube_entries.len(),
                if facts.youtube_entries.len() == 1 {
                    "activity"
                } else {
                    "activities"
                },
            ),
            remedy: Some("Install it with `shepherd-admin media-deps install`.".to_string()),
            since: now,
        });
    }

    // Media cache disk. The cache cap bounds the cache, not the volume it sits
    // on, so a full disk is the admin's problem and not something prefetch can
    // solve by evicting.
    if let Some(disk) = &facts.media_cache_disk
        && disk.floor_bytes > 0
        && disk.free_bytes < disk.floor_bytes
    {
        out.push(Diagnostic {
            code: DiagnosticCode::MediaCacheDiskLow,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "Media prefetch is paused: {} MB free on {} is below the {} MB floor",
                disk.free_bytes / (1024 * 1024),
                disk.path,
                disk.floor_bytes / (1024 * 1024),
            ),
            remedy: Some(
                "Free space on that volume, or lower `service.media.free_space_floor_bytes`."
                    .to_string(),
            ),
            since: now,
        });
    }

    if !facts.sound_backend_available {
        out.push(Diagnostic {
            code: DiagnosticCode::NoSoundBackend,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Warning,
            message: "No sound backend was detected; volume control does nothing".to_string(),
            remedy: Some("Install and start PipeWire or PulseAudio.".to_string()),
            since: now,
        });
    }

    // A browser policy on a kind that cannot apply it. Static — derivable from
    // the policy alone — so it is probed rather than raised at launch: an
    // administrator should learn their setting does nothing before their child
    // opens the activity, not from a log line afterwards.
    for entry_id in &facts.browser_policy_ignored_entries {
        out.push(Diagnostic {
            code: DiagnosticCode::BrowserPolicyIgnored,
            subject: DiagnosticSubject::Entry {
                entry_id: entry_id.clone(),
            },
            severity: DiagnosticSeverity::Warning,
            message: "This activity sets a browser policy, but its kind cannot apply one, so \
                      the policy is ignored"
                .to_string(),
            remedy: Some(
                "Browser policy is supported only for the Chrome flatpak entry kind.".to_string(),
            ),
            since: now,
        });
    }

    // Input devices. Same "only if it matters" rule as yt-dlp: with no
    // input-gated entry, an unreadable /dev/input changes nothing.
    if facts.input_devices_readable == Some(false) && facts.any_entry_requires_input {
        out.push(Diagnostic {
            code: DiagnosticCode::InputDevicesUnavailable,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Warning,
            message: "No input devices are readable, so activities that require one cannot be \
                      shown"
                .to_string(),
            remedy: Some(
                "Add shepherdd's user to the `input` group (see docs/INSTALL.md).".to_string(),
            ),
            since: now,
        });
    }

    out
}

/// Gather the environment facts [`evaluate`] needs.
///
/// The impure half, deliberately separated so every rule above stays testable
/// without a filesystem or a subprocess. Runs off the reactor: the firewall
/// probe execs `pkcheck` and the input scan walks `/dev/input`.
pub async fn gather_facts(policy: &Policy, sound_backend_available: bool) -> ProbeFacts {
    // Re-probe rather than read the cache. This is the call that makes an
    // installed helper take effect without a daemon restart.
    let firewall = tokio::task::spawn_blocking(refresh_firewall_enforcement)
        .await
        .ok()
        .map(|status| match status {
            FirewallEnforcementStatus::Supported => FirewallFact::Enforceable,
            FirewallEnforcementStatus::Unsupported { reason } => {
                FirewallFact::Unenforceable { reason }
            }
        });

    let input_devices_readable = tokio::task::spawn_blocking(crate::input_devices::inputs_readable)
        .await
        .ok();

    let media = policy.service.media.clone();
    let media_cache_disk = tokio::task::spawn_blocking(move || {
        let dir = shepherd_media_cache::media_cache_dir("videos")?;
        let free_bytes = crate::media::free_space(&dir)?;
        Some(DiskFact {
            free_bytes,
            floor_bytes: media.free_space_floor_bytes,
            path: dir.display().to_string(),
        })
    })
    .await
    .ok()
    .flatten();

    let youtube_entries = crate::media::youtube_entry_ids(policy);
    let ytdlp_available = if youtube_entries.is_empty() {
        // Skip the probe entirely when nothing could care; it is the only
        // input whose absence is not itself interesting.
        true
    } else {
        tokio::task::spawn_blocking(shepherd_media_cache::ytdlp_available)
            .await
            .unwrap_or(false)
    };

    ProbeFacts {
        firewall,
        firewalled_entries: firewalled_entry_ids(policy),
        youtube_entries,
        ytdlp_available,
        media_cache_disk,
        sound_backend_available,
        input_devices_readable,
        any_entry_requires_input: policy.entries.iter().any(|e| !e.requires_input.is_empty()),
        browser_policy_ignored_entries: browser_policy_ignored_entry_ids(policy),
    }
}

/// Entries whose `[entries.browser]` will be ignored because their kind cannot
/// apply one. Only the supported Chrome flatpak can.
fn browser_policy_ignored_entry_ids(policy: &Policy) -> Vec<EntryId> {
    policy
        .entries
        .iter()
        .filter(|e| e.browser.is_some())
        .filter(|e| match &e.kind {
            EntryKind::Flatpak { app_id, .. } => {
                !shepherd_host_linux::is_supported_browser_flatpak(app_id)
            }
            _ => true,
        })
        .map(|e| e.id.clone())
        .collect()
}

/// Entries whose configured firewall could actually be applied on a working
/// host, and which therefore lose something when it cannot be.
///
/// Steam entries are excluded: a firewall there is ignored whatever the host
/// can do, because the adapter has no way to apply one to a Steam-launched
/// process. That is a static property of the configuration, so it belongs to
/// config validation rather than here — and treating it as a host problem would
/// block the activity forever for a mistake no host change could fix.
fn firewalled_entry_ids(policy: &Policy) -> Vec<EntryId> {
    policy
        .entries
        .iter()
        .filter(|e| e.firewall.is_some() && !matches!(e.kind, EntryKind::Steam { .. }))
        .map(|e| e.id.clone())
        .collect()
}

/// Holds what is currently wrong. Cheap to clone the set out of; the daemon
/// keeps one behind an `Arc`.
#[derive(Default)]
pub struct DiagnosticRegistry {
    probed: Mutex<Vec<Diagnostic>>,
    observed: Mutex<HashMap<Key, Diagnostic>>,
}

impl DiagnosticRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the probed set with a freshly computed one.
    ///
    /// Carries `since` forward for conditions present in both, so "since" means
    /// since the problem started rather than since it was last checked — the
    /// difference between "your firewall has been broken for three days" and
    /// "your firewall has been broken for an hour", on an hourly sweep.
    ///
    /// Returns whether anything changed, so a sweep that finds the same
    /// problems does not rebroadcast a snapshot to every client.
    pub fn replace_probed(&self, fresh: Vec<Diagnostic>) -> bool {
        let mut probed = self.probed.lock().expect("diagnostics lock");
        let previous: HashMap<Key, DateTime<Local>> = probed
            .iter()
            .map(|d| ((d.code, d.subject.clone()), d.since))
            .collect();

        let carried: Vec<Diagnostic> = fresh
            .into_iter()
            .map(|mut d| {
                if let Some(since) = previous.get(&(d.code, d.subject.clone())) {
                    d.since = *since;
                }
                d
            })
            .collect();

        if *probed == carried {
            return false;
        }
        *probed = carried;
        true
    }

    /// Raise an observed condition, or update one already raised. Preserves the
    /// original `since`, so re-raising does not reset the clock.
    pub fn raise(&self, diagnostic: Diagnostic) -> bool {
        let mut observed = self.observed.lock().expect("diagnostics lock");
        let key = (diagnostic.code, diagnostic.subject.clone());
        match observed.get(&key) {
            Some(existing) => {
                let updated = Diagnostic {
                    since: existing.since,
                    ..diagnostic
                };
                if *existing == updated {
                    return false;
                }
                observed.insert(key, updated);
                true
            }
            None => {
                observed.insert(key, diagnostic);
                true
            }
        }
    }

    /// Clear an observed condition. Returns whether anything was removed.
    pub fn clear(&self, code: DiagnosticCode, subject: &DiagnosticSubject) -> bool {
        self.observed
            .lock()
            .expect("diagnostics lock")
            .remove(&(code, subject.clone()))
            .is_some()
    }

    /// The current set, sorted and capped, as clients see it.
    pub fn current(&self) -> DiagnosticSet {
        let probed = self.probed.lock().expect("diagnostics lock").clone();
        let observed = self
            .observed
            .lock()
            .expect("diagnostics lock")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        DiagnosticSet::new(probed.into_iter().chain(observed).collect())
    }
}

/// Raises and clears observed conditions from wherever they are noticed, and
/// nudges the daemon to republish when something actually changed.
///
/// Observed conditions cannot be recomputed on a sweep — nothing can ask "did a
/// library fail to load an hour ago" — so they are raised at the site and
/// cleared when the same site next succeeds. The nudge is a channel rather than
/// a direct broadcast because the raise site is usually on a worker with no
/// access to the engine or the IPC server.
#[derive(Clone)]
pub struct DiagnosticPublisher {
    registry: Arc<DiagnosticRegistry>,
    changed: mpsc::UnboundedSender<()>,
}

impl DiagnosticPublisher {
    pub fn new(registry: Arc<DiagnosticRegistry>, changed: mpsc::UnboundedSender<()>) -> Self {
        Self { registry, changed }
    }

    pub fn raise(&self, diagnostic: Diagnostic) {
        if self.registry.raise(diagnostic) {
            let _ = self.changed.send(());
        }
    }

    pub fn clear(&self, code: DiagnosticCode, subject: &DiagnosticSubject) {
        if self.registry.clear(code, subject) {
            let _ = self.changed.send(());
        }
    }
}

impl shepherd_api::DiagnosticSink for DiagnosticPublisher {
    fn raise(&self, diagnostic: Diagnostic) {
        DiagnosticPublisher::raise(self, diagnostic);
    }

    fn clear(&self, code: DiagnosticCode, subject: &DiagnosticSubject) {
        DiagnosticPublisher::clear(self, code, subject);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Local> {
        DateTime::from_timestamp(secs, 0).unwrap().into()
    }

    fn entry(id: &str) -> EntryId {
        EntryId::new(id)
    }

    fn codes(diags: &[Diagnostic]) -> Vec<DiagnosticCode> {
        diags.iter().map(|d| d.code).collect()
    }

    fn healthy() -> ProbeFacts {
        ProbeFacts {
            firewall: Some(FirewallFact::Enforceable),
            sound_backend_available: true,
            input_devices_readable: Some(true),
            ..ProbeFacts::default()
        }
    }

    #[test]
    fn a_healthy_device_reports_nothing() {
        assert!(evaluate(&healthy(), at(0)).is_empty());
    }

    #[test]
    fn facts_not_yet_gathered_raise_nothing() {
        // The default has `firewall: None` and `input_devices_readable: None`.
        // A probe that has not run yet must not look like a failing one, or
        // every device would report faults during boot.
        let facts = ProbeFacts {
            sound_backend_available: true,
            ..ProbeFacts::default()
        };
        assert!(evaluate(&facts, at(0)).is_empty());
    }

    #[test]
    fn an_unenforceable_firewall_names_the_host_and_every_activity_it_costs() {
        let facts = ProbeFacts {
            firewall: Some(FirewallFact::Unenforceable {
                reason: "helper not installed".into(),
            }),
            firewalled_entries: vec![entry("browser"), entry("school")],
            ..healthy()
        };
        let diags = evaluate(&facts, at(0));
        assert_eq!(
            codes(&diags),
            vec![
                DiagnosticCode::FirewallUnenforceable,
                DiagnosticCode::FirewallNotApplied,
                DiagnosticCode::FirewallNotApplied,
            ]
        );
        assert!(
            diags
                .iter()
                .all(|d| d.severity == DiagnosticSeverity::Critical)
        );
        // The host-wide one carries the probe's own explanation as the remedy.
        assert_eq!(diags[0].remedy.as_deref(), Some("helper not installed"));
        // The per-entry ones name their entry, so a UI can badge the tile.
        let named: Vec<_> = diags[1..].iter().filter_map(|d| d.entry_id()).collect();
        assert_eq!(named, vec![&entry("browser"), &entry("school")]);
    }

    #[test]
    fn an_enforceable_firewall_costs_no_activity_however_many_use_it() {
        let facts = ProbeFacts {
            firewall: Some(FirewallFact::Enforceable),
            firewalled_entries: vec![entry("browser")],
            ..healthy()
        };
        assert!(evaluate(&facts, at(0)).is_empty());
    }

    #[test]
    fn missing_ytdlp_matters_only_when_something_references_youtube() {
        let unused = ProbeFacts {
            ytdlp_available: false,
            youtube_entries: vec![],
            ..healthy()
        };
        assert!(
            evaluate(&unused, at(0)).is_empty(),
            "a device that never plays YouTube is not missing anything"
        );

        let used = ProbeFacts {
            ytdlp_available: false,
            youtube_entries: vec![entry("kid-tv")],
            ..healthy()
        };
        assert_eq!(
            codes(&evaluate(&used, at(0))),
            vec![DiagnosticCode::YtDlpMissing]
        );
    }

    #[test]
    fn the_ytdlp_message_counts_the_activities() {
        let one = ProbeFacts {
            youtube_entries: vec![entry("a")],
            ..healthy()
        };
        let two = ProbeFacts {
            youtube_entries: vec![entry("a"), entry("b")],
            ..healthy()
        };
        assert!(
            evaluate(&one, at(0))[0]
                .message
                .contains("1 media activity ")
        );
        assert!(
            evaluate(&two, at(0))[0]
                .message
                .contains("2 media activities ")
        );
    }

    #[test]
    fn a_disk_floor_of_zero_disables_the_check() {
        // 0 is the documented "disabled" spelling for the floor, so an empty
        // disk must not raise when the admin has turned the check off.
        let facts = ProbeFacts {
            media_cache_disk: Some(DiskFact {
                free_bytes: 0,
                floor_bytes: 0,
                path: "/var/cache".into(),
            }),
            ..healthy()
        };
        assert!(evaluate(&facts, at(0)).is_empty());
    }

    #[test]
    fn disk_at_the_floor_is_not_below_it() {
        let facts = ProbeFacts {
            media_cache_disk: Some(DiskFact {
                free_bytes: 2_000,
                floor_bytes: 2_000,
                path: "/var/cache".into(),
            }),
            ..healthy()
        };
        assert!(evaluate(&facts, at(0)).is_empty());

        let below = ProbeFacts {
            media_cache_disk: Some(DiskFact {
                free_bytes: 1_999,
                floor_bytes: 2_000,
                path: "/var/cache".into(),
            }),
            ..healthy()
        };
        assert_eq!(
            codes(&evaluate(&below, at(0))),
            vec![DiagnosticCode::MediaCacheDiskLow]
        );
    }

    #[test]
    fn unreadable_inputs_matter_only_when_an_activity_requires_one() {
        let nothing_needs_them = ProbeFacts {
            input_devices_readable: Some(false),
            any_entry_requires_input: false,
            ..healthy()
        };
        assert!(evaluate(&nothing_needs_them, at(0)).is_empty());

        let something_does = ProbeFacts {
            input_devices_readable: Some(false),
            any_entry_requires_input: true,
            ..healthy()
        };
        assert_eq!(
            codes(&evaluate(&something_does, at(0))),
            vec![DiagnosticCode::InputDevicesUnavailable]
        );
    }

    #[test]
    fn a_browser_policy_that_cannot_be_applied_is_reported_per_activity() {
        // Static, so it is probed: an admin learns the setting does nothing
        // before the child opens the activity, not from a log line afterwards.
        let facts = ProbeFacts {
            browser_policy_ignored_entries: vec![entry("school"), entry("homework")],
            ..healthy()
        };
        let diags = evaluate(&facts, at(0));
        assert_eq!(
            codes(&diags),
            vec![
                DiagnosticCode::BrowserPolicyIgnored,
                DiagnosticCode::BrowserPolicyIgnored
            ]
        );
        let named: Vec<_> = diags.iter().filter_map(|d| d.entry_id()).collect();
        assert_eq!(named, vec![&entry("school"), &entry("homework")]);
    }

    #[test]
    fn an_unreadable_library_is_raised_per_activity_and_clears_on_recovery() {
        // The observed half: nothing can ask "did this fail an hour ago", so
        // the site that notices the failure clears it when it next succeeds.
        let reg = DiagnosticRegistry::new();
        let subject = DiagnosticSubject::Entry {
            entry_id: entry("movies"),
        };
        let raise = |msg: &str| Diagnostic {
            code: DiagnosticCode::MediaLibraryUnreadable,
            subject: subject.clone(),
            severity: DiagnosticSeverity::Warning,
            message: msg.to_string(),
            remedy: None,
            since: at(1_000),
        };

        assert!(reg.raise(raise("gone")));
        assert_eq!(reg.current().items.len(), 1);

        // A different activity's failure is a separate condition, not an
        // update to this one.
        assert!(reg.raise(Diagnostic {
            subject: DiagnosticSubject::Entry {
                entry_id: entry("shows"),
            },
            ..raise("also gone")
        }));
        assert_eq!(reg.current().items.len(), 2);

        assert!(reg.clear(DiagnosticCode::MediaLibraryUnreadable, &subject));
        assert_eq!(
            reg.current().items.len(),
            1,
            "clearing one activity must not clear the other"
        );
    }

    #[test]
    fn a_sweep_that_finds_the_same_problems_does_not_report_a_change() {
        // What stops an hourly sweep rebroadcasting a snapshot to every client
        // for no reason.
        let reg = DiagnosticRegistry::new();
        let facts = ProbeFacts {
            sound_backend_available: false,
            ..healthy()
        };
        assert!(reg.replace_probed(evaluate(&facts, at(1_000))));
        assert!(!reg.replace_probed(evaluate(&facts, at(2_000))));
    }

    #[test]
    fn a_persisting_problem_keeps_the_time_it_started() {
        // "Since" must mean since the problem began, not since the last sweep,
        // or an hourly probe would make every fault look an hour old forever.
        let reg = DiagnosticRegistry::new();
        let facts = ProbeFacts {
            sound_backend_available: false,
            ..healthy()
        };
        reg.replace_probed(evaluate(&facts, at(1_000)));
        reg.replace_probed(evaluate(&facts, at(9_999)));
        assert_eq!(reg.current().items[0].since, at(1_000));
    }

    #[test]
    fn a_fixed_problem_disappears_without_anyone_clearing_it() {
        // The whole point of the probed/observed split: nothing has to remember
        // to retract this.
        let reg = DiagnosticRegistry::new();
        let broken = ProbeFacts {
            sound_backend_available: false,
            ..healthy()
        };
        reg.replace_probed(evaluate(&broken, at(1_000)));
        assert!(!reg.current().is_empty());

        assert!(reg.replace_probed(evaluate(&healthy(), at(2_000))));
        assert!(reg.current().is_empty());
    }

    #[test]
    fn an_observed_condition_is_raised_once_and_cleared_explicitly() {
        let reg = DiagnosticRegistry::new();
        let subject = DiagnosticSubject::Entry {
            entry_id: entry("movies"),
        };
        let diag = Diagnostic {
            code: DiagnosticCode::MediaLibraryUnreadable,
            subject: subject.clone(),
            severity: DiagnosticSeverity::Warning,
            message: "no".into(),
            remedy: None,
            since: at(1_000),
        };

        assert!(reg.raise(diag.clone()), "first raise is a change");
        assert!(!reg.raise(diag.clone()), "re-raising the same is not");
        assert_eq!(reg.current().items.len(), 1);

        assert!(reg.clear(DiagnosticCode::MediaLibraryUnreadable, &subject));
        assert!(reg.current().is_empty());
        assert!(
            !reg.clear(DiagnosticCode::MediaLibraryUnreadable, &subject),
            "clearing what is not raised changes nothing"
        );
    }

    #[test]
    fn re_raising_an_observed_condition_does_not_reset_its_clock() {
        let reg = DiagnosticRegistry::new();
        let base = Diagnostic {
            code: DiagnosticCode::BlePairingAgentUnavailable,
            subject: DiagnosticSubject::Service,
            severity: DiagnosticSeverity::Warning,
            message: "first".into(),
            remedy: None,
            since: at(1_000),
        };
        reg.raise(base.clone());
        assert!(reg.raise(Diagnostic {
            message: "second".into(),
            since: at(5_000),
            ..base
        }));

        let items = reg.current().items;
        assert_eq!(items[0].message, "second", "the newer detail wins");
        assert_eq!(items[0].since, at(1_000), "but the clock does not restart");
    }

    #[test]
    fn probed_and_observed_conditions_share_one_set() {
        let reg = DiagnosticRegistry::new();
        reg.replace_probed(evaluate(
            &ProbeFacts {
                sound_backend_available: false,
                ..healthy()
            },
            at(1_000),
        ));
        reg.raise(Diagnostic {
            code: DiagnosticCode::BrowserPolicyIgnored,
            subject: DiagnosticSubject::Entry {
                entry_id: entry("school"),
            },
            severity: DiagnosticSeverity::Info,
            message: "ignored".into(),
            remedy: None,
            since: at(1_000),
        });

        let set = reg.current();
        assert_eq!(set.items.len(), 2);
        // Sorted by severity: the Warning outranks the Info.
        assert_eq!(
            codes(&set.items),
            vec![
                DiagnosticCode::NoSoundBackend,
                DiagnosticCode::BrowserPolicyIgnored
            ]
        );
    }
}
