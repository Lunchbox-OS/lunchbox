//! Value-scored eviction for an on-disk file cache.
//!
//! Shared by both media front-ends' video caches, which agree on naming (see
//! [`crate::cache_key`]) and on this ordering but little else. The Android
//! cache downloads direct HTTP into one directory per library, so it has one
//! rendition per URL and no sentinels; `lunchbox-media-cache` runs yt-dlp,
//! keys on the URL *and* the format selector, and tracks completion with
//! `.done` sentinels and download claims with `.lock` files. So only the
//! eviction *policy* is shared here: given the cached files with their sizes
//! and a [`Score`], delete the lowest-scoring until the total is within a byte
//! cap. Each caller scans its own directory (applying its own filters, building
//! its own [`Standing`]) and supplies an `on_evict` hook for any paired
//! bookkeeping (deleting a sentinel, logging).
//!
//! Deliberately std-only: no networking or image work, so it stays reusable and
//! cross-compiles for Android like the rest of this crate.
//!
//! # Why a score and not two classes
//!
//! This used to be a strict two-class ordering: every unwatched file sorted
//! below every watched one, so a guess could never cost the child something
//! they chose. That guarantee was absolute, and absolute was too strong. A film
//! watched once, months ago, outranked a video the parent added to the library
//! yesterday — forever. Worse, once every byte of a full cache had been watched
//! at least once, prefetch had nothing left it was allowed to spend and went
//! permanently inert.
//!
//! So watching now buys a *grace*: a fixed head start on the time axis, which
//! erodes at one day per day. Everything is scored on that one axis and the
//! lowest score is evicted first:
//!
//! ```text
//! watched    score = played_at  + watched_grace
//! unwatched  score = first_seen - min(ordinal, RANK_CAP) * POSITION_STEP
//! ```
//!
//! The guarantee is now time-bounded rather than absolute — within
//! [`ScoreWeights::watched_grace`] of a play, nothing speculative can touch the
//! file; past it, the file has to compete on age like anything else. That is
//! the one behavior a parent might notice, which is why the grace is
//! configurable (`service.media.watched_grace_days`) rather than a constant.
//!
//! Nothing here needs a deadband to stay stable, because none of the inputs
//! drift: the first-seen marker is write-once and the ordinal comes from the
//! library, so a file that loses a comparison today loses it again tomorrow
//! rather than trading places with whatever replaced it. The only input that
//! moves is `played_at`, and it moves because somebody watched something.
//!
//! The two terms on the unwatched side answer two different questions, and
//! conflating them was the old model's other mistake. `first_seen` is when the
//! item entered the library — a *new* item deserves a chance at the disk.
//! `ordinal` is where it sits in that library, which is a proxy for how soon
//! anyone will reach it. Ordering guesses by download time alone had to serve
//! both, so it inverted (newest first) to keep prefetch from re-downloading the
//! head of the list every sweep, and in doing so made every freshly added item
//! the first thing thrown away.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Default protection a play buys: for this long after watching something, no
/// speculative download can displace it.
///
/// Thirty days is chosen so a weekly favourite is never at risk and a film
/// watched once at the start of a school holiday is fair game by the end of the
/// next one.
pub const DEFAULT_WATCHED_GRACE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// What one place further down the library costs an unwatched file.
///
/// Small on purpose. It only has to break ties *within* a prefetch sweep, where
/// every file's `first_seen` is minutes apart; it must not let library position
/// outweigh a genuine difference in age.
pub const POSITION_STEP: Duration = Duration::from_secs(60 * 60);

/// Deepest library position that still counts against a file, so a
/// thousand-item playlist cannot spread its tail across a year of penalty.
/// At [`POSITION_STEP`] this caps the whole library's spread at one week.
pub const RANK_CAP: u32 = 168;

/// The tunable half of [`Score`]. Both processes sharing a cache directory must
/// agree on these or they will spend the same disk by different rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreWeights {
    /// How long a play protects a file outright. See [`DEFAULT_WATCHED_GRACE`].
    pub watched_grace: Duration,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            watched_grace: DEFAULT_WATCHED_GRACE,
        }
    }
}

/// What the cache knows about one file, before it is turned into a [`Score`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    /// When this item first entered the library, as far as this device saw.
    ///
    /// Recorded per *interest* (see [`crate::interest`]) so it survives the
    /// file being evicted and re-downloaded: an item added last year that has
    /// churned through the cache twice is not a new item.
    pub first_seen: SystemTime,
    /// When playback last started, or `None` if nobody has ever watched it.
    pub played_at: Option<SystemTime>,
    /// The item's index in its library at the time it was queued. `None` for a
    /// file no prefetch pass has claimed — a legacy entry, or a cache that does
    /// not prefetch at all — which is treated as the tail of the list.
    pub ordinal: Option<u32>,
}

impl Standing {
    /// A file nobody has watched, first seen at `first_seen`, at library
    /// position `ordinal`.
    pub fn guessed(first_seen: SystemTime, ordinal: Option<u32>) -> Self {
        Self {
            first_seen,
            played_at: None,
            ordinal,
        }
    }

    /// Whether anyone has watched this file.
    pub fn is_watched(&self) -> bool {
        self.played_at.is_some()
    }
}

/// How much a cached file has earned its place. Ordered least- to
/// most-deserving, so the smallest is evicted first.
///
/// It is a point on the time axis, not an abstract number: "this file behaves
/// as though it were last wanted at time T". That keeps the weights in units
/// anyone can reason about — a grace is a number of days, not a magic constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score(SystemTime);

impl Score {
    /// Score a file from what the cache knows about it.
    pub fn of(standing: &Standing, weights: ScoreWeights) -> Self {
        match standing.played_at {
            Some(at) => Score(saturating_add(at, weights.watched_grace)),
            None => Score(saturating_sub(
                standing.first_seen,
                position_penalty(standing.ordinal),
            )),
        }
    }

    /// The score of a download the user earned by watching the previous video
    /// through to the end, as of `now`.
    ///
    /// This is the maximum any file can hold, so such a download may displace
    /// anything — except a file watched equally recently, which is how it
    /// avoids evicting itself before the trim that follows it.
    pub fn earned_at(now: SystemTime, weights: ScoreWeights) -> Self {
        Score(saturating_add(now, weights.watched_grace))
    }

    /// Whether this file is worth strictly more than `other`, and so may take
    /// its place.
    ///
    /// There is no deadband here, and none is needed: what stops two files
    /// evicting each other in turn is that neither one's score moves. The
    /// first-seen marker is write-once and the ordinal comes from the library,
    /// so an item that loses a comparison today loses it again tomorrow instead
    /// of drifting back above its replacement. The one input that does move —
    /// `played_at` — only moves when somebody actually watches something, which
    /// is a real change of standing and should win.
    ///
    /// Equality is deliberately *not* enough. A download and the file it just
    /// wrote score identically, so this is also what stops a download evicting
    /// itself in the trim that follows it.
    pub fn outranks(self, other: Score) -> bool {
        self.0 > other.0
    }
}

/// What library position costs an unwatched file. An unknown position is the
/// tail: those files belong to no prefetch pass, so nothing is coming to ask
/// for them next.
fn position_penalty(ordinal: Option<u32>) -> Duration {
    POSITION_STEP * ordinal.unwrap_or(RANK_CAP).min(RANK_CAP)
}

fn saturating_add(t: SystemTime, d: Duration) -> SystemTime {
    t.checked_add(d).unwrap_or(t)
}

fn saturating_sub(t: SystemTime, d: Duration) -> SystemTime {
    t.checked_sub(d).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// A cached file eligible for eviction: its path, size in bytes, and the score
/// that orders it against the others (the smallest is evicted first).
pub struct LruEntry<K> {
    pub path: PathBuf,
    pub size: u64,
    pub score: K,
}

/// Delete lowest-scoring files until the total size is at or below
/// `max_bytes`. A no-op when the total is already within the cap.
///
/// `on_evict` runs after each successful removal — use it to drop a paired
/// sentinel file or log the eviction. It is not called for a file that fails to
/// delete (that file's bytes still count toward the remaining total, so the
/// loop may remove more than strictly necessary rather than spin).
pub fn evict_to_cap<K: Ord>(
    entries: Vec<LruEntry<K>>,
    max_bytes: u64,
    on_evict: impl FnMut(&Path),
) {
    evict_to_cap_where(entries, max_bytes, |_| true, on_evict)
}

/// As [`evict_to_cap`], but only files for which `eligible` returns true may be
/// deleted. Every entry still counts toward the total, so an ineligible file
/// occupies space that eviction cannot reclaim — if the eligible set runs out
/// the total may remain above `max_bytes`.
///
/// This is how a caller bounds what a download may spend:
/// `lunchbox-media-cache` passes `|e| incoming.outranks(e.score)`, so a
/// download only ever displaces files worth less than itself.
pub fn evict_to_cap_where<K: Ord>(
    mut entries: Vec<LruEntry<K>>,
    max_bytes: u64,
    mut eligible: impl FnMut(&LruEntry<K>) -> bool,
    mut on_evict: impl FnMut(&Path),
) {
    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    if total <= max_bytes {
        return;
    }
    // Lowest score first.
    entries.sort_by(|a, b| a.score.cmp(&b.score));
    for entry in entries {
        if total <= max_bytes {
            break;
        }
        if !eligible(&entry) {
            continue;
        }
        if std::fs::remove_file(&entry.path).is_ok() {
            total = total.saturating_sub(entry.size);
            on_evict(&entry.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn seed(dir: &Path, name: &str, size: usize) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![0u8; size]).unwrap();
        p
    }

    fn watched(now: SystemTime, ago: Duration) -> Score {
        Score::of(
            &Standing {
                first_seen: now - ago,
                played_at: Some(now - ago),
                ordinal: Some(0),
            },
            ScoreWeights::default(),
        )
    }

    fn guessed(now: SystemTime, ago: Duration, ordinal: u32) -> Score {
        Score::of(
            &Standing::guessed(now - ago, Some(ordinal)),
            ScoreWeights::default(),
        )
    }

    #[test]
    fn evicts_lowest_scoring_until_within_cap() {
        // Cap 250 bytes; three 100-byte files → the lowest-scoring is evicted.
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let low = seed(d, "low", 100);
        let mid = seed(d, "mid", 100);
        let high = seed(d, "high", 100);
        let mut evicted = Vec::new();
        evict_to_cap(
            vec![
                LruEntry {
                    path: low.clone(),
                    size: 100,
                    score: 1u64,
                },
                LruEntry {
                    path: mid.clone(),
                    size: 100,
                    score: 2,
                },
                LruEntry {
                    path: high.clone(),
                    size: 100,
                    score: 3,
                },
            ],
            250,
            |p| evicted.push(p.to_path_buf()),
        );
        assert!(!low.exists(), "the lowest-scoring file is evicted");
        assert!(mid.exists());
        assert!(high.exists());
        assert_eq!(evicted, vec![low], "on_evict fires once, for the lowest");
    }

    #[test]
    fn noop_when_within_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let a = seed(tmp.path(), "a", 100);
        let mut hook_ran = false;
        evict_to_cap(
            vec![LruEntry {
                path: a.clone(),
                size: 100,
                score: 1u64,
            }],
            1_000,
            |_| hook_ran = true,
        );
        assert!(a.exists());
        assert!(!hook_ran);
    }

    #[test]
    fn ineligible_files_are_never_deleted_even_when_over_cap() {
        // The protection `lunchbox-media-cache` relies on: a download may only
        // spend files it outranks, so it can leave the cache above its cap
        // rather than touch one it does not.
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let protected = seed(d, "protected", 100);
        let spare = seed(d, "spare", 100);
        evict_to_cap_where(
            vec![
                LruEntry {
                    path: protected.clone(),
                    size: 100,
                    score: 1u64,
                },
                LruEntry {
                    path: spare.clone(),
                    size: 100,
                    score: 2,
                },
            ],
            50,
            |e| e.path != protected,
            |_| {},
        );
        assert!(protected.exists(), "protected file survives");
        assert!(!spare.exists(), "the eligible file is still reclaimed");
    }

    // --- the scoring policy ---

    #[test]
    fn a_recent_play_outranks_a_fresh_addition() {
        // Inside the grace the old absolute guarantee still holds.
        let now = SystemTime::now();
        assert!(watched(now, DAY) > guessed(now, Duration::ZERO, 0));
    }

    #[test]
    fn a_stale_play_loses_to_a_fresh_addition() {
        // The headline change: watched once, six weeks ago, is no longer a
        // permanent claim on the disk.
        let now = SystemTime::now();
        assert!(
            watched(now, 42 * DAY) < guessed(now, Duration::ZERO, 0),
            "protection has to erode, or a full cache never takes new content"
        );
    }

    #[test]
    fn the_grace_is_what_decides_where_that_flips() {
        let now = SystemTime::now();
        let fresh_guess = guessed(now, Duration::ZERO, 0);
        assert!(
            watched(now, 29 * DAY) > fresh_guess,
            "just inside the default grace"
        );
        assert!(
            watched(now, 31 * DAY) < fresh_guess,
            "just outside the default grace"
        );
    }

    #[test]
    fn a_longer_grace_protects_for_longer() {
        let now = SystemTime::now();
        let weights = ScoreWeights {
            watched_grace: 90 * DAY,
        };
        let stale = Score::of(
            &Standing {
                first_seen: now - 42 * DAY,
                played_at: Some(now - 42 * DAY),
                ordinal: None,
            },
            weights,
        );
        let fresh = Score::of(&Standing::guessed(now, Some(0)), weights);
        assert!(stale > fresh, "the knob has to actually move the boundary");
    }

    #[test]
    fn among_guesses_of_one_sweep_the_tail_of_the_library_goes_first() {
        // Prefetch fills in display order, so within a sweep every file's
        // `first_seen` is minutes apart and position is what separates them.
        // Evicting the head instead would have the next pass re-download it.
        let now = SystemTime::now();
        assert!(guessed(now, Duration::ZERO, 90) < guessed(now, Duration::ZERO, 3));
    }

    #[test]
    fn among_guesses_the_older_arrival_goes_first() {
        // The old model had this backwards: it evicted the newest guess, which
        // is exactly the item a parent had just added.
        let now = SystemTime::now();
        assert!(guessed(now, 30 * DAY, 5) < guessed(now, Duration::ZERO, 5));
    }

    #[test]
    fn a_freshly_added_tail_item_still_beats_a_stale_guess() {
        // Position must break ties, not outweigh age — hence the small step and
        // the cap.
        let now = SystemTime::now();
        assert!(guessed(now, Duration::ZERO, RANK_CAP) > guessed(now, 30 * DAY, 0));
    }

    #[test]
    fn library_position_cannot_run_away_on_a_huge_playlist() {
        let now = SystemTime::now();
        assert_eq!(
            guessed(now, Duration::ZERO, RANK_CAP),
            guessed(now, Duration::ZERO, 50_000)
        );
    }

    #[test]
    fn an_unplaced_file_sorts_as_the_tail() {
        // Legacy entries and the Android cache, which does not prefetch: with
        // nothing coming to ask for them, they rank as the end of the list.
        let now = SystemTime::now();
        let unplaced = Score::of(&Standing::guessed(now, None), ScoreWeights::default());
        assert_eq!(unplaced, guessed(now, Duration::ZERO, RANK_CAP));
    }

    #[test]
    fn among_watched_files_the_least_recently_played_goes_first() {
        let now = SystemTime::now();
        assert!(watched(now, 2 * DAY) < watched(now, DAY));
    }

    #[test]
    fn a_file_does_not_outrank_its_own_equal() {
        // What stops a download evicting the file it just wrote: the two score
        // identically, and equality is not enough to displace.
        let now = SystemTime::now();
        let incumbent = guessed(now, 10 * DAY, 0);
        assert!(!guessed(now, 10 * DAY, 0).outranks(incumbent));
        assert!(guessed(now, 9 * DAY, 0).outranks(incumbent));
    }

    #[test]
    fn a_guess_cannot_displace_one_closer_to_the_head_of_its_own_library() {
        // The stability property that replaces a deadband. Within a sweep every
        // file shares a first-seen, so position decides — and position does not
        // change between sweeps, so the item that loses today loses tomorrow
        // instead of trading places with its replacement.
        let now = SystemTime::now();
        assert!(!guessed(now, Duration::ZERO, 9).outranks(guessed(now, Duration::ZERO, 2)));
    }

    #[test]
    fn an_earned_download_outranks_everything_but_an_equally_recent_play() {
        let now = SystemTime::now();
        let earned = Score::earned_at(now, ScoreWeights::default());
        assert!(earned.outranks(watched(now, 2 * DAY)));
        assert!(earned.outranks(guessed(now, Duration::ZERO, 0)));
        assert!(
            !earned.outranks(watched(now, Duration::ZERO)),
            "it must not evict the file it just downloaded"
        );
    }

    #[test]
    fn a_new_arrival_can_take_the_place_of_an_older_guess_wherever_it_sits() {
        // How a full cache keeps accepting content: an item appended to the end
        // of a library still outranks guesses made months ago, because the
        // position penalty is capped well below the ages it has to compete with.
        let now = SystemTime::now();
        assert!(guessed(now, Duration::ZERO, RANK_CAP).outranks(guessed(now, 60 * DAY, 0)));
    }
}
