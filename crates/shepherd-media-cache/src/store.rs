//! The on-disk cache layout: what a committed download looks like, how recently
//! it mattered to anyone, and which files eviction is allowed to take.
//!
//! One flat directory holds, per content key (see [`crate::key`]):
//!
//! | File | Meaning |
//! |---|---|
//! | `<key>.<ext>` | the video |
//! | `<key>.done` | commit sentinel; records the interest key, selector, ordinal |
//! | `<key>.part` | an in-flight direct-HTTP download |
//! | `<key>.lock` | download claim (see [`crate::lock`]) |
//! | `<key>.failed` | when the last download attempt failed |
//!
//! plus, per *interest* key, `<ikey>.played` — written the first time the video
//! is played from cache, and never removed. It is what separates "the child
//! watched this" from "we guessed they might" — and `<ikey>.seen`, written the
//! first time the item is queued at all, which is what separates "the parent
//! added this yesterday" from "this has sat here unwatched since spring".
//!
//! **Files from before URL keying are left in place.** They are named after
//! library item ids, so nothing will ever ask for them again, but they carry
//! valid sentinels — which makes them ordinary eviction candidates that age out
//! on their own. Migrating them would mean keeping a rename map forever to save
//! a download that is, by definition, re-downloadable.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use shepherd_media_app::interest;
use shepherd_media_app::lru::{self, LruEntry, Score, ScoreWeights, Standing};
use tracing::{info, warn};

use crate::lock::is_lock_file;

/// What the cache holds for a content key.
///
/// There is no "stale" state any more: the yt-dlp selector is part of the
/// content key, so a file downloaded under a different one lives under a
/// different name and cannot be mistaken for this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// Nothing committed for this key.
    Absent,
    /// A committed, playable file.
    Present,
}

/// Extension of the failure marker. Keyed by *content*, not interest: a
/// rendition that will not download says nothing about the others.
const FAILED_EXT: &str = "failed";

/// Whether `name` is bookkeeping rather than a cached video.
fn is_sidecar(name: &str) -> bool {
    name.ends_with(".part")
        || name.ends_with(".done")
        || name.ends_with(".failed")
        || interest::is_marker(name)
        || is_lock_file(name)
}

/// Record that downloading `key` just failed.
///
/// Without this a permanently broken item is retried on every sweep, forever,
/// at full speed: a library whose videos have all become unavailable turns into
/// an hourly burst of doomed yt-dlp invocations and a screenful of warnings. The
/// marker's mtime is when it last failed, which is all [`retry_blocked`] needs.
pub fn mark_failed(cache_dir: &Path, key: &str) {
    let path = cache_dir.join(format!("{key}.{FAILED_EXT}"));
    if let Err(e) = std::fs::write(&path, b"") {
        warn!("could not record download failure for {key}: {e}");
    }
}

/// Forget any recorded failure for `key`. Called on a successful download, so
/// an item that recovers is not held back by the last time it did not.
pub fn clear_failed(cache_dir: &Path, key: &str) {
    let _ = std::fs::remove_file(cache_dir.join(format!("{key}.{FAILED_EXT}")));
}

/// Whether `key` failed too recently to be worth trying again.
///
/// Applies to speculative downloads only. A download the user earned by
/// watching the previous video is always attempted: they are waiting on it, and
/// a stale marker must not be why they get nothing.
pub fn retry_blocked(cache_dir: &Path, key: &str, cooldown: Duration) -> bool {
    let Ok(meta) = cache_dir.join(format!("{key}.{FAILED_EXT}")).metadata() else {
        return false;
    };
    let Ok(failed_at) = meta.modified() else {
        return false;
    };
    // A marker dated in the future (a clock that stepped back) reads as "just
    // failed" rather than blocking the item until the clock catches up.
    failed_at
        .elapsed()
        .map(|since| since < cooldown)
        .unwrap_or(true)
}

/// Scan `cache_dir` for a completed download named `<key>.<ext>`.
///
/// Requires the sentinel `<key>.done`; without it the download is considered
/// in-progress (yt-dlp may have written intermediate per-format files that are
/// not yet merged) and `None` is returned.
pub fn find_cached_file(cache_dir: &Path, key: &str) -> Option<PathBuf> {
    // The sentinel is written only after the download fully commits.
    if !cache_dir.join(format!("{key}.done")).exists() {
        return None;
    }
    let prefix = format!("{key}.");
    for entry in std::fs::read_dir(cache_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) && !is_sidecar(&name_str) {
            return Some(entry.path());
        }
    }
    None
}

/// Write the completion sentinel for `key`, recording the interest key the
/// video's markers live under, the selector it was fetched with, and where the
/// item sat in its library when it was queued.
///
/// The interest key has to be recorded rather than recomputed: walking the
/// directory yields content keys, and a hash cannot be run backwards to the URL
/// that would produce the interest key. The selector is kept for debugging
/// only — it is part of the content key now, so nothing compares it.
///
/// The ordinal is what eviction uses to order guesses within a sweep, and it
/// has to be recorded for the same reason: the directory has no idea what
/// library a file came from, let alone where in it. `None` for a download no
/// prefetch pass placed (one earned by watching the previous video), which
/// scores as the tail — irrelevant either way, since such a file is watched and
/// takes the other branch of [`Score::of`].
pub fn write_done_sentinel(
    cache_dir: &Path,
    key: &str,
    interest_key: &str,
    selector: &str,
    ordinal: Option<u32>,
) -> Result<(), String> {
    let path = cache_dir.join(format!("{key}.done"));
    let mut body = format!("interest={interest_key}\nselector={selector}\n");
    if let Some(ordinal) = ordinal {
        body.push_str(&format!("ordinal={ordinal}\n"));
    }
    std::fs::write(&path, body.as_bytes())
        .map_err(|e| format!("failed to write done sentinel: {e}"))
}

/// One `field=value` line from `key`'s sentinel.
///
/// Sentinels written before a given field existed simply yield `None` for it —
/// the pre-hash leftovers hold a bare selector string and have none of them.
fn sentinel_field(cache_dir: &Path, key: &str, field: &str) -> Option<String> {
    let body = std::fs::read_to_string(cache_dir.join(format!("{key}.done"))).ok()?;
    let prefix = format!("{field}=");
    body.lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .map(|v| v.trim().to_string())
}

/// Classify what the cache holds for `key`.
pub fn cache_state(cache_dir: &Path, key: &str) -> CacheState {
    if find_cached_file(cache_dir, key).is_some() {
        CacheState::Present
    } else {
        CacheState::Absent
    }
}

/// Record that the video under `interest_key` was played.
///
/// Called when playback actually starts from a cached file, not when the cache
/// is merely inspected — enumerating the cache, which the prefetcher does,
/// must not make everything look watched.
///
/// The marker is never removed, including when the video is evicted: a child
/// who watched something has shown an interest in it that survives the file.
pub fn mark_played(cache_dir: &Path, interest_key: &str) {
    if let Err(e) = interest::mark_played(cache_dir, interest_key) {
        warn!("could not record playback of {interest_key}: {e}");
    }
}

/// Record that the video under `interest_key` has been offered to this device.
///
/// Called for every library item a prefetch pass walks, downloaded or not — an
/// item the cache had no room for still has to be correctly aged when room
/// appears. Write-once, so the value stays "when it first appeared".
pub fn mark_seen(cache_dir: &Path, interest_key: &str) {
    if let Err(e) = interest::mark_seen(cache_dir, interest_key) {
        warn!("could not record first sighting of {interest_key}: {e}");
    }
}

/// When the item under `interest_key` first appeared, given the mtime of a
/// cached file for it.
///
/// The recorded marker wins, but the file's own mtime is a floor: an item
/// cached before the marker existed gets stamped on the next sweep, and taking
/// that stamp at face value would present the whole pre-existing cache as
/// freshly added. Whichever is older is the honest answer.
fn first_seen(
    cache_dir: &Path,
    interest_key: Option<&str>,
    downloaded_at: SystemTime,
) -> SystemTime {
    interest_key
        .and_then(|ikey| interest::first_seen_at(cache_dir, ikey))
        .map_or(downloaded_at, |seen| seen.min(downloaded_at))
}

/// Whether a file under a content key is bookkeeping that outlives the content.
///
/// The `.lock` file is spared because removing it while the caller holds it
/// would leave the next claimant locking a *different* inode, and the mutual
/// exclusion it exists for would silently stop working. The `.failed` marker is
/// spared because clearing the content is exactly what happens on the way into
/// and out of a failed attempt — deleting the record of that failure in the
/// same breath would make the cooldown a no-op.
fn is_bookkeeping(name: &str) -> bool {
    is_lock_file(name) || name.ends_with(".failed")
}

/// Delete every committed file for `key` plus its sentinel, leaving the
/// bookkeeping beside it alone (see [`is_bookkeeping`]).
pub fn remove_cached_item(cache_dir: &Path, key: &str) {
    let prefix = format!("{key}.");
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix)
            && !is_bookkeeping(&name_str)
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            warn!("could not remove stale cache file {:?}: {e}", entry.path());
        }
    }
}

struct CacheEntry {
    path: PathBuf,
    size: u64,
    standing: Standing,
}

/// Collect all committed video files in `cache_dir` — those with a matching
/// `<key>.done` sentinel — with their sizes and standing. In-progress downloads
/// (no sentinel) are excluded so they neither count toward the size cap nor get
/// evicted mid-download. Returns `None` only if the directory cannot be read.
fn collect_cache_entries(cache_dir: &Path) -> Option<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    for de in std::fs::read_dir(cache_dir).ok()?.flatten() {
        let name = de.file_name();
        let name_str = name.to_string_lossy();
        if is_sidecar(&name_str) {
            continue;
        }
        let Ok(meta) = de.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        // Derive the content key from the filename stem.
        let key = match de.path().file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Only include files whose download has been fully committed.
        if !cache_dir.join(format!("{key}.done")).exists() {
            continue;
        }

        // Everything eviction orders by comes from the sentinel and the two
        // interest markers beside it. A file whose sentinel predates the
        // interest key has neither marker to consult: it reads as never
        // watched, never placed, and as old as its own mtime — which is what
        // ages the pre-hash leftovers out.
        let downloaded_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let interest_key = sentinel_field(cache_dir, &key, "interest");
        let played_at = interest_key
            .as_deref()
            .and_then(|ikey| interest::played_at(cache_dir, ikey));

        entries.push(CacheEntry {
            path: de.path(),
            size: meta.len(),
            standing: Standing {
                first_seen: first_seen(cache_dir, interest_key.as_deref(), downloaded_at),
                played_at,
                ordinal: sentinel_field(cache_dir, &key, "ordinal").and_then(|v| v.parse().ok()),
            },
        });
    }
    Some(entries)
}

/// Total size of the committed cache, in bytes.
pub fn cache_total(cache_dir: &Path) -> u64 {
    collect_cache_entries(cache_dir)
        .map(|e| e.iter().map(|x| x.size).sum())
        .unwrap_or(0)
}

fn lru_entries(cache_dir: &Path, weights: ScoreWeights) -> Option<Vec<LruEntry<Score>>> {
    Some(
        collect_cache_entries(cache_dir)?
            .into_iter()
            .map(|e| LruEntry {
                path: e.path,
                size: e.size,
                score: Score::of(&e.standing, weights),
            })
            .collect(),
    )
}

fn drop_sentinel(cache_dir: &Path, path: &Path) {
    info!("evicted cached video: {}", path.display());
    if let Some(key) = path.file_stem().and_then(|s| s.to_str()) {
        let _ = std::fs::remove_file(cache_dir.join(format!("{key}.done")));
    }
}

/// Evict until the total is at or below `target_bytes`, spending only files
/// that `incoming` outranks.
///
/// One rule covers both callers, because "what may this download cost?" is the
/// same question in both cases and only the answer's magnitude differs. A
/// speculative prefetch scores as a guess, so it recycles space held by other
/// guesses and by content watched long enough ago to have lost its grace — and
/// stops rather than touch a film watched last week. A download earned by
/// watching the previous video scores as a play (see [`Score::earned_at`]), so
/// it outranks everything except an equally recent play, which is how it avoids
/// evicting itself.
///
/// If nothing left is outranked, the total simply stays above the target and
/// the caller skips its download.
pub fn evict_for(cache_dir: &Path, target_bytes: u64, incoming: Score, weights: ScoreWeights) {
    let Some(entries) = lru_entries(cache_dir, weights) else {
        return;
    };
    lru::evict_to_cap_where(
        entries,
        target_bytes,
        |e| incoming.outranks(e.score),
        |path| drop_sentinel(cache_dir, path),
    );
}

/// The score a not-yet-downloaded item would hold: a guess at library position
/// `ordinal`, first seen whenever this device first saw it (or now, if this is
/// the first sighting).
///
/// Computed *before* the download so the worker can decide whether the cache
/// holds anything cheap enough to make room, and reused after it so the trim
/// spends exactly what the pre-flight promised.
pub fn prospective_score(
    cache_dir: &Path,
    interest_key: &str,
    ordinal: Option<u32>,
    now: SystemTime,
    weights: ScoreWeights,
) -> Score {
    let first_seen = interest::first_seen_at(cache_dir, interest_key).unwrap_or(now);
    Score::of(&Standing::guessed(first_seen, ordinal), weights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;
    use std::time::Duration;

    const H264: &str = "bv*[vcodec^=avc1][height<=?1080]+ba/b";
    const DAY: i64 = 24 * 60 * 60;

    /// Commit `key` to the cache as if a prefetch pass had downloaded it at
    /// library position `ordinal`, with `ikey` as its video's interest key.
    fn commit_at(dir: &Path, key: &str, ikey: &str, ext: &str, ordinal: Option<u32>) {
        std::fs::write(dir.join(format!("{key}.{ext}")), b"video").unwrap();
        write_done_sentinel(dir, key, ikey, H264, ordinal).unwrap();
        mark_seen(dir, ikey);
    }

    /// As [`commit_at`], at the head of the library.
    fn commit(dir: &Path, key: &str, ikey: &str, ext: &str) {
        commit_at(dir, key, ikey, ext, Some(0));
    }

    fn age(dir: &Path, name: &str, secs_ago: i64) {
        filetime::set_file_mtime(
            dir.join(name),
            FileTime::from_unix_time(FileTime::now().unix_seconds() - secs_ago, 0),
        )
        .unwrap();
    }

    /// Age both of an item's first-sighting records, so it reads as genuinely
    /// old rather than as something stamped by this test a moment ago.
    fn age_arrival(dir: &Path, key: &str, ikey: &str, ext: &str, secs_ago: i64) {
        age(dir, &format!("{key}.{ext}"), secs_ago);
        age(dir, &format!("{ikey}.seen"), secs_ago);
    }

    fn weights() -> ScoreWeights {
        ScoreWeights::default()
    }

    /// The score a fresh prefetch of a head-of-library item would hold.
    fn fresh_guess(dir: &Path) -> Score {
        prospective_score(
            dir,
            "unseen-interest-key",
            Some(0),
            SystemTime::now(),
            weights(),
        )
    }

    /// The size of one committed test video, used as the eviction target when a
    /// test wants room for exactly one file.
    const ONE_VIDEO: u64 = 5; // b"video"

    #[test]
    fn absent_when_nothing_committed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
    }

    #[test]
    fn absent_while_a_download_is_still_in_flight() {
        // A file with no sentinel is an unfinished download, not a broken one —
        // treating it as present would hand a partial file to the player.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip.part"), b"partial").unwrap();
        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
    }

    #[test]
    fn present_once_committed() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Present);
    }

    #[test]
    fn removing_an_item_clears_every_extension_and_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "webm");
        std::fs::write(dir.path().join("clip.part"), b"partial").unwrap();
        // A different item must survive.
        commit(dir.path(), "other", "ikey2", "mp4");

        remove_cached_item(dir.path(), "clip");

        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
        assert!(!dir.path().join("clip.part").exists());
        assert_eq!(cache_state(dir.path(), "other"), CacheState::Present);
    }

    #[test]
    fn a_lock_file_is_not_content() {
        // It must not be served as the video, counted toward the cap, or
        // deleted along with the item it guards.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip.lock"), b"").unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");

        assert_eq!(
            find_cached_file(dir.path(), "clip").unwrap().extension(),
            Some("mp4".as_ref())
        );
        assert_eq!(cache_total(dir.path()), ONE_VIDEO);

        remove_cached_item(dir.path(), "clip");
        assert!(
            dir.path().join("clip.lock").exists(),
            "the lock must survive so the holder keeps its inode"
        );
    }

    #[test]
    fn neither_marker_is_content() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        mark_played(dir.path(), "ikey");
        assert!(dir.path().join("ikey.seen").exists());
        assert_eq!(
            cache_total(dir.path()),
            ONE_VIDEO,
            "markers must not count toward the cap"
        );
        assert_eq!(
            find_cached_file(dir.path(), "clip").unwrap().extension(),
            Some("mp4".as_ref())
        );
    }

    #[test]
    fn the_sentinel_carries_what_eviction_orders_by() {
        let dir = tempfile::tempdir().unwrap();
        commit_at(dir.path(), "clip", "ikey", "mp4", Some(7));
        assert_eq!(
            sentinel_field(dir.path(), "clip", "interest").as_deref(),
            Some("ikey")
        );
        assert_eq!(
            sentinel_field(dir.path(), "clip", "ordinal").as_deref(),
            Some("7")
        );
        assert_eq!(
            sentinel_field(dir.path(), "clip", "selector").as_deref(),
            Some(H264)
        );
    }

    // --- eviction: what a guess may spend ---

    #[test]
    fn a_recent_play_is_untouchable_by_a_guess() {
        // Inside the grace, the guarantee is what it always was: a speculative
        // download must not cost the child something they chose.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "watched", "iw", "mp4");
        mark_played(dir.path(), "iw");

        evict_for(dir.path(), 0, fresh_guess(dir.path()), weights());

        assert_eq!(cache_state(dir.path(), "watched"), CacheState::Present);
    }

    #[test]
    fn a_play_old_enough_to_have_lost_its_grace_is_fair_game() {
        // The headline change. Under the old two-class ordering this file was
        // protected forever, and a cache full of them went permanently inert.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "watched", "iw", "mp4");
        mark_played(dir.path(), "iw");
        age(dir.path(), "iw.played", 60 * DAY);

        evict_for(dir.path(), 0, fresh_guess(dir.path()), weights());

        assert_eq!(cache_state(dir.path(), "watched"), CacheState::Absent);
    }

    #[test]
    fn a_longer_grace_keeps_that_same_file() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "watched", "iw", "mp4");
        mark_played(dir.path(), "iw");
        age(dir.path(), "iw.played", 60 * DAY);

        let patient = ScoreWeights {
            watched_grace: Duration::from_secs(120 * DAY as u64),
        };
        evict_for(dir.path(), 0, fresh_guess(dir.path()), patient);

        assert_eq!(
            cache_state(dir.path(), "watched"),
            CacheState::Present,
            "the configured grace has to reach the eviction pass"
        );
    }

    #[test]
    fn a_stale_play_still_outranks_a_stale_guess() {
        // Losing the grace does not put watched content *below* an equally old
        // guess — it only stops it winning automatically.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "watched", "iw", "mp4");
        commit(dir.path(), "guessed", "ig", "mp4");
        mark_played(dir.path(), "iw");
        age(dir.path(), "iw.played", 60 * DAY);
        age_arrival(dir.path(), "watched", "iw", "mp4", 60 * DAY);
        age_arrival(dir.path(), "guessed", "ig", "mp4", 60 * DAY);

        evict_for(dir.path(), ONE_VIDEO, fresh_guess(dir.path()), weights());

        assert_eq!(cache_state(dir.path(), "watched"), CacheState::Present);
        assert_eq!(cache_state(dir.path(), "guessed"), CacheState::Absent);
    }

    #[test]
    fn among_guesses_of_one_sweep_the_tail_of_the_library_goes_first() {
        // Prefetch walks a library in display order, so within a sweep position
        // is what separates the files. Evicting the head would have the next
        // pass immediately re-download it.
        let dir = tempfile::tempdir().unwrap();
        commit_at(dir.path(), "head", "ih", "mp4", Some(0));
        commit_at(dir.path(), "tail", "it", "mp4", Some(90));

        evict_for(
            dir.path(),
            ONE_VIDEO,
            Score::earned_at(SystemTime::now(), weights()),
            weights(),
        );

        assert_eq!(cache_state(dir.path(), "head"), CacheState::Present);
        assert_eq!(cache_state(dir.path(), "tail"), CacheState::Absent);
    }

    #[test]
    fn a_freshly_added_item_survives_a_guess_that_has_sat_there_for_a_month() {
        // What the old ordering got exactly backwards: it evicted the newest
        // unwatched file, which is precisely the one a parent had just added.
        let dir = tempfile::tempdir().unwrap();
        commit_at(dir.path(), "stale", "is", "mp4", Some(0));
        commit_at(dir.path(), "added", "ia", "mp4", Some(40));
        age_arrival(dir.path(), "stale", "is", "mp4", 30 * DAY);

        evict_for(
            dir.path(),
            ONE_VIDEO,
            Score::earned_at(SystemTime::now(), weights()),
            weights(),
        );

        assert_eq!(
            cache_state(dir.path(), "added"),
            CacheState::Present,
            "a new library item must get a chance at the disk"
        );
        assert_eq!(cache_state(dir.path(), "stale"), CacheState::Absent);
    }

    #[test]
    fn among_watched_files_the_least_recently_played_goes_first() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "old", "io", "mp4");
        commit(dir.path(), "new", "in", "mp4");
        mark_played(dir.path(), "io");
        mark_played(dir.path(), "in");
        age(dir.path(), "io.played", 3 * DAY);

        evict_for(
            dir.path(),
            ONE_VIDEO,
            Score::earned_at(SystemTime::now(), weights()),
            weights(),
        );

        assert_eq!(cache_state(dir.path(), "old"), CacheState::Absent);
        assert_eq!(cache_state(dir.path(), "new"), CacheState::Present);
    }

    #[test]
    fn an_earned_download_may_displace_a_watched_file() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "watched", "iw", "mp4");
        mark_played(dir.path(), "iw");
        age(dir.path(), "iw.played", 3 * DAY);

        evict_for(
            dir.path(),
            0,
            Score::earned_at(SystemTime::now(), weights()),
            weights(),
        );

        assert_eq!(cache_state(dir.path(), "watched"), CacheState::Absent);
    }

    #[test]
    fn a_played_marker_outlives_the_video_it_names() {
        // Interest in a video survives the file: re-downloaded later, it is
        // still content the child has chosen once.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        mark_played(dir.path(), "ikey");

        evict_for(
            dir.path(),
            0,
            Score::earned_at(SystemTime::now(), weights()),
            weights(),
        );

        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
        assert!(dir.path().join("ikey.played").exists());
        assert!(
            dir.path().join("ikey.seen").exists(),
            "and so does when it first appeared, or a re-download would look new"
        );
    }

    #[test]
    fn a_re_download_is_not_a_new_arrival() {
        // The reason first-seen is a marker and not the file's mtime: an item
        // that has churned through the cache is not a fresh addition.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        age(dir.path(), "ikey.seen", 30 * DAY);
        // The file itself is brand new — this is the re-download.
        let entries = collect_cache_entries(dir.path()).unwrap();
        let first_seen = entries[0].standing.first_seen;
        assert!(
            first_seen < SystemTime::now() - Duration::from_secs(29 * DAY as u64),
            "the marker, not the file, says how old the item is"
        );
    }

    #[test]
    fn a_cache_that_predates_the_marker_is_not_presented_as_brand_new() {
        // On upgrade every existing file gets stamped on the next sweep. Taking
        // that stamp at face value would make the whole cache look freshly
        // added and scramble the ordering, so the file's mtime is a floor.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        age(dir.path(), "clip.mp4", 45 * DAY);
        // `.seen` was written just now, as the first post-upgrade sweep would.

        let entries = collect_cache_entries(dir.path()).unwrap();
        assert!(
            entries[0].standing.first_seen
                < SystemTime::now() - Duration::from_secs(44 * DAY as u64),
            "an old file must not be aged from the day it was first stamped"
        );
    }

    // --- failure marking ---

    #[test]
    fn a_key_with_no_failure_is_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!retry_blocked(
            dir.path(),
            "clip",
            Duration::from_secs(3600)
        ));
    }

    #[test]
    fn a_recent_failure_blocks_a_retry_and_an_old_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        mark_failed(dir.path(), "clip");
        assert!(retry_blocked(dir.path(), "clip", Duration::from_secs(3600)));

        age(dir.path(), "clip.failed", 2 * 3600);
        assert!(!retry_blocked(
            dir.path(),
            "clip",
            Duration::from_secs(3600)
        ));
    }

    #[test]
    fn a_success_forgets_the_failure() {
        // An item that recovers must not stay blocked by the last time it did
        // not work.
        let dir = tempfile::tempdir().unwrap();
        mark_failed(dir.path(), "clip");
        clear_failed(dir.path(), "clip");
        assert!(!retry_blocked(
            dir.path(),
            "clip",
            Duration::from_secs(3600)
        ));
    }

    #[test]
    fn clearing_an_item_does_not_clear_its_failure_record() {
        // The worker clears the half-written debris of a failed attempt right
        // after recording the failure. If that took the record with it the
        // cooldown would never hold and the retry storm would be back.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        mark_failed(dir.path(), "clip");

        remove_cached_item(dir.path(), "clip");

        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
        assert!(retry_blocked(dir.path(), "clip", Duration::from_secs(3600)));
    }

    #[test]
    fn a_failure_marker_is_not_content() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        mark_failed(dir.path(), "clip");
        assert_eq!(
            cache_total(dir.path()),
            ONE_VIDEO,
            "the marker must not count toward the cap"
        );
        assert_eq!(
            find_cached_file(dir.path(), "clip").unwrap().extension(),
            Some("mp4".as_ref()),
            "and must not be served as the video"
        );
    }

    /// Files predating the current keying are ordinary eviction candidates,
    /// not a special case: they carry sentinels, count toward the cap, and —
    /// having no recorded interest key — read as never watched and unplaced.
    #[test]
    fn pre_hash_files_still_count_and_still_evict() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big-buck-bunny.mp4"), b"video").unwrap();
        // The phase-2 sentinel format: a bare selector, no interest key.
        std::fs::write(dir.path().join("big-buck-bunny.done"), H264.as_bytes()).unwrap();
        age(dir.path(), "big-buck-bunny.mp4", 30 * DAY);

        assert_eq!(cache_total(dir.path()), ONE_VIDEO);
        evict_for(dir.path(), 0, fresh_guess(dir.path()), weights());
        assert!(find_cached_file(dir.path(), "big-buck-bunny").is_none());
    }
}
