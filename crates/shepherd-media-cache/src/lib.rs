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
//! Two caching behaviors are exposed:
//!
//! - [`VideoCache::queue_prefetch`] — speculative. The worker **skips** if the
//!   cache is already at capacity, so nothing the user has watched is displaced
//!   for something they have not asked for.
//! - [`VideoCache::queue_after_play`] — the user just watched this to the end.
//!   The worker downloads and then evicts LRU files to come back under the cap.
//!
//! See [`key`] for why files are named after their source URL, [`lock`] for how
//! two processes stay off each other's downloads, and [`store`] for the on-disk
//! layout and eviction.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use shepherd_media_core::resolver::resolve_source;
use shepherd_media_core::{ClassifiedUri, Library, Source};
use tracing::{debug, warn};

pub mod download;
pub mod key;
pub mod lock;
pub mod paths;
pub mod playlist;
pub mod store;

pub use download::DownloadKind;
pub use key::{content_key, interest_key, source_url};
pub use paths::media_cache_dir;
pub use playlist::{fetch_playlist, ytdlp_available};
pub use store::{CacheState, Recency, cache_total};

use download::{DownloadRequest, download_worker};

/// Default maximum total size of the video cache.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10 GiB

/// Environment variable that overrides [`DEFAULT_MAX_CACHE_BYTES`].
pub const MAX_BYTES_ENV: &str = "SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES";

/// The configured cache cap: [`MAX_BYTES_ENV`] if set and parseable, else
/// [`DEFAULT_MAX_CACHE_BYTES`].
pub fn max_cache_bytes() -> u64 {
    std::env::var(MAX_BYTES_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_CACHE_BYTES)
}

/// Where a [`VideoCache`] stores things and how much it may use.
pub struct VideoCacheConfig {
    pub cache_dir: PathBuf,
    pub max_bytes: u64,
    /// The yt-dlp `--format` selector downloads use. Recorded in each item's
    /// sentinel, so a file downloaded under a different one can be spotted and
    /// replaced.
    pub ytdl_format: String,
}

pub struct VideoCache {
    cache_dir: PathBuf,
    download_tx: mpsc::Sender<DownloadRequest>,
    ytdl_format: String,
}

impl VideoCache {
    /// Construct a `VideoCache` over the standard cache directory, with the cap
    /// from [`max_cache_bytes`]. Returns `None` if the directory cannot be
    /// determined or created; the caller then skips caching entirely.
    pub fn new(ytdl_format: &str) -> Option<Arc<Self>> {
        Self::with_config(VideoCacheConfig {
            cache_dir: media_cache_dir("videos")?,
            max_bytes: max_cache_bytes(),
            ytdl_format: ytdl_format.to_string(),
        })
    }

    /// Construct a `VideoCache` over an explicit directory and cap. Spawns the
    /// background download worker.
    pub fn with_config(config: VideoCacheConfig) -> Option<Arc<Self>> {
        let VideoCacheConfig {
            cache_dir,
            max_bytes,
            ytdl_format,
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
            .spawn(move || download_worker(dir, rx, max_bytes, format))
            .ok()?;

        Some(Arc::new(VideoCache {
            cache_dir,
            download_tx: tx,
            ytdl_format,
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

    /// Queue a speculative background download for `source`. The worker skips
    /// it if the cache is at capacity, so cached content is never displaced for
    /// something the user has not watched.
    ///
    /// `label` names the item in logs only; the file is named by cache key.
    pub fn queue_prefetch(&self, label: &str, source: &Source) {
        self.enqueue(label, source, false);
    }

    /// Queue a download because `source` was just played to completion. The
    /// worker evicts LRU files afterwards to keep the cache within the cap.
    pub fn queue_after_play(&self, label: &str, source: &Source) {
        self.enqueue(label, source, true);
    }

    /// Queue speculative downloads for every remote item in `library`.
    pub fn queue_all(&self, library: &Library) {
        let platform_info = shepherd_media_core::PlatformInfo::current();
        for item in &library.items {
            if let Some(source) = resolve_source(item, &platform_info) {
                self.queue_prefetch(&item.id, source);
            }
        }
    }

    fn enqueue(&self, label: &str, source: &Source, evict_after: bool) {
        let (url, kind) = match &source.uri {
            ClassifiedUri::YouTube(url) => (url.to_string(), DownloadKind::YouTube),
            ClassifiedUri::DirectHttp(url) => (url.to_string(), DownloadKind::Http),
            _ => return,
        };
        let key = content_key(&url, kind.selector(&self.ytdl_format));

        if store::cache_state(&self.cache_dir, &key) == CacheState::Present {
            debug!("video cache hit for {label}, skipping queue");
            return;
        }

        let _ = self.download_tx.send(DownloadRequest {
            key,
            interest_key: interest_key(&url),
            label: label.to_string(),
            url,
            kind,
            evict_after,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_media_core::{PlayerHint, Source};

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
        })
        .expect("cache constructs over a temp dir")
    }

    /// Commit `source` to `cache`'s directory as if it had been downloaded.
    fn commit(cache: &VideoCache, dir: &std::path::Path, source: &Source) -> PathBuf {
        let key = cache.content_key_for(source).unwrap();
        let ikey = interest_key(&source_url(source).unwrap());
        let path = dir.join(format!("{key}.mp4"));
        std::fs::write(&path, b"video").unwrap();
        store::write_done_sentinel(dir, &key, &ikey, "").unwrap();
        path
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
