//! A [`PlayerHandle`] wrapper that plays from the video cache when it can, and
//! fills the cache when it can't.
//!
//! The cache itself lives in `lunchbox-media-cache`, which lunchboxd shares
//! (issue #127); this is the player-side half that cannot: it needs a
//! `PlayerHandle` to wrap, and a daemon has no player.
//!
//! Two behaviors:
//!
//! - **On play**, a cached file is substituted for the remote source, and its
//!   recency is bumped so LRU eviction keeps frequently-watched items warm.
//! - **On natural end of file**, an uncached item is queued for download, so
//!   the next play comes off local disk.
//!
//! Nothing here waits on a download. When lunchboxd is prefetching the same
//! item, the cache refuses the duplicate claim and this side simply streams the
//! remote source — a child should never sit in front of a progress bar they
//! cannot see.

use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::sync::Arc;

use lunchbox_media_cache::{VideoCache, source_url};
use lunchbox_media_core::resolver::resolve_source;
use lunchbox_media_core::{ClassifiedUri, Library, PlayerError, PlayerEvent, PlayerHandle, Source};
use tracing::debug;

pub struct CachingPlayer {
    inner: Box<dyn PlayerHandle>,
    cache: Arc<VideoCache>,
    /// Source URL → library item id, used only to name items in log lines.
    /// The cache keys files by URL hash, so nothing on disk depends on this.
    url_to_label: HashMap<String, String>,
    /// The (label, source) of the currently-playing uncached remote item, for
    /// the queue-on-EOF path.
    last_played: Option<(String, Source)>,
}

impl CachingPlayer {
    pub fn new(inner: Box<dyn PlayerHandle>, cache: Arc<VideoCache>, library: &Library) -> Self {
        let platform_info = lunchbox_media_core::PlatformInfo::current();
        let mut url_to_label = HashMap::new();
        for item in &library.items {
            if let Some(source) = resolve_source(item, &platform_info)
                && let Some(url) = source_url(source)
            {
                url_to_label.insert(url, item.id.clone());
            }
        }
        Self {
            inner,
            cache,
            url_to_label,
            last_played: None,
        }
    }

    /// The library item id for a source, for logs. Falls back to the URL when
    /// the source isn't one of this library's items.
    fn label_for(&self, source: &Source) -> String {
        source_url(source)
            .map(|url| self.url_to_label.get(&url).cloned().unwrap_or(url))
            .unwrap_or_else(|| "<local>".to_string())
    }
}

impl PlayerHandle for CachingPlayer {
    fn play(&mut self, source: &Source) -> Result<(), PlayerError> {
        if let Some(path) = self.cache.cached_path(source) {
            debug!("playing from video cache: {}", path.display());
            // Record interest, so eviction stops treating this as a replaceable
            // guess and starts protecting it against the next prefetch.
            self.cache.mark_played(source);
            // Already cached — no need to re-download after EOF.
            self.last_played = None;
            let local = Source {
                platforms: source.platforms.clone(),
                uri: ClassifiedUri::Local(path),
                player_hint: source.player_hint,
            };
            self.inner.play(&local)
        } else {
            // Not cached; record for the download-after-EOF path.
            self.last_played = Some((self.label_for(source), source.clone()));
            self.inner.play(source)
        }
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.last_played = None;
        self.inner.stop()
    }

    fn is_playing(&self) -> bool {
        self.inner.is_playing()
    }

    fn poll_event(&mut self) -> Option<PlayerEvent> {
        let event = self.inner.poll_event()?;
        if matches!(event, PlayerEvent::EndOfFile)
            && let Some((ref label, ref source)) = self.last_played
        {
            // The user watched this to completion — cache it so the next play
            // comes from local storage, evicting LRU if needed.
            self.cache.queue_after_play(label, source);
        }
        Some(event)
    }

    fn set_paused(&mut self, paused: bool) -> Result<(), PlayerError> {
        self.inner.set_paused(paused)
    }

    fn is_paused(&self) -> bool {
        self.inner.is_paused()
    }

    fn seek_relative(&mut self, delta_seconds: f64) -> Result<(), PlayerError> {
        self.inner.seek_relative(delta_seconds)
    }

    fn seek_absolute(&mut self, seconds: f64) -> Result<(), PlayerError> {
        self.inner.seek_absolute(seconds)
    }

    fn position(&self) -> Option<f64> {
        self.inner.position()
    }

    fn duration(&self) -> Option<f64> {
        self.inner.duration()
    }

    fn video_size(&self) -> Option<(i64, i64)> {
        self.inner.video_size()
    }

    fn set_volume(&mut self, percent: f64) -> Result<(), PlayerError> {
        self.inner.set_volume(percent)
    }

    fn volume(&self) -> Option<f64> {
        self.inner.volume()
    }

    fn set_start_position(&mut self, seconds: Option<f64>) {
        self.inner.set_start_position(seconds);
    }

    fn bind_gl(
        &mut self,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        native_display: Option<lunchbox_media_core::NativeDisplay>,
    ) -> Result<(), PlayerError> {
        self.inner.bind_gl(get_proc_address, native_display)
    }

    fn render(&self, fbo: i32, width: i32, height: i32) -> Result<(), PlayerError> {
        self.inner.render(fbo, width, height)
    }

    fn set_redraw_callback(&mut self, cb: Box<dyn Fn() + Send + Sync + 'static>) {
        self.inner.set_redraw_callback(cb);
    }
}
