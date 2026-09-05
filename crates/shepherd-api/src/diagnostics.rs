//! Administrator-facing conditions: things that are wrong with the device
//! right now and that somebody could fix (issue #143).
//!
//! Distinct from the *other* two things this codebase calls a warning — the
//! time-limit warning shown to the child before their session expires
//! ([`crate::WarningSeverity`], `AuditEventType::WarningIssued`). Those are
//! aimed at the person using the device; these are aimed at the person who
//! configured it.
//!
//! **State, not events.** A diagnostic is a condition that is currently true
//! and later stops being true, so the daemon holds a set and clients read it —
//! rather than an append-only stream they would have to reconcile. It travels
//! on [`crate::ServiceStateSnapshot`], so every client has the current set on
//! subscribe, and a `DiagnosticsChanged` event carries deltas.
//!
//! Identity is `(code, subject)`. Raising the same pair twice updates in place,
//! which is what lets this replace the four incompatible de-duplication
//! mechanisms the tree grew (a `warned` bool, an `AtomicBool` swap, an
//! `attempts % 10` throttle, a `reported` HashSet) — and the per-launch
//! warnings that had none at all.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use shepherd_util::EntryId;

/// Upper bound on how many diagnostics travel on a snapshot.
///
/// [`crate::ServiceStateSnapshot`] already carries every `EntryView` and is
/// delivered over BLE, where a frame caps at 16 KiB
/// (`shepherd_ble::protocol::MAX_FRAME_BYTES`). An unbounded list here is the
/// one field that could push a snapshot past that, so it is capped and the
/// overflow is reported rather than silently dropped. A real device raising
/// even ten of these is in trouble; 32 is headroom, not a target.
pub const MAX_DIAGNOSTICS: usize = 32;

/// What is wrong. An enum rather than a string so the UIs can special-case
/// presentation and the wire drift test covers the variant set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    /// Per-entry firewall enforcement is unavailable on this host — the helper
    /// is not installed, or polkit denies it.
    FirewallUnenforceable,
    /// This entry configures a firewall that cannot be applied, so it will not
    /// launch. Distinct from [`Self::FirewallUnenforceable`], which is the
    /// host-wide cause: this one names an activity the child has lost.
    FirewallNotApplied,
    /// shepherd cannot talk to the compositor, so it cannot see what is on
    /// screen. The escape sweep closes nothing and no orphaned window is
    /// reported, which is indistinguishable from a clear screen unless it is
    /// said out loud (issue #147).
    CompositorUnreachable,
    /// The compositor's IPC socket is still reachable by every process at this
    /// uid, because hardening it failed (issue #144).
    ///
    /// The session is deliberately left running — an unhardened kiosk beats no
    /// kiosk — so nothing else about the device looks wrong. Without this the
    /// only trace is one log line, and a device ships without a protection it
    /// is configured to have.
    CompositorNotHardened,
    /// Something replaced or removed shepherdd's management socket, so the
    /// daemon is no longer reachable at the path its clients use (issue #144).
    ///
    /// An activity can do this: the socket lives in a directory owned by the
    /// uid every activity runs as, and no file mode prevents it — a root-owned
    /// directory stops shepherdd binding at all, and the sticky bit restricts
    /// deletion to the file's owner, which an activity is. Clients refuse to
    /// talk to whatever bound the name instead, so this is a denial rather than
    /// a breach; without saying so, it looks like a launcher that stopped
    /// working for no reason.
    IpcSocketReplaced,
    /// shepherdd's own management socket is reachable by processes that are
    /// not part of the session — the peer allow-list is not armed, or it is
    /// armed somewhere it cannot mean anything (issue #144).
    ///
    /// Like [`Self::CompositorNotHardened`], the session is deliberately left
    /// running, so nothing else about the device looks wrong and the downgrade
    /// is invisible unless it is said out loud.
    IpcSocketNotHardened,
    /// Something at this uid tried to drive the daemon from outside the
    /// session and was refused (issue #144). Worth an administrator's
    /// attention: an activity probing the management socket is not something
    /// that happens by accident.
    IpcPeerRejected,
    /// This entry sets a browser policy that its kind does not support, so the
    /// policy is ignored.
    BrowserPolicyIgnored,
    /// A media activity references YouTube but `yt-dlp` is not installed.
    YtDlpMissing,
    /// Free space on the media cache volume is below the configured floor, so
    /// prefetch has stopped.
    MediaCacheDiskLow,
    /// A media library could not be read or parsed.
    MediaLibraryUnreadable,
    /// No sound backend was detected; volume control does nothing.
    NoSoundBackend,
    /// The sound backend is present but its device topology could not be read,
    /// so which output is selected and which are plugged in are both unknown.
    /// Distinct from [`Self::NoSoundBackend`]: there *is* a backend, and the
    /// per-output volume limits are running on the last state seen rather than
    /// on what is true now.
    AudioTopologyUnreadable,
    /// No readable input devices, so input-gated entries cannot be evaluated.
    InputDevicesUnavailable,
    /// The BlueZ pairing agent could not be registered; a new phone will not be
    /// shown a pairing code.
    BlePairingAgentUnavailable,
    /// A RetroArch entry names a libretro core that is not installed, so the
    /// activity will not launch.
    RetroarchCoreMissing,
    /// A RetroArch entry's content — its ROM or disc image — is not there, so
    /// the activity will not launch.
    RetroarchContentMissing,
    /// An ebook entry's book is not there, so the activity opens on an error
    /// instead of a page.
    EbookBookMissing,
    /// An ebook entry's reader, or the backend for that book's format, is not
    /// installed. On Ubuntu the EPUB backend ships separately from Okular, so
    /// this is the likely first-run failure.
    EbookReaderMissing,
    /// An ebook entry lays the book out in pages on a device that has no way
    /// to turn one: a touchscreen and nothing else. Reading would stop at the
    /// end of the first page.
    EbookNoPageTurn,
}

/// What a diagnostic is about.
///
/// The split exists so a UI can render a per-entry problem on the entry itself,
/// next to the availability reasons already there, instead of only in a global
/// list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiagnosticSubject {
    /// The device as a whole.
    Service,
    /// One configured activity.
    Entry { entry_id: EntryId },
}

impl DiagnosticSubject {
    /// Sort key. `EntryId` is deliberately not `Ord` — ordering entry ids has
    /// no meaning anywhere else — so the one place that needs a stable order
    /// derives it here instead of adding trait impls to a shared type.
    /// `None` sorts first, putting device-wide problems above per-activity
    /// ones, which is also how a reader wants to read them.
    fn sort_key(&self) -> Option<&str> {
        match self {
            DiagnosticSubject::Service => None,
            DiagnosticSubject::Entry { entry_id } => Some(entry_id.as_str()),
        }
    }
}

/// How bad it is. Declaration order is the sort order: `Critical` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// The configuration claims a protection the device is not providing.
    /// Unmissable in both UIs.
    Critical,
    /// A feature is unavailable or degraded.
    Warning,
    /// Worth knowing; nothing is broken.
    Info,
}

/// One condition that is currently true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub subject: DiagnosticSubject,
    pub severity: DiagnosticSeverity,
    /// One line, for a person. Most of these already exist verbatim as the log
    /// message the diagnostic replaces.
    pub message: String,
    /// What to do about it, when there is a concrete answer — a command to run,
    /// a group to join. `None` when the fix is not something we can name.
    pub remedy: Option<String>,
    /// When this condition was first observed. Preserved across a re-raise, so
    /// "since" means since it started, not since it was last checked.
    pub since: DateTime<Local>,
}

impl Diagnostic {
    /// Identity for de-duplication: raising the same pair twice updates in
    /// place rather than accumulating.
    pub fn key(&self) -> (DiagnosticCode, &DiagnosticSubject) {
        (self.code, &self.subject)
    }

    /// The entry this concerns, if it concerns one. Lets a UI join diagnostics
    /// onto entries without matching on the subject enum.
    pub fn entry_id(&self) -> Option<&EntryId> {
        match &self.subject {
            DiagnosticSubject::Entry { entry_id } => Some(entry_id),
            DiagnosticSubject::Service => None,
        }
    }
}

/// The current set, as clients see it.
///
/// A struct rather than a bare `Vec` so the cap can report itself: a client
/// showing 32 of 40 problems while implying it is showing all of them would be
/// worse than showing none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiagnosticSet {
    /// Sorted most severe first, then by subject, then by code — a stable
    /// order, so a client diffing two snapshots sees real changes rather than
    /// reordering.
    pub items: Vec<Diagnostic>,
    /// Whether [`MAX_DIAGNOSTICS`] hid anything. UIs must say so.
    pub truncated: bool,
}

impl DiagnosticSet {
    /// Build a set from raised diagnostics, sorting and applying the cap.
    ///
    /// Truncation drops the *least* severe, because a set over the cap is
    /// exactly when the critical ones most need to survive.
    pub fn new(mut items: Vec<Diagnostic>) -> Self {
        items.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.subject.sort_key().cmp(&b.subject.sort_key()))
                .then_with(|| a.code.cmp(&b.code))
        });
        let truncated = items.len() > MAX_DIAGNOSTICS;
        items.truncate(MAX_DIAGNOSTICS);
        Self { items, truncated }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether anything here is severe enough to warrant an unmissable
    /// treatment rather than a list entry.
    pub fn has_critical(&self) -> bool {
        self.items
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Critical)
    }
}

/// Somewhere to report an administrator-facing condition from.
///
/// Exists because the code that notices a problem is usually not the code that
/// can publish one: the BLE server, the host adapter, and the daemon's workers
/// all sit in crates that know nothing about shepherdd's registry. They take
/// one of these instead, and the daemon supplies the implementation.
///
/// Deliberately fire-and-forget. A raise site is reporting something it has
/// already handled — it must not have to care whether anybody is listening, and
/// a diagnostic that fails to publish must never fail the operation that
/// noticed it.
pub trait DiagnosticSink: Send + Sync {
    fn raise(&self, diagnostic: Diagnostic);
    fn clear(&self, code: DiagnosticCode, subject: &DiagnosticSubject);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Local> {
        DateTime::from_timestamp(secs, 0).unwrap().into()
    }

    fn diag(code: DiagnosticCode, severity: DiagnosticSeverity) -> Diagnostic {
        Diagnostic {
            code,
            subject: DiagnosticSubject::Service,
            severity,
            message: format!("{code:?}"),
            remedy: None,
            since: at(1_000),
        }
    }

    fn entry_diag(code: DiagnosticCode, entry: &str) -> Diagnostic {
        Diagnostic {
            subject: DiagnosticSubject::Entry {
                entry_id: EntryId::new(entry),
            },
            ..diag(code, DiagnosticSeverity::Warning)
        }
    }

    #[test]
    fn the_most_severe_sorts_first() {
        let set = DiagnosticSet::new(vec![
            diag(DiagnosticCode::NoSoundBackend, DiagnosticSeverity::Info),
            diag(
                DiagnosticCode::FirewallUnenforceable,
                DiagnosticSeverity::Critical,
            ),
            diag(DiagnosticCode::YtDlpMissing, DiagnosticSeverity::Warning),
        ]);
        let order: Vec<_> = set.items.iter().map(|d| d.severity).collect();
        assert_eq!(
            order,
            vec![
                DiagnosticSeverity::Critical,
                DiagnosticSeverity::Warning,
                DiagnosticSeverity::Info
            ]
        );
    }

    #[test]
    fn the_order_is_stable_for_equal_severities() {
        // A client diffing two snapshots must see real changes, not shuffling.
        let build = || {
            DiagnosticSet::new(vec![
                entry_diag(DiagnosticCode::MediaLibraryUnreadable, "zebra"),
                entry_diag(DiagnosticCode::BrowserPolicyIgnored, "alpha"),
                entry_diag(DiagnosticCode::MediaLibraryUnreadable, "alpha"),
            ])
        };
        assert_eq!(build(), build());
        let subjects: Vec<_> = build()
            .items
            .iter()
            .map(|d| d.entry_id().unwrap().as_str().to_string())
            .collect();
        assert_eq!(subjects, vec!["alpha", "alpha", "zebra"]);
    }

    #[test]
    fn truncation_keeps_the_severe_ones_and_admits_itself() {
        // Over the cap is exactly when a critical diagnostic most needs to
        // survive, so the cap spends the least severe first.
        let mut items: Vec<_> = (0..MAX_DIAGNOSTICS)
            .map(|i| entry_diag(DiagnosticCode::MediaLibraryUnreadable, &format!("e{i:03}")))
            .collect();
        items.push(diag(
            DiagnosticCode::FirewallUnenforceable,
            DiagnosticSeverity::Critical,
        ));

        let set = DiagnosticSet::new(items);
        assert_eq!(set.items.len(), MAX_DIAGNOSTICS);
        assert!(
            set.truncated,
            "a UI must be able to say it is not showing all"
        );
        assert!(set.has_critical(), "the critical one survived the cap");
    }

    #[test]
    fn a_set_within_the_cap_is_not_truncated() {
        let set = DiagnosticSet::new(vec![diag(
            DiagnosticCode::YtDlpMissing,
            DiagnosticSeverity::Warning,
        )]);
        assert!(!set.truncated);
        assert!(!set.is_empty());
    }

    #[test]
    fn an_empty_set_is_the_default() {
        let set = DiagnosticSet::default();
        assert!(set.is_empty());
        assert!(!set.truncated);
        assert!(!set.has_critical());
    }

    #[test]
    fn identity_is_the_code_and_the_subject() {
        let a = entry_diag(DiagnosticCode::MediaLibraryUnreadable, "movies");
        let b = entry_diag(DiagnosticCode::MediaLibraryUnreadable, "movies");
        let other_entry = entry_diag(DiagnosticCode::MediaLibraryUnreadable, "shows");
        let other_code = entry_diag(DiagnosticCode::BrowserPolicyIgnored, "movies");

        assert_eq!(a.key(), b.key());
        assert_ne!(
            a.key(),
            other_entry.key(),
            "same problem, different activity"
        );
        assert_ne!(
            a.key(),
            other_code.key(),
            "same activity, different problem"
        );
    }

    #[test]
    fn a_service_diagnostic_names_no_entry() {
        assert!(
            diag(DiagnosticCode::NoSoundBackend, DiagnosticSeverity::Warning)
                .entry_id()
                .is_none()
        );
    }
}
