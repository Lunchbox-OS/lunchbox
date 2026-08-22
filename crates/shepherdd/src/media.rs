//! Background media prefetch (issue #127).
//!
//! Caching a video used to require `shepherd-media` to be running: browse mode
//! queued the library at launch, and closing the activity stopped the
//! downloads. So the first time a child opened anything it buffered, and a
//! library only became watchable offline if they happened to leave the grid up
//! long enough. This task moves that work to the daemon, where it can happen
//! while nobody is watching.
//!
//! What it will not do:
//!
//! - **Run during an activity.** A download competing with a game — or with the
//!   very video the child is watching — spends their CPU and bandwidth on
//!   content nobody has asked for yet. Overridable with
//!   `service.media.prefetch_while_session_active`.
//! - **Run while the internet is down**, which it learns from the connectivity
//!   checks shepherdd already performs rather than probing again.
//! - **Displace anything recently watched.** A speculative download is scored
//!   as the guess it is, so it can recycle space held by other guesses and by
//!   content watched long enough ago to have lost its grace
//!   (`service.media.watched_grace_days`) — never a film watched last week. See
//!   `shepherd-media-cache`.
//! - **Fill the disk.** Below `service.media.free_space_floor_bytes` it warns
//!   and stops. The cache's own cap bounds the cache, not the volume it sits
//!   on, and these devices have small ones.
//!
//! `[service.media]` is re-read from the engine at the top of every sweep
//! rather than snapshotted at construction. The eviction grace is the reason:
//! the launch path hands each spawned activity the *current*
//! `watched_grace_days`, so a prefetcher still running on the value from
//! startup would value the shared cache directory differently from the player
//! writing to it, and the two would undo each other's trims. The lock is taken
//! for the length of a clone, never across a download.
//!
//! The *targets* are still snapshotted: picking up an added or removed `media`
//! entry means rebuilding the list, which is a restart-on-reload change rather
//! than this one.

use std::path::Path;
use std::sync::Arc;

use shepherd_api::{EntryKind, Event, EventPayload, MediaMode, MediaQuality};
use shepherd_config::{MediaServiceConfig, Policy};
use shepherd_core::CoreEngine;
use shepherd_media_app::Quality;
use shepherd_media_cache::{VideoCache, fetch_playlist, ytdlp_available};
use shepherd_media_core::{
    Library, PlatformInfo, build_library_from_entries, is_youtube_playlist_url, load_library,
    resolve_source,
};
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, sleep};
use tracing::{debug, info, warn};

/// How long to wait after a session ends before resuming prefetch. Long enough
/// that stopping one activity and starting another doesn't spend the gap
/// downloading.
const RESUME_DELAY: Duration = Duration::from_secs(30);

/// How often to re-walk the configured libraries. A playlist gains videos, and
/// its metadata cache expires every 6 hours; nothing else here changes on its
/// own.
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// One library to keep cached, resolved from a `media` entry.
struct PrefetchTarget {
    entry_id: String,
    library: String,
    quality: MediaQuality,
    /// For a `mode = "play"` entry, the single item it launches. Browse entries
    /// leave this `None` and take the whole library.
    only_item: Option<String>,
}

/// The libraries to keep cached, and the service settings that govern how.
///
/// The targets are resolved once: they come from the entry list, and picking up
/// an added or removed `media` entry would mean rebuilding this task. The
/// settings are not — see [`MediaPrefetcher::refreshed_settings`].
pub struct MediaPrefetcher {
    targets: Vec<PrefetchTarget>,
    /// `[service.media]` as of the last sweep. Seeded at construction so the
    /// startup sweep has something to run on before the first refresh.
    settings: MediaServiceConfig,
}

impl MediaPrefetcher {
    /// Build a prefetcher for `policy`, or `None` when there is nothing to do:
    /// no media entries, or prefetch switched off.
    ///
    /// Also the point where the yt-dlp warning is raised, because it is the
    /// only place that knows both what is configured and what is installed.
    pub fn from_policy(policy: &Policy) -> Option<Self> {
        let mut targets = Vec::new();
        for entry in &policy.entries {
            let EntryKind::Media {
                library,
                mode,
                item,
                quality,
                prefetch,
                ..
            } = &entry.kind
            else {
                continue;
            };
            // An entry the admin has switched off entirely should not be
            // consuming disk on the child's behalf. Schedule is deliberately
            // *not* consulted: an activity outside its window today is exactly
            // the one worth having ready tomorrow.
            if entry.disabled {
                debug!(entry = %entry.id.as_str(), "skipping prefetch for a disabled entry");
                continue;
            }
            if !prefetch.unwrap_or(policy.service.media.prefetch) {
                debug!(entry = %entry.id.as_str(), "prefetch opted out for this entry");
                continue;
            }
            targets.push(PrefetchTarget {
                entry_id: entry.id.as_str().to_string(),
                library: expand_tilde(library),
                quality: *quality,
                // A direct-play entry launches exactly one item; caching the
                // rest of its library would download things this activity can
                // never reach.
                only_item: match mode {
                    MediaMode::Play => item.clone(),
                    MediaMode::Browse => None,
                },
            });
        }

        warn_about_missing_ytdlp(policy);

        if targets.is_empty() {
            return None;
        }
        Some(Self {
            targets,
            settings: policy.service.media.clone(),
        })
    }

    /// Run until shutdown, sweeping the configured libraries whenever the
    /// device is idle and online.
    ///
    /// Events update state; they do not each trigger a sweep. The bus carries
    /// routine traffic (state snapshots, volume, availability), so sweeping on
    /// every event would walk every library several times a second — which is
    /// exactly what an earlier version of this did.
    pub async fn run(
        mut self,
        engine: Arc<Mutex<CoreEngine>>,
        mut events: broadcast::Receiver<Event>,
    ) {
        let mut session_active = false;
        let mut online = true;

        // Fires immediately, which is the startup sweep.
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.settings = Self::refreshed_settings(&engine).await;
                    if self.may_sweep(session_active, online) {
                        self.sweep().await;
                    }
                }
                received = events.recv() => match received {
                    Ok(event) => {
                        // Only a transition back into "allowed" is worth an
                        // extra sweep; everything else just updates state and
                        // waits for the next tick.
                        let resume = match &event.payload {
                            EventPayload::SessionStarted { .. } => {
                                session_active = true;
                                false
                            }
                            EventPayload::SessionEnded { .. } => {
                                session_active = false;
                                // Don't pounce the moment an activity closes;
                                // the child may be picking the next one.
                                sleep(RESUME_DELAY).await;
                                true
                            }
                            EventPayload::InternetStatusChanged { available, .. } => {
                                let came_back = *available && !online;
                                online = *available;
                                came_back
                            }
                            _ => false,
                        };
                        if resume {
                            self.settings = Self::refreshed_settings(&engine).await;
                            if self.may_sweep(session_active, online) {
                                self.sweep().await;
                                ticker.reset();
                            }
                        }
                    }
                    // Lagged: our view of session/online state may be stale.
                    // The next event corrects it, and a wrong guess costs at
                    // most one sweep.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!("media prefetch missed {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("event bus closed; stopping media prefetch");
                        return;
                    }
                },
            }
        }
    }

    /// `[service.media]` as the engine currently holds it.
    ///
    /// Cloned under the lock and returned by value: the settings are then used
    /// for the whole sweep, which shells out to yt-dlp and blocks on downloads,
    /// and none of that may happen with the engine held.
    async fn refreshed_settings(engine: &Arc<Mutex<CoreEngine>>) -> MediaServiceConfig {
        engine.lock().await.policy().service.media.clone()
    }

    fn may_sweep(&self, session_active: bool, online: bool) -> bool {
        // A reload that switches prefetch off has to be able to stop a task
        // that is already running, not just prevent the next one starting.
        if !self.settings.prefetch {
            return false;
        }
        if !online {
            return false;
        }
        !session_active || self.settings.prefetch_while_session_active
    }

    /// One pass over every configured library.
    async fn sweep(&self) {
        for target in &self.targets {
            if !self.have_disk_headroom() {
                return;
            }
            let entry_id = target.entry_id.clone();
            let library_source = target.library.clone();
            let ytdl_format = ytdl_format_for(target.quality).to_string();
            let only_item = target.only_item.clone();
            let watched_grace =
                shepherd_media_cache::grace_from_days(self.settings.watched_grace_days);

            // Library loading shells out to yt-dlp for playlists and reads
            // files otherwise; queueing hands work to the cache's own thread.
            // Neither belongs on the async runtime.
            let queued = tokio::task::spawn_blocking(move || {
                queue_library(
                    &entry_id,
                    &library_source,
                    &ytdl_format,
                    only_item.as_deref(),
                    watched_grace,
                )
            })
            .await;

            match queued {
                Ok(Some(count)) if count > 0 => {
                    info!(
                        entry = %target.entry_id,
                        items = count,
                        "queued media items for background download"
                    );
                }
                Ok(_) => {}
                Err(e) => warn!(entry = %target.entry_id, error = %e, "media prefetch task failed"),
            }
        }
    }

    /// Whether the cache's filesystem has room to spare. Warns once per sweep
    /// when it does not — a full disk on a kiosk is a support call, and the
    /// cache cap alone does not prevent one.
    fn have_disk_headroom(&self) -> bool {
        if self.settings.free_space_floor_bytes == 0 {
            return true;
        }
        let Some(dir) = shepherd_media_cache::media_cache_dir("videos") else {
            return true;
        };
        let Some(free) = free_space_bytes(&dir) else {
            // Unknown is not a reason to stop; the cache cap still applies.
            return true;
        };
        if free < self.settings.free_space_floor_bytes {
            warn!(
                free_mb = free / (1024 * 1024),
                floor_mb = self.settings.free_space_floor_bytes / (1024 * 1024),
                path = %dir.display(),
                "media prefetch paused: free disk space is below the configured floor"
            );
            return false;
        }
        true
    }
}

/// The yt-dlp format selector for a configured quality.
///
/// `MediaQuality` is the config/wire spelling and `Quality` is the media
/// crates' — the same boundary the host adapter crosses to build `--quality`.
/// The match is exhaustive on purpose: a new preset must be mapped here rather
/// than silently prefetching at the wrong resolution.
fn ytdl_format_for(quality: MediaQuality) -> &'static str {
    match quality {
        MediaQuality::Best => Quality::Best,
        MediaQuality::Q1080 => Quality::Q1080,
        MediaQuality::Q720 => Quality::Q720,
        MediaQuality::Q480 => Quality::Q480,
    }
    .ytdl_format()
}

/// Load `library_source` and queue every remote item in it. Returns the number
/// of items queued, or `None` if the library could not be read.
fn queue_library(
    entry_id: &str,
    library_source: &str,
    ytdl_format: &str,
    only_item: Option<&str>,
    watched_grace: Duration,
) -> Option<usize> {
    let library = match load_prefetch_library(library_source) {
        Ok(l) => l,
        Err(e) => {
            warn!(entry = %entry_id, error = %e, "could not read media library for prefetch");
            return None;
        }
    };

    // One cache per format selector: the selector is part of the content key,
    // so two entries at different qualities cache side by side.
    let cache = VideoCache::new(ytdl_format, watched_grace)?;

    let platform_info = PlatformInfo::current();
    let mut queued = 0;
    // The ordinal is the item's place in the library as browse would show it,
    // which is how eviction orders one sweep's guesses against each other — a
    // file further down the list is one nothing is about to reach. It comes
    // from the full walk, not from the filtered count, so a `mode = "play"`
    // entry's single item keeps the position it actually holds.
    for (ordinal, item) in library.items.iter().enumerate() {
        if only_item.is_some_and(|wanted| wanted != item.id) {
            continue;
        }
        if let Some(source) = resolve_source(item, &platform_info)
            && shepherd_media_cache::source_url(source).is_some()
        {
            cache.queue_prefetch(&item.id, source, ordinal as u32);
            queued += 1;
        }
    }
    Some(queued)
}

/// Load a library from either a file path or a YouTube playlist URL. Mirrors
/// what `shepherd-media` does at launch, minus the CLI ordering — prefetch
/// order follows the library's own, which is what browse shows by default.
fn load_prefetch_library(source: &str) -> Result<Library, String> {
    if is_youtube_playlist_url(source) {
        let info = fetch_playlist(source)?;
        Ok(build_library_from_entries(
            source,
            info.title,
            info.playlist_id.as_deref(),
            &info.entries,
        ))
    } else {
        load_library(Path::new(source)).map_err(|e| e.to_string())
    }
}

/// Warn when something in the policy needs `yt-dlp` and it isn't installed.
///
/// Without this the failure is invisible until a child taps a tile and the
/// activity dies: the library loads, the grid paints, and every YouTube item in
/// it is unplayable.
fn warn_about_missing_ytdlp(policy: &Policy) {
    let youtube_entries: Vec<&str> = policy
        .entries
        .iter()
        .filter(|e| match &e.kind {
            EntryKind::Media { library, .. } => {
                is_youtube_playlist_url(library) || library_references_youtube(library)
            }
            _ => false,
        })
        .map(|e| e.id.as_str())
        .collect();

    if youtube_entries.is_empty() || ytdlp_available() {
        return;
    }
    warn!(
        entries = ?youtube_entries,
        "yt-dlp is not installed, but these media activities reference YouTube; \
         they will fail to load or play. Install it with `shepherd-admin media-deps install`."
    );
}

/// Whether a library file has any YouTube source. Best-effort: an unreadable
/// library is somebody else's warning, not this one's.
fn library_references_youtube(library_source: &str) -> bool {
    let Ok(library) = load_library(Path::new(&expand_tilde(library_source))) else {
        return false;
    };
    let platform_info = PlatformInfo::current();
    library.items.iter().any(|item| {
        resolve_source(item, &platform_info)
            .is_some_and(|s| matches!(s.uri, shepherd_media_core::ClassifiedUri::YouTube(_)))
    })
}

/// Free bytes on the filesystem holding `path`, walking up to the nearest
/// existing ancestor when the cache directory has not been created yet.
fn free_space_bytes(path: &Path) -> Option<u64> {
    let mut probe = path;
    loop {
        if probe.exists() {
            let stat = nix::sys::statvfs::statvfs(probe).ok()?;
            return Some(stat.blocks_available() as u64 * stat.fragment_size() as u64);
        }
        probe = probe.parent()?;
    }
}

/// Expand a leading `~/`, matching what the host adapter does when it builds
/// the `shepherd-media` command line — otherwise the daemon would look for the
/// library somewhere the player never does.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selector shepherdd prefetches with must be the one `shepherd-media`
    /// plays with, or every prefetched file lands under a content key the
    /// player never looks up.
    #[test]
    fn prefetch_selectors_match_the_players() {
        for (api, media) in [
            (MediaQuality::Best, Quality::Best),
            (MediaQuality::Q1080, Quality::Q1080),
            (MediaQuality::Q720, Quality::Q720),
            (MediaQuality::Q480, Quality::Q480),
        ] {
            assert_eq!(ytdl_format_for(api), media.ytdl_format());
        }
    }

    #[test]
    fn tilde_expands_only_for_paths() {
        unsafe { std::env::set_var("HOME", "/home/test") };
        assert_eq!(expand_tilde("~/movies.toml"), "/home/test/movies.toml");
        assert_eq!(expand_tilde("/etc/movies.toml"), "/etc/movies.toml");
        // A URL has no leading `~/`, so it passes through untouched.
        let url = "https://www.youtube.com/playlist?list=PL1";
        assert_eq!(expand_tilde(url), url);
    }

    /// A prefetcher with no targets, carrying `settings`. Enough to exercise
    /// the gates, which is where refreshed settings have to take effect.
    fn prefetcher_with(settings: MediaServiceConfig) -> MediaPrefetcher {
        MediaPrefetcher {
            targets: Vec::new(),
            settings,
        }
    }

    #[test]
    fn the_sweep_gates_read_the_current_settings_not_the_startup_ones() {
        // The whole point of refreshing per sweep: a reload has to be able to
        // stop or unblock a task that is already running.
        let mut settings = MediaServiceConfig::default();
        assert!(prefetcher_with(settings.clone()).may_sweep(false, true));

        settings.prefetch = false;
        assert!(
            !prefetcher_with(settings.clone()).may_sweep(false, true),
            "switching prefetch off must stop a running prefetcher, not just prevent the next one"
        );

        settings.prefetch = true;
        assert!(!prefetcher_with(settings.clone()).may_sweep(true, true));
        settings.prefetch_while_session_active = true;
        assert!(prefetcher_with(settings.clone()).may_sweep(true, true));

        // Offline is not overridable: there is nothing to download.
        assert!(!prefetcher_with(settings).may_sweep(false, false));
    }

    #[test]
    fn the_grace_a_sweep_spends_comes_from_the_current_settings() {
        // The divergence this refresh exists to close: the launch path hands
        // each spawned activity the *current* `watched_grace_days`, so a
        // prefetcher still on the startup value would value the shared cache
        // directory differently from the player writing to it.
        let prefetcher = prefetcher_with(MediaServiceConfig {
            watched_grace_days: 90,
            ..Default::default()
        });
        assert_eq!(
            shepherd_media_cache::grace_from_days(prefetcher.settings.watched_grace_days),
            Duration::from_secs(90 * 24 * 60 * 60)
        );
    }

    #[test]
    fn the_disk_floor_is_read_from_the_current_settings_too() {
        assert!(
            prefetcher_with(MediaServiceConfig {
                free_space_floor_bytes: 0,
                ..Default::default()
            })
            .have_disk_headroom(),
            "a zero floor disables the check"
        );
        // A floor no real volume can satisfy must block the sweep.
        assert!(
            !prefetcher_with(MediaServiceConfig {
                free_space_floor_bytes: u64::MAX,
                ..Default::default()
            })
            .have_disk_headroom()
        );
    }

    #[test]
    fn free_space_walks_up_to_an_existing_ancestor() {
        // The cache directory may not exist yet on a first boot; the check must
        // still report the volume rather than silently disable itself.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("videos").join("not-created-yet");
        assert!(free_space_bytes(&missing).is_some());
    }
}
