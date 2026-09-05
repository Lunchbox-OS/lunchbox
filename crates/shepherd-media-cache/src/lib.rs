//! On-disk video cache for `shepherd-media`.
//!
//! Remote library sources — YouTube URLs and plain HTTP files — are downloaded
//! to `$XDG_CACHE_HOME/shepherd/media/videos/` so a later play comes off local
//! disk: no buffering, no bandwidth, and the item stays watchable offline.
//!
//! This lives in its own crate because **two processes use it**. `shepherd-media`
//! reads it at play time and queues after a video finishes; shepherdd will
//! prefetch into it in the background, whether or not the player is running
//! (issue #127). Everything here is therefore free of the player: no libmpv, no
//! egui, nothing a daemon should not link.
//!
//! Two caching behaviors are exposed, and they differ only in what the download
//! is judged to be worth (see `shepherd_media_app::lru`):
//!
//! - [`VideoCache::queue_prefetch`] — speculative, and scored as a guess. It
//!   may recycle space held by other guesses and by content watched long enough
//!   ago to have lost its grace; it **skips** rather than displace a file it
//!   does not outrank.
//! - [`VideoCache::queue_after_play`] — the user just watched this to the end,
//!   so it is scored as a play and may displace almost anything.
//!
//! See [`key`] for why files are named after their source URL, [`lock`] for how
//! two processes stay off each other's downloads, and [`store`] for the on-disk
//! layout and eviction.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use shepherd_media_app::lru::ScoreWeights;
use shepherd_media_core::resolver::resolve_source;
use shepherd_media_core::{ClassifiedUri, Library, Source};
use tracing::{debug, warn};

pub mod download;
pub mod key;
pub mod lock;
pub mod paths;
pub mod playlist;
pub mod sponsorblock;
pub mod store;
pub mod subprocess;

pub use download::{DEFAULT_DOWNLOAD_INTERVAL, DownloadKind, RETRY_COOLDOWN};
pub use key::{content_key, interest_key, source_url};
pub use paths::media_cache_dir;
pub use playlist::{fetch_playlist, refetch_playlist, ytdlp_available};
pub use sponsorblock::{DEFAULT_API as SPONSORBLOCK_API, SponsorBlockCache};
pub use subprocess::{
    ProgramResolverFn, ScopePrefixFn, set_program_resolver_fn, set_scope_prefix_fn,
};
// The shared eviction scoring; re-exported so callers of this crate need not
// reach into `shepherd-media-app` for it.
pub use shepherd_media_app::lru::{DEFAULT_WATCHED_GRACE, Score, ScoreWeights as CacheWeights};
pub use store::{CacheState, cache_total, clear_all_failures};

use download::{DownloadRequest, download_worker};

/// Default maximum total size of the video cache.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10 GiB

/// Environment variable that overrides the configured cap.
///
/// A local override for debugging and for a `shepherd-media` run by hand — the
/// deployment mechanism is `service.media.cache_max_bytes`, which shepherdd
/// hands to both its own prefetcher and every media activity it launches.
/// Setting this on only one of the two processes sharing a cache directory will
/// have them trimming it to different sizes.
pub const MAX_BYTES_ENV: &str = "SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES";

/// Default watched grace, in whole days — the unit the config and the CLI use.
pub const DEFAULT_WATCHED_GRACE_DAYS: u64 = 30;

/// The cache cap to use: [`MAX_BYTES_ENV`] if set and parseable, else
/// `configured`.
///
/// An unparseable value falls back rather than failing — a cache is
/// best-effort, and refusing to start over a malformed debugging variable would
/// be a worse outcome than ignoring it.
pub fn max_cache_bytes(configured: u64) -> u64 {
    std::env::var(MAX_BYTES_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(configured)
}

/// A grace expressed in whole days, as the config and the CLI carry it.
pub fn grace_from_days(days: u64) -> Duration {
    Duration::from_secs(days * 24 * 60 * 60)
}

/// What [`VideoCache::queue_prefetch`] did with an item.
///
/// Returned so a caller sweeping a library can report what the sweep actually
/// amounted to. "Queued 92 items" reads the same whether 92 downloads are about
/// to start or the library has been complete for an hour, and those are the two
/// states an operator most needs to tell apart — the second looks exactly like a
/// prefetcher that has silently stopped working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueOutcome {
    /// Handed to the download worker.
    #[default]
    Queued,
    /// Already in the cache, complete.
    AlreadyCached,
    /// Skipped: it failed recently and is inside [`RETRY_COOLDOWN`].
    FailedRecently,
    /// Nothing to cache — a local file, or a URI scheme this cache does not
    /// fetch.
    NotCacheable,
}

/// Where a [`VideoCache`] stores things and how much it may use.
pub struct VideoCacheConfig {
    pub cache_dir: PathBuf,
    pub max_bytes: u64,
    /// The yt-dlp `--format` selector downloads use. Recorded in each item's
    /// sentinel, so a file downloaded under a different one can be spotted and
    /// replaced.
    pub ytdl_format: String,
    /// How eviction values what it holds. Two processes sharing a directory
    /// must agree on this; see [`grace_from_days`].
    pub weights: ScoreWeights,
    /// How long the worker waits between downloads that reached the network.
    /// Zero disables the pacing, which is what tests want.
    pub download_interval: Duration,
}

pub struct VideoCache {
    cache_dir: PathBuf,
    download_tx: mpsc::Sender<DownloadRequest>,
    ytdl_format: String,
    weights: ScoreWeights,
}

impl VideoCache {
    /// Construct a `VideoCache` over the standard cache directory, with the cap
    /// from [`max_cache_bytes`]. Returns `None` if the directory cannot be
    /// determined or created; the caller then skips caching entirely.
    ///
    /// `watched_grace` and `max_bytes` have to be passed in rather than
    /// defaulted here: this directory is shared with shepherdd's prefetcher, and
    /// two processes that disagreed about how big it may be or what is worth
    /// keeping would spend the same disk by different rules and undo each
    /// other's trims. shepherdd resolves both from policy and hands them to
    /// both. [`MAX_BYTES_ENV`] still overrides the cap locally.
    pub fn new(ytdl_format: &str, watched_grace: Duration, max_bytes: u64) -> Option<Arc<Self>> {
        Self::with_config(VideoCacheConfig {
            cache_dir: media_cache_dir("videos")?,
            max_bytes: max_cache_bytes(max_bytes),
            ytdl_format: ytdl_format.to_string(),
            weights: ScoreWeights { watched_grace },
            download_interval: DEFAULT_DOWNLOAD_INTERVAL,
        })
    }

    /// Construct a `VideoCache` over an explicit directory and cap. Spawns the
    /// background download worker.
    pub fn with_config(config: VideoCacheConfig) -> Option<Arc<Self>> {
        let VideoCacheConfig {
            cache_dir,
            max_bytes,
            ytdl_format,
            weights,
            download_interval,
        } = config;

        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            warn!(
                "could not create video cache dir {}: {e}",
                cache_dir.display()
            );
            return None;
        }

        let (tx, rx) = mpsc::channel::<DownloadRequest>();
        let dir = cache_dir.clone();
        let format = ytdl_format.clone();
        std::thread::Builder::new()
            .name("video-cache-worker".into())
            .spawn(move || download_worker(dir, rx, max_bytes, format, weights, download_interval))
            .ok()?;

        Some(Arc::new(VideoCache {
            cache_dir,
            download_tx: tx,
            ytdl_format,
            weights,
        }))
    }

    /// The directory this cache owns.
    pub fn cache_dir(&self) -> &std::path::Path {
        &self.cache_dir
    }

    /// The content key for `source` under this cache's format selector, or
    /// `None` for a local file that needs no caching.
    fn content_key_for(&self, source: &Source) -> Option<String> {
        let url = source_url(source)?;
        let selector = match &source.uri {
            ClassifiedUri::YouTube(_) => self.ytdl_format.as_str(),
            _ => "",
        };
        Some(content_key(&url, selector))
    }

    /// Return the path to a fully-downloaded cached video for `source`, or
    /// `None` if no complete file is present.
    ///
    /// A pure lookup: it does **not** record interest. Call [`Self::mark_played`]
    /// when playback actually starts, so that merely enumerating the cache —
    /// which the prefetcher does — cannot make every guess look watched.
    pub fn cached_path(&self, source: &Source) -> Option<PathBuf> {
        let key = self.content_key_for(source)?;
        store::find_cached_file(&self.cache_dir, &key)
    }

    /// Record that `source` was played, so eviction stops treating it as a
    /// replaceable guess. Applies to the video, not the rendition: watching it
    /// at one quality protects the copy cached at another.
    pub fn mark_played(&self, source: &Source) {
        if let Some(url) = source_url(source) {
            store::mark_played(&self.cache_dir, &interest_key(&url));
        }
    }

    /// How this cache values what it holds.
    pub fn weights(&self) -> ScoreWeights {
        self.weights
    }

    /// Queue a speculative background download for `source`, which sits at
    /// position `ordinal` in its library.
    ///
    /// The download is scored as a guess, so it recycles space held by other
    /// guesses and by content watched long enough ago to have lost its grace,
    /// and is dropped rather than displace a file it does not outrank.
    ///
    /// The ordinal is how eviction orders one sweep's guesses against each
    /// other: prefetch fills in display order, so a file further down the list
    /// is one nothing is about to reach. It has to be passed in because the
    /// cache directory is shared by every library on the device and has no idea
    /// where a file came from.
    ///
    /// `label` names the item in logs only; the file is named by cache key.
    pub fn queue_prefetch(&self, label: &str, source: &Source, ordinal: u32) -> QueueOutcome {
        self.enqueue(label, source, Some(ordinal), false)
    }

    /// Queue a download because `source` was just played to completion. Scored
    /// as a play, so it may displace almost anything; the worker trims back to
    /// the cap afterwards.
    pub fn queue_after_play(&self, label: &str, source: &Source) {
        self.enqueue(label, source, None, true);
    }

    /// The cooldown a speculative download observes after a failure. Exposed so
    /// callers can report a skip without re-deriving it.
    pub fn retry_cooldown() -> Duration {
        RETRY_COOLDOWN
    }

    /// Queue speculative downloads for every remote item in `library`, in
    /// display order.
    pub fn queue_all(&self, library: &Library) {
        let platform_info = shepherd_media_core::PlatformInfo::current();
        for (ordinal, item) in library.items.iter().enumerate() {
            if let Some(source) = resolve_source(item, &platform_info) {
                let _ = self.queue_prefetch(&item.id, source, ordinal as u32);
            }
        }
    }

    fn enqueue(
        &self,
        label: &str,
        source: &Source,
        ordinal: Option<u32>,
        earned: bool,
    ) -> QueueOutcome {
        let (url, kind) = match &source.uri {
            ClassifiedUri::YouTube(url) => (url.to_string(), DownloadKind::YouTube),
            ClassifiedUri::DirectHttp(url) => (url.to_string(), DownloadKind::Http),
            _ => return QueueOutcome::NotCacheable,
        };
        let key = content_key(&url, kind.selector(&self.ytdl_format));
        let interest_key = interest_key(&url);

        if store::cache_state(&self.cache_dir, &key) == CacheState::Present {
            debug!("video cache hit for {label}, skipping queue");
            // Still record that the item was offered. A library the cache is
            // already full of would otherwise never have its first sightings
            // written, and every item in it would read as brand new the day one
            // of them finally gets evicted.
            store::mark_seen(&self.cache_dir, &interest_key);
            return QueueOutcome::AlreadyCached;
        }

        // The worker checks this too, since the cooldown can expire while a
        // request sits in the queue. Checking here as well is what lets the
        // sweep report the skip rather than counting it as work it started.
        if !earned && store::retry_blocked(&self.cache_dir, &key, RETRY_COOLDOWN) {
            debug!("{label} failed recently; not queueing yet");
            store::mark_seen(&self.cache_dir, &interest_key);
            return QueueOutcome::FailedRecently;
        }

        let _ = self.download_tx.send(DownloadRequest {
            key,
            interest_key,
            label: label.to_string(),
            url,
            kind,
            ordinal,
            earned,
        });
        QueueOutcome::Queued
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_media_core::{PlayerHint, Source};
    use std::time::Duration as StdDuration;

    fn http_source(url: &str) -> Source {
        Source {
            platforms: Vec::new(),
            uri: ClassifiedUri::DirectHttp(url.parse().unwrap()),
            player_hint: Some(PlayerHint::Mpv),
        }
    }

    fn local_source(path: &str) -> Source {
        Source {
            platforms: Vec::new(),
            uri: ClassifiedUri::Local(PathBuf::from(path)),
            player_hint: Some(PlayerHint::Mpv),
        }
    }

    fn cache_in(dir: &std::path::Path) -> Arc<VideoCache> {
        VideoCache::with_config(VideoCacheConfig {
            cache_dir: dir.to_path_buf(),
            max_bytes: 1024,
            ytdl_format: "test-selector".into(),
            weights: ScoreWeights::default(),
            download_interval: StdDuration::ZERO,
        })
        .expect("cache constructs over a temp dir")
    }

    /// Commit `source` to `cache`'s directory as if it had been downloaded.
    fn commit(cache: &VideoCache, dir: &std::path::Path, source: &Source) -> PathBuf {
        let key = cache.content_key_for(source).unwrap();
        let ikey = interest_key(&source_url(source).unwrap());
        let path = dir.join(format!("{key}.mp4"));
        std::fs::write(&path, b"video").unwrap();
        store::write_done_sentinel(dir, &key, &ikey, "", Some(0)).unwrap();
        path
    }

    #[test]
    fn the_configured_cap_is_used_when_the_override_is_unset() {
        // Deliberately not touching the environment: these run in one process
        // with other tests, and a set-then-unset would race them.
        assert_eq!(max_cache_bytes(123), 123);
        assert_eq!(DEFAULT_MAX_CACHE_BYTES, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn the_day_and_duration_spellings_of_the_default_grace_agree() {
        // The config, the CLI flag, and the policy default all carry days; the
        // scoring carries a `Duration`. They must describe the same window.
        assert_eq!(
            grace_from_days(DEFAULT_WATCHED_GRACE_DAYS),
            DEFAULT_WATCHED_GRACE
        );
        assert_eq!(
            DEFAULT_WATCHED_GRACE,
            StdDuration::from_secs(30 * 24 * 60 * 60)
        );
    }

    #[test]
    fn a_local_source_is_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        assert!(cache.cached_path(&local_source("/movies/a.mp4")).is_none());
    }

    #[test]
    fn a_committed_download_is_found_by_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let source = http_source("https://example.com/a.mp4");

        assert!(cache.cached_path(&source).is_none());
        let path = commit(&cache, dir.path(), &source);
        assert_eq!(cache.cached_path(&source).unwrap(), path);
    }

    /// The collision URL keying exists to prevent: before it, both of these
    /// were `intro.mp4` in one flat directory.
    #[test]
    fn two_libraries_sharing_an_item_id_do_not_share_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let one = http_source("https://one.example/intro.mp4");
        let two = http_source("https://two.example/intro.mp4");

        commit(&cache, dir.path(), &one);

        assert!(cache.cached_path(&one).is_some());
        assert!(
            cache.cached_path(&two).is_none(),
            "the other library's item must not be served this file"
        );
    }

    #[test]
    fn a_lookup_does_not_record_interest() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let source = http_source("https://example.com/a.mp4");
        commit(&cache, dir.path(), &source);

        let _ = cache.cached_path(&source);
        let ikey = interest_key(&source_url(&source).unwrap());
        assert!(
            !dir.path().join(format!("{ikey}.played")).exists(),
            "enumerating the cache must not make every guess look watched"
        );

        cache.mark_played(&source);
        assert!(dir.path().join(format!("{ikey}.played")).exists());
    }

    /// The distinction the sweep log exists to make: a library that is already
    /// complete must not report the same thing as one that just queued 92
    /// downloads.
    #[test]
    fn queueing_reports_what_it_actually_did() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let source = http_source("https://example.com/a.mp4");

        assert_eq!(
            cache.queue_prefetch("a", &source, 0),
            QueueOutcome::Queued,
            "an uncached item is work"
        );

        commit(&cache, dir.path(), &source);
        assert_eq!(
            cache.queue_prefetch("a", &source, 0),
            QueueOutcome::AlreadyCached,
            "a complete one is not"
        );

        assert_eq!(
            cache.queue_prefetch("local", &local_source("/movies/a.mp4"), 0),
            QueueOutcome::NotCacheable
        );
    }

    #[test]
    fn an_item_in_its_failure_cooldown_reports_the_skip() {
        // Counting this as queued would hide a library that has stopped working
        // behind a log line claiming it started downloading.
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let source = http_source("https://example.com/a.mp4");
        let key = content_key(&source_url(&source).unwrap(), "");
        store::mark_failed(dir.path(), &key);

        assert_eq!(
            cache.queue_prefetch("a", &source, 0),
            QueueOutcome::FailedRecently
        );
    }

    #[test]
    fn queueing_an_already_cached_item_still_records_the_sighting() {
        // Otherwise a library the cache is already full of never gets its first
        // sightings written, and every item in it reads as brand new the day
        // one of them is finally evicted.
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let source = http_source("https://example.com/a.mp4");
        commit(&cache, dir.path(), &source);

        cache.queue_prefetch("a", &source, 0);

        let ikey = interest_key(&source_url(&source).unwrap());
        assert!(dir.path().join(format!("{ikey}.seen")).exists());
    }

    /// Interest is in the video, so watching it at one quality protects the
    /// copy cached at another — the two are separate files but one interest.
    #[test]
    fn marking_played_protects_every_quality_of_the_same_video() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://example.com/a.mp4";
        let source = http_source(url);

        let hd = VideoCache::with_config(VideoCacheConfig {
            cache_dir: dir.path().to_path_buf(),
            max_bytes: 1024,
            ytdl_format: "hd".into(),
            weights: ScoreWeights::default(),
            download_interval: StdDuration::ZERO,
        })
        .unwrap();
        // Direct-HTTP sources carry no selector, so force the YouTube path by
        // checking the keys the two caches would use for the same YouTube URL.
        let yt = |sel: &str| content_key(url, sel);
        assert_ne!(yt("hd"), yt("sd"), "quality is part of the content key");

        hd.mark_played(&source);
        let ikey = interest_key(url);
        assert!(
            dir.path().join(format!("{ikey}.played")).exists(),
            "the marker is keyed by video, not by rendition"
        );
    }
}
