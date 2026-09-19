//! Per-library playback positions and the last-watched item.
//!
//! Both media front-ends can be told to remember where the viewer left off:
//! the Linux binary with `--resume`, the Android app with a per-library
//! "Resume playback" toggle. Both are **off by default** — with the option off
//! nothing here is constructed and no file is ever written.
//!
//! The state is per library (`library_id` on Linux, the settings entry id on
//! Android) and holds two things:
//!
//! - a position per item, so re-opening an item resumes where it stopped, and
//! - the id of the item watched most recently, so re-opening the *library* can
//!   offer to continue it.
//!
//! Positions are deliberately forgettable: an item watched to (near) its end
//! drops its entry, so the next play starts from the beginning rather than at
//! the credits, and an item stopped in its first few seconds never records one.
//!
//! This module is pure state + TOML persistence. Where the file lives is the
//! platform binary's decision (`$XDG_STATE_HOME/shepherd/media/resume/` on
//! Linux, app-private storage on Android), as is when to call [`ResumeStore`]'s
//! record/flush methods.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::settings::SettingsIoError;

/// Schema version of an on-disk resume file. Bump when the shape changes in a
/// backward-incompatible way.
pub const RESUME_SCHEMA_VERSION: u32 = 1;

/// Positions below this are not worth restoring — the viewer barely started, and
/// resuming 4 seconds in is more confusing than starting over.
pub const MIN_RESUME_SECONDS: f64 = 20.0;

/// A stop this close to the end counts as "finished": the entry is dropped so
/// the next play starts from the beginning instead of the closing seconds.
pub const NEAR_END_SECONDS: f64 = 30.0;

/// How often [`ResumeStore::flush_if_due`] is willing to write while an item
/// plays. Frequent enough that a power cut loses seconds, rare enough that it
/// costs nothing on a TV's flash storage.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(10);

/// Where one item was left off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemPosition {
    /// Seconds from the start of the item.
    pub position_seconds: f64,
    /// The item's total length when known, so a later read can tell a
    /// near-the-end position from a near-the-start one without the player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
}

/// One library's resume state: the last item watched and a position per item.
///
/// Item ids are `[a-z0-9-]+` (enforced by `lunchbox-media-core`'s library
/// parser), so they are safe as TOML table keys. The map is a `BTreeMap` to keep
/// the file's key order stable across writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeState {
    pub schema_version: u32,
    /// The item played most recently, whether or not it has a saved position
    /// (it may have been watched to the end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_item: Option<String>,
    #[serde(default)]
    pub positions: BTreeMap<String, ItemPosition>,
}

impl Default for ResumeState {
    fn default() -> Self {
        Self {
            schema_version: RESUME_SCHEMA_VERSION,
            last_item: None,
            positions: BTreeMap::new(),
        }
    }
}

impl ResumeState {
    /// Fresh, empty state at the current schema version.
    pub fn new() -> Self {
        Self::default()
    }

    /// The saved position for `item_id`, if one is worth resuming from.
    pub fn position(&self, item_id: &str) -> Option<f64> {
        self.positions.get(item_id).map(|p| p.position_seconds)
    }

    /// The item watched most recently, if it is still one of `known_ids`.
    /// A library whose contents changed (a playlist that dropped a video)
    /// must not offer to resume something that is no longer there.
    pub fn last_item_in<'a, I>(&self, known_ids: I) -> Option<&str>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let last = self.last_item.as_deref()?;
        known_ids.into_iter().any(|id| id == last).then_some(last)
    }

    /// Record that `item_id` is now playing. Does not touch its position — that
    /// is what [`record`](Self::record) is for.
    pub fn note_started(&mut self, item_id: &str) {
        self.last_item = Some(item_id.to_string());
    }

    /// Record where `item_id` currently is.
    ///
    /// Applies the forget-it policy: a position inside the first
    /// [`MIN_RESUME_SECONDS`], or within [`NEAR_END_SECONDS`] of a known
    /// duration, removes the entry instead of storing it. A non-finite or
    /// negative position is ignored outright (mpv reports no position at all
    /// between files, and a caller polling every frame will see that).
    pub fn record(&mut self, item_id: &str, position_seconds: f64, duration_seconds: Option<f64>) {
        if !position_seconds.is_finite() || position_seconds < 0.0 {
            return;
        }
        let near_end = duration_seconds
            .filter(|d| d.is_finite() && *d > 0.0)
            .is_some_and(|d| position_seconds >= d - NEAR_END_SECONDS);
        if position_seconds < MIN_RESUME_SECONDS || near_end {
            self.positions.remove(item_id);
            return;
        }
        self.positions.insert(
            item_id.to_string(),
            ItemPosition {
                position_seconds,
                duration_seconds: duration_seconds.filter(|d| d.is_finite() && *d > 0.0),
            },
        );
    }

    /// Drop positions for items the library no longer contains, so a file can't
    /// grow without bound as a playlist churns. The last-watched item is kept
    /// even when absent — [`last_item_in`](Self::last_item_in) filters it at
    /// read time, and a temporarily unreachable library shouldn't lose it.
    pub fn retain_known<'a, I>(&mut self, known_ids: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        let known: std::collections::HashSet<&str> = known_ids.into_iter().collect();
        self.positions.retain(|id, _| known.contains(id.as_str()));
    }

    /// Reject a file this build can't read.
    pub fn validate(&self) -> Result<(), SettingsIoError> {
        if self.schema_version != RESUME_SCHEMA_VERSION {
            return Err(SettingsIoError::Invalid(
                crate::settings::SettingsError::UnsupportedSchema {
                    actual: self.schema_version,
                    supported: RESUME_SCHEMA_VERSION,
                },
            ));
        }
        Ok(())
    }
}

/// A [`ResumeState`] bound to a file, with write batching.
///
/// The front-ends poll the player's position every frame; writing that through
/// would hammer the disk, so mutations only mark the store dirty and
/// [`flush_if_due`](Self::flush_if_due) writes at most once per
/// [`SAVE_INTERVAL`]. [`flush`](Self::flush) forces a write for the moments that
/// matter (playback ended, the process is going away).
pub struct ResumeStore {
    path: PathBuf,
    state: ResumeState,
    dirty: bool,
    last_saved: Option<Instant>,
    /// Set once a write fails so a broken path (read-only storage, missing
    /// parent) doesn't log on every flush.
    write_failed: bool,
}

impl ResumeStore {
    /// Load the store for `path`. A missing or unreadable file yields empty
    /// state — resume is a convenience, and losing it must never stop playback.
    /// The `Err` case is reserved for a file that parsed but this build cannot
    /// interpret, so a caller can surface it.
    pub fn load(path: PathBuf) -> Result<Self, SettingsIoError> {
        let state = match std::fs::read_to_string(&path) {
            Ok(body) => {
                let state: ResumeState =
                    toml::from_str(&body).map_err(|e| SettingsIoError::Parse {
                        path: path.clone(),
                        source: e,
                    })?;
                state.validate()?;
                state
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ResumeState::new(),
            Err(e) => {
                return Err(SettingsIoError::Read {
                    path: path.clone(),
                    source: e,
                });
            }
        };
        Ok(Self {
            path,
            state,
            dirty: false,
            last_saved: None,
            write_failed: false,
        })
    }

    /// Load `path`, falling back to empty state on any error. Resume state is
    /// disposable; a corrupt file should cost the viewer their bookmarks, not
    /// their video. Returns the error alongside so the caller can log it.
    pub fn load_or_empty(path: PathBuf) -> (Self, Option<SettingsIoError>) {
        match Self::load(path.clone()) {
            Ok(store) => (store, None),
            Err(e) => (
                Self {
                    path,
                    state: ResumeState::new(),
                    dirty: false,
                    last_saved: None,
                    write_failed: false,
                },
                Some(e),
            ),
        }
    }

    pub fn state(&self) -> &ResumeState {
        &self.state
    }

    /// The saved position for `item_id`, if any.
    pub fn position(&self, item_id: &str) -> Option<f64> {
        self.state.position(item_id)
    }

    /// The most recently watched item, if it is still in the library.
    pub fn last_item_in<'a, I>(&self, known_ids: I) -> Option<&str>
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.state.last_item_in(known_ids)
    }

    /// See [`ResumeState::note_started`].
    pub fn note_started(&mut self, item_id: &str) {
        if self.state.last_item.as_deref() != Some(item_id) {
            self.state.note_started(item_id);
            self.dirty = true;
        }
    }

    /// See [`ResumeState::record`]. Marks the store dirty only when the stored
    /// state actually changed, so a paused item doesn't schedule writes.
    pub fn record(&mut self, item_id: &str, position_seconds: f64, duration_seconds: Option<f64>) {
        let before = self.state.positions.get(item_id).cloned();
        self.state
            .record(item_id, position_seconds, duration_seconds);
        if self.state.positions.get(item_id).cloned() != before {
            self.dirty = true;
        }
    }

    /// See [`ResumeState::retain_known`].
    pub fn retain_known<'a, I>(&mut self, known_ids: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        let before = self.state.positions.len();
        self.state.retain_known(known_ids);
        if self.state.positions.len() != before {
            self.dirty = true;
        }
    }

    /// Write if there are pending changes and the last write is at least
    /// [`SAVE_INTERVAL`] old. `now` is passed in so callers (and tests) control
    /// the clock. Returns whether a write happened.
    pub fn flush_if_due(&mut self, now: Instant) -> bool {
        let due = match self.last_saved {
            Some(t) => now.duration_since(t) >= SAVE_INTERVAL,
            None => true,
        };
        if !due {
            return false;
        }
        self.flush_at(now)
    }

    /// Write pending changes now. Returns whether a write happened.
    pub fn flush(&mut self) -> bool {
        self.flush_at(Instant::now())
    }

    fn flush_at(&mut self, now: Instant) -> bool {
        if !self.dirty {
            return false;
        }
        match self.write() {
            Ok(()) => {
                self.dirty = false;
                self.last_saved = Some(now);
                true
            }
            Err(e) => {
                // Report once: a path we can't write to won't start working.
                if !self.write_failed {
                    self.write_failed = true;
                    log_write_failure(&self.path, &e);
                }
                self.dirty = false;
                false
            }
        }
    }

    /// Serialize and write atomically (temp sibling + rename), creating the
    /// parent directory on first write.
    fn write(&self) -> std::io::Result<()> {
        let body = toml::to_string_pretty(&self.state)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp_name = self.path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = self.path.with_file_name(tmp_name);
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// How long after a playback starts to ignore the player's position readings.
///
/// A load that carries a start offset does not report that offset instantly:
/// for a moment the backend still answers with the previous file's position, or
/// with zero. Recording that would erase the very position just resumed from, so
/// the first moment of every playback is not recorded.
pub const SETTLE_AFTER_START: Duration = Duration::from_secs(2);

/// Follows the item currently playing and feeds its position into a
/// [`ResumeStore`].
///
/// Both front-ends poll their player for a position every frame and need the
/// same three things from it — ignore the readings while a playback settles,
/// batch the writes, and record a final position once playback ends (by which
/// point the player reports nothing at all) — so that logic lives here rather
/// than twice in the UI layers.
pub struct ResumeTracker {
    store: ResumeStore,
    current: Option<Current>,
}

struct Current {
    item_id: String,
    started_at: Instant,
    /// The most recent usable `(position, duration)` reading.
    last: Option<(f64, Option<f64>)>,
}

impl ResumeTracker {
    pub fn new(store: ResumeStore) -> Self {
        Self {
            store,
            current: None,
        }
    }

    /// Where `item_id` should start from, for the player's start-position hook.
    pub fn start_position(&self, item_id: &str) -> Option<f64> {
        self.store.position(item_id)
    }

    /// The most recently watched item, if it is still in the library.
    pub fn last_item_in<'a, I>(&self, known_ids: I) -> Option<&str>
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.store.last_item_in(known_ids)
    }

    /// Drop positions for items the library no longer has.
    pub fn retain_known<'a, I>(&mut self, known_ids: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.store.retain_known(known_ids);
    }

    /// `item_id` has started playing: it becomes the last-watched item, and its
    /// position readings start being tracked.
    pub fn note_started(&mut self, item_id: &str, now: Instant) {
        self.store.note_started(item_id);
        self.current = Some(Current {
            item_id: item_id.to_string(),
            started_at: now,
            last: None,
        });
    }

    /// Feed the player's current reading (both `None` when it has none). Call
    /// once per frame while playing; writes are batched to [`SAVE_INTERVAL`].
    pub fn progress(&mut self, position: Option<f64>, duration: Option<f64>, now: Instant) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if now.duration_since(current.started_at) < SETTLE_AFTER_START {
            return;
        }
        if let Some(position) = position.filter(|p| p.is_finite() && *p >= 0.0) {
            current.last = Some((position, duration));
            let item_id = current.item_id.clone();
            self.store.record(&item_id, position, duration);
        }
        self.store.flush_if_due(now);
    }

    /// The live position of the item playing, if one has been observed. The
    /// front-ends re-apply this as the restart point so an automatic retry after
    /// a stream error resumes where the failure hit.
    pub fn live_position(&self) -> Option<f64> {
        self.current
            .as_ref()
            .and_then(|c| c.last)
            .map(|(position, _)| position)
    }

    /// Playback ended (naturally, by the viewer leaving, or by shutdown):
    /// record the last reading and write it out. An item that reached its end
    /// is forgotten by [`ResumeState::record`]'s policy, so the next play starts
    /// over.
    pub fn finished(&mut self) {
        if let Some(current) = self.current.take()
            && let Some((position, duration)) = current.last
        {
            self.store.record(&current.item_id, position, duration);
        }
        self.store.flush();
    }

    /// Write out anything pending without ending the current playback — for
    /// shutdown paths that can't tell whether an item is still playing.
    pub fn flush(&mut self) {
        if let Some(current) = self.current.as_ref()
            && let Some((position, duration)) = current.last
        {
            let item_id = current.item_id.clone();
            self.store.record(&item_id, position, duration);
        }
        self.store.flush();
    }
}

/// The two front-ends log through different stacks (`tracing-subscriber` on
/// Linux, `android_logger` on Android). A bare `log` record reaches both: the
/// former bridges `log` records in via `tracing-log`, the latter consumes them
/// directly.
fn log_write_failure(path: &Path, e: &std::io::Error) {
    log::warn!("could not save resume positions to {}: {e}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("a writable temp dir")
    }

    #[test]
    fn new_state_is_empty_and_current() {
        let s = ResumeState::new();
        assert_eq!(s.schema_version, RESUME_SCHEMA_VERSION);
        assert!(s.last_item.is_none());
        assert!(s.positions.is_empty());
        s.validate().unwrap();
    }

    #[test]
    fn records_a_mid_item_position() {
        let mut s = ResumeState::new();
        s.record("bunny", 300.0, Some(600.0));
        assert_eq!(s.position("bunny"), Some(300.0));
        assert_eq!(
            s.positions["bunny"].duration_seconds,
            Some(600.0),
            "duration should be kept so a later read can judge the position"
        );
    }

    #[test]
    fn ignores_the_first_seconds() {
        let mut s = ResumeState::new();
        s.record("bunny", MIN_RESUME_SECONDS - 0.1, Some(600.0));
        assert_eq!(s.position("bunny"), None);
    }

    #[test]
    fn a_near_end_stop_forgets_the_position() {
        let mut s = ResumeState::new();
        s.record("bunny", 300.0, Some(600.0));
        // Watched to the end: the next play should start over, not at the credits.
        s.record("bunny", 600.0 - NEAR_END_SECONDS + 1.0, Some(600.0));
        assert_eq!(s.position("bunny"), None);
    }

    #[test]
    fn an_unknown_duration_never_counts_as_finished() {
        // Live streams and playlists mpv hasn't measured report no duration.
        let mut s = ResumeState::new();
        s.record("live", 9_000.0, None);
        assert_eq!(s.position("live"), Some(9_000.0));
    }

    #[test]
    fn ignores_nonsense_positions() {
        let mut s = ResumeState::new();
        s.record("x", 300.0, Some(600.0));
        s.record("x", f64::NAN, Some(600.0));
        s.record("x", -1.0, Some(600.0));
        assert_eq!(
            s.position("x"),
            Some(300.0),
            "a garbage reading should leave the good one alone"
        );
    }

    #[test]
    fn last_item_is_filtered_by_the_current_library() {
        let mut s = ResumeState::new();
        s.note_started("gone");
        assert_eq!(s.last_item_in(["bunny", "sintel"]), None);
        s.note_started("sintel");
        assert_eq!(s.last_item_in(["bunny", "sintel"]), Some("sintel"));
    }

    #[test]
    fn retain_known_drops_departed_items_but_keeps_last_item() {
        let mut s = ResumeState::new();
        s.record("stays", 100.0, Some(600.0));
        s.record("goes", 100.0, Some(600.0));
        s.note_started("goes");
        s.retain_known(["stays"]);
        assert_eq!(s.position("stays"), Some(100.0));
        assert_eq!(s.position("goes"), None);
        assert_eq!(s.last_item.as_deref(), Some("goes"));
    }

    #[test]
    fn round_trips_through_toml() {
        let mut s = ResumeState::new();
        s.note_started("sintel");
        s.record("sintel", 123.5, Some(888.0));
        s.record("live", 500.0, None);
        let body = toml::to_string_pretty(&s).unwrap();
        let back: ResumeState = toml::from_str(&body).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let s: ResumeState = toml::from_str("schema_version = 999").unwrap();
        assert!(s.validate().is_err());
    }

    #[test]
    fn store_load_missing_file_is_empty() {
        let dir = temp_dir();
        let store = ResumeStore::load(dir.path().join("resume.toml")).unwrap();
        assert_eq!(store.state(), &ResumeState::new());
    }

    #[test]
    fn store_load_or_empty_survives_a_corrupt_file() {
        let dir = temp_dir();
        let path = dir.path().join("resume.toml");
        std::fs::write(&path, "this is not toml {{{").unwrap();
        let (store, err) = ResumeStore::load_or_empty(path);
        assert!(err.is_some(), "the parse failure should be reported");
        assert_eq!(store.state(), &ResumeState::new());
    }

    #[test]
    fn store_writes_and_reloads() {
        let dir = temp_dir();
        // A nested path exercises the parent-directory creation on first write.
        let path = dir.path().join("resume").join("movies.toml");
        let mut store = ResumeStore::load(path.clone()).unwrap();
        store.note_started("sintel");
        store.record("sintel", 300.0, Some(900.0));
        assert!(store.flush(), "a dirty store should write");
        assert!(!store.flush(), "a clean store should not rewrite");

        let back = ResumeStore::load(path).unwrap();
        assert_eq!(back.position("sintel"), Some(300.0));
        assert_eq!(back.last_item_in(["sintel"]), Some("sintel"));
    }

    #[test]
    fn flush_if_due_batches_writes() {
        let dir = temp_dir();
        let mut store = ResumeStore::load(dir.path().join("resume.toml")).unwrap();
        let t0 = Instant::now();

        store.record("sintel", 300.0, Some(900.0));
        assert!(store.flush_if_due(t0), "the first write is always due");

        store.record("sintel", 305.0, Some(900.0));
        assert!(
            !store.flush_if_due(t0 + SAVE_INTERVAL / 2),
            "a write within the interval should be batched"
        );
        assert!(
            store.flush_if_due(t0 + SAVE_INTERVAL),
            "once the interval passes the pending change should land"
        );
    }

    fn tracker(dir: &tempfile::TempDir) -> ResumeTracker {
        ResumeTracker::new(ResumeStore::load(dir.path().join("resume.toml")).unwrap())
    }

    #[test]
    fn tracker_ignores_readings_while_a_playback_settles() {
        // The trap this guards: resuming at 10:00 and immediately recording the
        // backend's stale "0s" would erase the position being resumed from.
        let dir = temp_dir();
        let mut t = tracker(&dir);
        t.store.record("sintel", 600.0, Some(1200.0));
        let t0 = Instant::now();
        t.note_started("sintel", t0);
        t.progress(Some(0.0), Some(1200.0), t0);
        t.progress(Some(0.0), Some(1200.0), t0 + SETTLE_AFTER_START / 2);
        assert_eq!(t.start_position("sintel"), Some(600.0));

        // Once settled, real readings are recorded.
        t.progress(Some(660.0), Some(1200.0), t0 + SETTLE_AFTER_START);
        assert_eq!(t.start_position("sintel"), Some(660.0));
    }

    #[test]
    fn tracker_records_the_final_position_when_playback_ends() {
        let dir = temp_dir();
        let mut t = tracker(&dir);
        let t0 = Instant::now();
        t.note_started("sintel", t0);
        t.progress(Some(700.0), Some(1200.0), t0 + SETTLE_AFTER_START);
        // The player reports nothing once it has stopped; the last reading is
        // what gets written.
        t.progress(None, None, t0 + SETTLE_AFTER_START);
        t.finished();

        let back = ResumeStore::load(dir.path().join("resume.toml")).unwrap();
        assert_eq!(back.position("sintel"), Some(700.0));
        assert_eq!(back.last_item_in(["sintel"]), Some("sintel"));
    }

    #[test]
    fn tracker_forgets_an_item_watched_to_the_end() {
        let dir = temp_dir();
        let mut t = tracker(&dir);
        let t0 = Instant::now();
        t.note_started("sintel", t0);
        t.progress(Some(1199.0), Some(1200.0), t0 + SETTLE_AFTER_START);
        t.finished();

        let back = ResumeStore::load(dir.path().join("resume.toml")).unwrap();
        assert_eq!(
            back.position("sintel"),
            None,
            "a finished item should start over next time"
        );
        assert_eq!(
            back.last_item_in(["sintel"]),
            Some("sintel"),
            "it is still the item watched most recently"
        );
    }

    #[test]
    fn tracker_exposes_the_live_position_for_a_restart() {
        let dir = temp_dir();
        let mut t = tracker(&dir);
        let t0 = Instant::now();
        assert_eq!(t.live_position(), None);
        t.note_started("sintel", t0);
        t.progress(Some(700.0), Some(1200.0), t0 + SETTLE_AFTER_START);
        assert_eq!(t.live_position(), Some(700.0));
        t.finished();
        assert_eq!(t.live_position(), None);
    }

    #[test]
    fn flush_is_a_no_op_without_changes() {
        let dir = temp_dir();
        let path = dir.path().join("resume.toml");
        let mut store = ResumeStore::load(path.clone()).unwrap();
        // Recording a sub-threshold position for an item with no entry changes
        // nothing, so nothing should be written.
        store.record("bunny", 1.0, Some(600.0));
        assert!(!store.flush());
        assert!(!path.exists());
    }
}
