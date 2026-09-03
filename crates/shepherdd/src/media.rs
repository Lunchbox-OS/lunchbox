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
//! The target list is rebuilt from the same read, so adding, removing, or
//! retargeting a `media` entry takes effect on the next sweep too. That is also
//! why this task is always spawned, even with no media entries configured:
//! a prefetcher that only existed when the startup policy had work for it could
//! never be handed any by a reload.

use std::path::Path;
use std::sync::Arc;

use shepherd_api::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSubject, EntryKind, Event,
    EventPayload, MediaMode, MediaQuality,
};
use shepherd_config::{MediaServiceConfig, Policy};
use shepherd_core::CoreEngine;
use shepherd_media_app::Quality;
use shepherd_media_cache::{
    QueueOutcome, SponsorBlockCache, VideoCache, fetch_playlist, ytdlp_available,
};
use shepherd_media_core::{
    ClassifiedUri, Library, PlatformInfo, build_library_from_entries, is_youtube_playlist_url,
    load_library, resolve_source,
};
use shepherd_util::EntryId;
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
#[derive(Debug, PartialEq, Eq)]
struct PrefetchTarget {
    entry_id: String,
    library: String,
    quality: MediaQuality,
    /// For a `mode = "play"` entry, the single item it launches. Browse entries
    /// leave this `None` and take the whole library.
    only_item: Option<String>,
    /// Whether this entry skips SponsorBlock segments, with its own override
    /// already resolved against the service default (issue #159). Prefetch
    /// warms the segment buckets for the videos it caches, so a library filled
    /// while online still skips when it is played offline.
    sponsorblock: bool,
}

/// Everything a sweep needs, as of the last time policy was read.
///
/// Both halves are refreshed together before each sweep — see
/// [`MediaPrefetcher::reread_policy`] — so a config reload reaches the libraries
/// and the settings in the same pass.
pub struct MediaPrefetcher {
    targets: Vec<PrefetchTarget>,
    settings: MediaServiceConfig,
}

/// What one read of the policy yields. Assembled under the engine lock and used
/// after it is dropped, so nothing that touches the disk or spawns a process
/// happens while the engine is held.
struct PolicyRead {
    settings: MediaServiceConfig,
    targets: Vec<PrefetchTarget>,
    /// `(entry id, library source)` for **every** media entry, including ones
    /// prefetch skips. The yt-dlp warning covers those too: a missing yt-dlp
    /// breaks them when a child taps the tile, not only when this task would
    /// have downloaded them.
    media_entries: Vec<(String, String)>,
}

impl MediaPrefetcher {
    /// Build a prefetcher for `policy`.
    ///
    /// Always returns one, even when nothing is configured. A task that only
    /// existed when the *startup* policy had media entries could never be
    /// handed any by a reload, and an idle sweep is a lock, a clone, and a walk
    /// of the entry list once an hour.
    pub fn from_policy(policy: &Policy) -> Self {
        let read = read_policy(policy);
        warn_about_missing_ytdlp(&read.media_entries);
        Self {
            targets: read.targets,
            settings: read.settings,
        }
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
        diagnostics: crate::diagnostics::DiagnosticPublisher,
    ) {
        let mut session_active = false;
        let mut online = true;

        info!(
            libraries = self.targets.len(),
            "background media prefetch started"
        );

        // Fires immediately, which is the startup sweep.
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.reread_policy(&engine).await;
                    if self.may_sweep(session_active, online) {
                        self.sweep(&diagnostics).await;
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
                            self.reread_policy(&engine).await;
                            if self.may_sweep(session_active, online) {
                                self.sweep(&diagnostics).await;
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

    /// Re-read policy, replacing both the settings and the target list.
    ///
    /// The read happens under the lock and everything that could block happens
    /// after it: [`read_policy`] is pure string work, while the yt-dlp probe it
    /// feeds spawns a process and the library check behind it touches the disk.
    /// A sweep then runs on the values this leaves behind.
    async fn reread_policy(&mut self, engine: &Arc<Mutex<CoreEngine>>) {
        let read = { read_policy(engine.lock().await.policy()) };

        if read.targets != self.targets {
            info!(
                libraries = read.targets.len(),
                "media prefetch targets changed"
            );
            // Re-warn on a change, not every sweep: an hourly reminder that
            // yt-dlp is missing is noise, but a reload that *adds* a YouTube
            // entry to a device without it should say so.
            warn_about_missing_ytdlp(&read.media_entries);
        }
        self.targets = read.targets;
        self.settings = read.settings;
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
    async fn sweep(&self, diagnostics: &crate::diagnostics::DiagnosticPublisher) {
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
            let cache_max_bytes = self.settings.cache_max_bytes;
            // `Some` only when this entry skips segments; the prefetcher makes
            // no request otherwise, exactly as the player does not.
            let sponsorblock_api = target
                .sponsorblock
                .then(|| self.settings.sponsorblock.api.clone());

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
                    cache_max_bytes,
                    sponsorblock_api.as_deref(),
                )
            })
            .await;

            // Logged whatever the outcome, including "nothing to do". A sweep
            // that queues nothing because the library is already complete is
            // indistinguishable, from the outside, from a prefetcher that has
            // silently stopped working — and the old line said "queued 92
            // items" in both cases, because it counted items *offered* rather
            // than downloads actually started.
            match queued {
                Ok(Ok(tally)) => {
                    info!(
                        entry = %target.entry_id,
                        total = tally.total,
                        queued = tally.queued,
                        cached = tally.cached,
                        cooling = tally.cooling,
                        warmed = tally.warmed,
                        "media prefetch sweep"
                    );
                    // The same site that notices the failure notices the
                    // recovery, so a library that starts parsing again clears
                    // itself without anything else having to remember.
                    diagnostics.clear(
                        DiagnosticCode::MediaLibraryUnreadable,
                        &DiagnosticSubject::Entry {
                            entry_id: EntryId::new(target.entry_id.clone()),
                        },
                    );
                }
                Ok(Err(reason)) => diagnostics.raise(Diagnostic {
                    code: DiagnosticCode::MediaLibraryUnreadable,
                    subject: DiagnosticSubject::Entry {
                        entry_id: EntryId::new(target.entry_id.clone()),
                    },
                    severity: DiagnosticSeverity::Warning,
                    message: format!("This activity's media library could not be read: {reason}"),
                    remedy: Some(
                        "Check the `library` path or URL on this activity, and that the \
                         file parses."
                            .to_string(),
                    ),
                    since: shepherd_util::now(),
                }),
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

/// Resolve everything a sweep needs from `policy`.
///
/// Deliberately pure and cheap — no disk, no processes — because it runs with
/// the engine lock held.
fn read_policy(policy: &Policy) -> PolicyRead {
    let mut targets = Vec::new();
    let mut media_entries = Vec::new();
    for entry in &policy.entries {
        let EntryKind::Media {
            library,
            mode,
            item,
            quality,
            prefetch,
            sponsorblock,
            ..
        } = &entry.kind
        else {
            continue;
        };
        media_entries.push((entry.id.as_str().to_string(), library.clone()));

        // An entry the admin has switched off entirely should not be consuming
        // disk on the child's behalf. Schedule is deliberately *not* consulted:
        // an activity outside its window today is exactly the one worth having
        // ready tomorrow.
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
            // A direct-play entry launches exactly one item; caching the rest of
            // its library would download things this activity can never reach.
            only_item: match mode {
                MediaMode::Play => item.clone(),
                MediaMode::Browse => None,
            },
            sponsorblock: sponsorblock.unwrap_or(policy.service.media.sponsorblock.enabled)
                && !policy.service.media.sponsorblock.categories.is_empty(),
        });
    }

    PolicyRead {
        settings: policy.service.media.clone(),
        targets,
        media_entries,
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

/// What one library's sweep amounted to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SweepTally {
    /// Items handed to the download worker.
    queued: usize,
    /// Items already complete in the cache.
    cached: usize,
    /// Items skipped because a recent download failed.
    cooling: usize,
    /// Items in the library, including ones with nothing to cache.
    total: usize,
    /// Items whose SponsorBlock bucket was fetched or confirmed present, so
    /// they still skip once the device is offline (issue #159).
    warmed: usize,
}

/// Load `library_source` and queue every remote item in it. Returns what the
/// sweep amounted to, or `None` if the library could not be read.
fn queue_library(
    entry_id: &str,
    library_source: &str,
    ytdl_format: &str,
    only_item: Option<&str>,
    watched_grace: Duration,
    cache_max_bytes: u64,
    sponsorblock_api: Option<&str>,
) -> Result<SweepTally, String> {
    let library = match load_prefetch_library(library_source) {
        Ok(l) => l,
        Err(e) => {
            warn!(entry = %entry_id, error = %e, "could not read media library for prefetch");
            return Err(e.to_string());
        }
    };

    // One cache per format selector: the selector is part of the content key,
    // so two entries at different qualities cache side by side.
    // No cache is not a library problem, so it reports an empty sweep rather
    // than an error: conflating the two is what the `Option` did before.
    let Some(cache) = VideoCache::new(ytdl_format, watched_grace, cache_max_bytes) else {
        return Ok(SweepTally::default());
    };

    let platform_info = PlatformInfo::current();
    let segments = sponsorblock_api.map(SponsorBlockCache::new);
    let mut tally = SweepTally::default();
    // The ordinal is the item's place in the library as browse would show it,
    // which is how eviction orders one sweep's guesses against each other — a
    // file further down the list is one nothing is about to reach. It comes
    // from the full walk, not from the filtered count, so a `mode = "play"`
    // entry's single item keeps the position it actually holds.
    for (ordinal, item) in library.items.iter().enumerate() {
        if only_item.is_some_and(|wanted| wanted != item.id) {
            continue;
        }
        tally.total += 1;
        if let Some(source) = resolve_source(item, &platform_info)
            && shepherd_media_cache::source_url(source).is_some()
        {
            let outcome = cache.queue_prefetch(&item.id, source, ordinal as u32);
            match outcome {
                QueueOutcome::Queued => tally.queued += 1,
                QueueOutcome::AlreadyCached => tally.cached += 1,
                QueueOutcome::FailedRecently => tally.cooling += 1,
                QueueOutcome::NotCacheable => {}
            }

            // Warm the segments for the videos this cache is actually going to
            // hold. A video nobody is downloading can look its segments up when
            // it plays, because playing it needs the network anyway; a cached
            // one is the case that has to work with the network gone.
            if let Some(segments) = segments.as_ref()
                && matches!(outcome, QueueOutcome::Queued | QueueOutcome::AlreadyCached)
                && let ClassifiedUri::YouTube(url) = &source.uri
                && let Some(video_id) = shepherd_media_core::uri::youtube_video_id(url)
                && segments.warm(&video_id)
            {
                tally.warmed += 1;
            }
        }
    }
    Ok(tally)
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
fn warn_about_missing_ytdlp(media_entries: &[(String, String)]) {
    let youtube_entries: Vec<&str> = media_entries
        .iter()
        .filter(|(_, library)| {
            is_youtube_playlist_url(library) || library_references_youtube(library)
        })
        .map(|(id, _)| id.as_str())
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
/// Free bytes on the filesystem holding `path`, for the diagnostic registry
/// (issue #143). Blocking; call from `spawn_blocking`.
pub fn free_space(path: &Path) -> Option<u64> {
    free_space_bytes(path)
}

/// Ids of every media entry that references YouTube, by playlist URL or by a
/// source inside its library.
///
/// Shared with the diagnostic registry (issue #143) so "which activities does a
/// missing yt-dlp break" has one answer rather than two that can disagree.
pub fn youtube_entry_ids(policy: &Policy) -> Vec<shepherd_util::EntryId> {
    policy
        .entries
        .iter()
        .filter(|e| match &e.kind {
            EntryKind::Media { library, .. } => {
                is_youtube_playlist_url(library) || library_references_youtube(library)
            }
            _ => false,
        })
        .map(|e| e.id.clone())
        .collect()
}

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

    /// `shepherd-config` compiles to wasm for the config editor, so it cannot
    /// depend on the media crates and carries its own copy of the SponsorBlock
    /// category names. This is the guard: shepherdd links both, and a category
    /// added to (or renamed in) the core must reach the config that validates
    /// what a parent typed, or one of the two would silently stop matching.
    #[test]
    fn the_configs_sponsorblock_categories_match_the_cores() {
        let core: Vec<&str> = shepherd_media_core::sponsorblock::Category::all()
            .iter()
            .filter(|c| c.is_skippable())
            .map(|c| c.as_str())
            .collect();
        assert_eq!(core, shepherd_config::SPONSORBLOCK_CATEGORIES);

        // And the shipped default must be a subset of what the service knows.
        for category in shepherd_config::DEFAULT_SPONSORBLOCK_CATEGORIES {
            assert!(core.contains(category), "unknown default `{category}`");
        }
    }

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

    /// A policy from TOML, for exercising `read_policy`'s resolution.
    fn policy_from(config: &str) -> Policy {
        shepherd_config::parse_config(config).expect("test config parses")
    }

    const MEDIA_ENTRY: &str = r#"
        [[entries]]
        id = "movies"
        label = "Movies"
        [entries.kind]
        type = "media"
        library = "/etc/shepherd/movies.toml"
    "#;

    /// Prefetch warms the segment buckets only for entries that skip. An entry
    /// nobody enabled must leave the daemon as silent as the player.
    #[test]
    fn prefetch_warms_segments_only_where_sponsorblock_is_on() {
        let off = policy_from(&format!("config_version = 1\n{MEDIA_ENTRY}"));
        assert!(!read_policy(&off).targets[0].sponsorblock, "off by default");

        let on = policy_from(&format!(
            "config_version = 1\n[service.media.sponsorblock]\nenabled = true\n{MEDIA_ENTRY}"
        ));
        assert!(read_policy(&on).targets[0].sponsorblock);

        // An entry that opted out is not warmed either, so its bucket is never
        // requested on its behalf.
        let opted_out = policy_from(&format!(
            "config_version = 1\n[service.media.sponsorblock]\nenabled = true\n{MEDIA_ENTRY}\nsponsorblock = false\n"
        ));
        assert!(!read_policy(&opted_out).targets[0].sponsorblock);
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

    /// A policy with one browse-mode `media` entry per id, built by parsing
    /// real TOML rather than assembling structs — the resolution under test
    /// reads fields a hand-built `Policy` could drift away from.
    fn policy_with_media(entries: &[(&str, bool)]) -> Policy {
        let mut toml = String::from("config_version = 1\n");
        for (id, enabled) in entries {
            toml.push_str(&format!(
                r#"
[[entries]]
id = "{id}"
label = "{id}"
disabled = {disabled}
[entries.kind]
type = "media"
library = "~/{id}.toml"
mode = "browse"
"#,
                id = id,
                disabled = !enabled,
            ));
        }
        shepherd_config::parse_config(&toml).expect("test policy parses")
    }

    #[test]
    fn a_prefetcher_exists_even_with_nothing_configured() {
        // It has to: a task that only existed when the startup policy had media
        // entries could never be handed any by a reload.
        let prefetcher = MediaPrefetcher::from_policy(&policy_with_media(&[]));
        assert!(prefetcher.targets.is_empty());
    }

    #[test]
    fn re_reading_policy_picks_up_an_added_library() {
        let before = read_policy(&policy_with_media(&[("movies", true)]));
        let after = read_policy(&policy_with_media(&[("movies", true), ("shows", true)]));

        assert_eq!(before.targets.len(), 1);
        assert_eq!(after.targets.len(), 2);
        assert_ne!(
            before.targets, after.targets,
            "the change has to be visible, or the re-warn and the log never fire"
        );
    }

    #[test]
    fn re_reading_policy_drops_a_removed_library() {
        assert!(read_policy(&policy_with_media(&[])).targets.is_empty());
    }

    #[test]
    fn a_disabled_entry_is_not_a_target_but_is_still_a_media_entry() {
        // The yt-dlp warning covers entries prefetch skips: a missing yt-dlp
        // breaks them when a child taps the tile, not only when this task would
        // have downloaded them.
        let read = read_policy(&policy_with_media(&[("movies", false)]));
        assert!(read.targets.is_empty());
        assert_eq!(read.media_entries.len(), 1);
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
