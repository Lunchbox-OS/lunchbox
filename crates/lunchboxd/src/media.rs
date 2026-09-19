//! Background media prefetch (issue #127).
//!
//! Caching a video used to require `lunchbox-media` to be running: browse mode
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
//!   checks lunchboxd already performs rather than probing again.
//! - **Displace anything recently watched.** A speculative download is scored
//!   as the guess it is, so it can recycle space held by other guesses and by
//!   content watched long enough ago to have lost its grace
//!   (`service.media.watched_grace_days`) — never a film watched last week. See
//!   `lunchbox-media-cache`.
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

use lunchbox_api::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSubject, EntryKind, Event,
    EventPayload, MediaMode, MediaQuality,
};
use lunchbox_config::{MediaServiceConfig, Policy};
use lunchbox_core::CoreEngine;
use lunchbox_media_app::Quality;
use lunchbox_media_cache::{
    QueueOutcome, SponsorBlockCache, VideoCache, fetch_playlist, refetch_playlist, ytdlp_available,
};
use lunchbox_media_core::{
    ClassifiedUri, Library, PlatformInfo, build_library_from_entries, is_youtube_playlist_url,
    load_library, resolve_source,
};
use lunchbox_util::EntryId;
use std::collections::HashSet;

use tokio::sync::{Mutex, broadcast, mpsc};
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

/// Why a sweep is happening, which is the only thing that differs between the
/// two (issue #165).
///
/// A scheduled sweep is unattended work on a timer, so it trusts every cache it
/// meets: the playlist listing is good for six hours and a SponsorBlock bucket
/// for a day, and re-asking sooner would spend a household's bandwidth on
/// answers that have almost certainly not changed. A manual sweep exists
/// *because* somebody believes one of those answers is out of date, so it skips
/// the TTLs and re-asks. Everything after the fetch — what to queue, what a
/// download may evict, the disk floor — is identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepMode {
    /// The hourly timer, a session ending, the internet coming back.
    Scheduled,
    /// An administrator pressed refresh.
    Manual,
}

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
        mut refresh_rx: mpsc::Receiver<()>,
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
                        self.sweep(&diagnostics, SweepMode::Scheduled).await;
                    }
                }
                // An administrator pressed refresh (issue #165). Deliberately
                // *not* gated on `session_active`: the press almost always
                // happens with the child in front of the device asking where
                // the new video is, which is precisely the state the
                // background rule declines to work in. The ticker is reset
                // afterwards so the hourly sweep does not follow a minute
                // later and redo the same walk.
                Some(()) = refresh_rx.recv() => {
                    self.reread_policy(&engine).await;
                    self.refresh(&diagnostics, online).await;
                    ticker.reset();
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
                                self.sweep(&diagnostics, SweepMode::Scheduled).await;
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

    /// Service an administrator's refresh request (issue #165).
    ///
    /// The two gates a scheduled sweep applies that this one keeps are the two
    /// a button cannot argue with: prefetch switched off is a household
    /// decision, and an offline device has nothing to re-fetch. Both are
    /// reported as diagnostics rather than swallowed — the request came from a
    /// person watching for something to happen, and "nothing happened, here is
    /// why" is the only honest answer a fire-and-forget RPC can give them.
    async fn refresh(&self, diagnostics: &crate::diagnostics::DiagnosticPublisher, online: bool) {
        if !online {
            diagnostics.raise(Diagnostic {
                code: DiagnosticCode::MediaRefreshFailed,
                subject: DiagnosticSubject::Service,
                severity: DiagnosticSeverity::Warning,
                message: "A media refresh was requested, but this device has no internet \
                          connection, so nothing could be fetched."
                    .to_string(),
                remedy: Some(
                    "Reconnect this device to the internet and refresh again.".to_string(),
                ),
                since: lunchbox_util::now(),
            });
            return;
        }

        if !self.settings.prefetch {
            diagnostics.raise(Diagnostic {
                code: DiagnosticCode::MediaRefreshFailed,
                subject: DiagnosticSubject::Service,
                severity: DiagnosticSeverity::Warning,
                message: "A media refresh was requested, but background media downloads are \
                          turned off on this device."
                    .to_string(),
                remedy: Some(
                    "Set `service.media.prefetch = true` in the configuration, reload it, and \
                     refresh again."
                        .to_string(),
                ),
                since: lunchbox_util::now(),
            });
            return;
        }

        diagnostics.clear(
            DiagnosticCode::MediaRefreshFailed,
            &DiagnosticSubject::Service,
        );

        // Before the walk, not per library: the failure markers are keyed by
        // content in one directory shared by every library on the device, so
        // there is no per-library subset to clear — and clearing inside the
        // loop would wipe a failure the previous library's downloads had just
        // recorded.
        let cleared = tokio::task::spawn_blocking(clear_download_cooldowns)
            .await
            .unwrap_or(0);
        info!(
            libraries = self.targets.len(),
            cooldowns_cleared = cleared,
            "media refresh requested"
        );

        self.sweep(diagnostics, SweepMode::Manual).await;
    }

    /// One pass over every configured library.
    async fn sweep(&self, diagnostics: &crate::diagnostics::DiagnosticPublisher, mode: SweepMode) {
        for target in &self.targets {
            if !self.have_disk_headroom() {
                return;
            }
            let entry_id = target.entry_id.clone();
            let library_source = target.library.clone();
            let ytdl_format = ytdl_format_for(target.quality).to_string();
            let only_item = target.only_item.clone();
            let watched_grace =
                lunchbox_media_cache::grace_from_days(self.settings.watched_grace_days);
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
                    mode,
                )
            })
            .await;

            // Logged whatever the outcome, including "nothing to do". A sweep
            // that queues nothing because the library is already complete is
            // indistinguishable, from the outside, from a prefetcher that has
            // silently stopped working — and the old line said "queued 92
            // items" in both cases, because it counted items *offered* rather
            // than downloads actually started.
            let subject = DiagnosticSubject::Entry {
                entry_id: EntryId::new(target.entry_id.clone()),
            };
            match queued {
                Ok(Ok(report)) => {
                    let tally = report.tally;
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
                    diagnostics.clear(DiagnosticCode::MediaLibraryUnreadable, &subject);
                    self.report_refresh(diagnostics, target, &subject, mode, report.stale);
                }
                Ok(Err(reason)) => {
                    diagnostics.raise(Diagnostic {
                        code: DiagnosticCode::MediaLibraryUnreadable,
                        subject: subject.clone(),
                        severity: DiagnosticSeverity::Warning,
                        message: format!(
                            "This activity's media library could not be read: {reason}"
                        ),
                        remedy: Some(
                            "Check the `library` path or URL on this activity, and that the \
                             file parses."
                                .to_string(),
                        ),
                        since: lunchbox_util::now(),
                    });
                    // One problem, one diagnostic: an unreadable library is
                    // already named above, and saying it twice under two codes
                    // would just make the list longer.
                    self.report_refresh(diagnostics, target, &subject, mode, None);
                }
                Err(e) => warn!(entry = %target.entry_id, error = %e, "media prefetch task failed"),
            }
        }
    }

    /// Raise or clear this entry's [`DiagnosticCode::MediaRefreshFailed`] after
    /// a manual sweep. A scheduled sweep touches it in neither direction: it
    /// reads from the caches a refresh exists to bypass, so it can neither
    /// confirm nor deny that the source is reachable.
    fn report_refresh(
        &self,
        diagnostics: &crate::diagnostics::DiagnosticPublisher,
        target: &PrefetchTarget,
        subject: &DiagnosticSubject,
        mode: SweepMode,
        stale: Option<String>,
    ) {
        if mode != SweepMode::Manual {
            return;
        }
        match stale {
            Some(reason) => {
                warn!(entry = %target.entry_id, reason = %reason, "media refresh could not reach the source");
                diagnostics.raise(Diagnostic {
                    code: DiagnosticCode::MediaRefreshFailed,
                    subject: subject.clone(),
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "The last media refresh could not re-fetch this activity, so it is \
                         still showing what was cached: {reason}"
                    ),
                    remedy: Some(
                        "Check this device's internet connection and the activity's \
                         `library` URL, then refresh again."
                            .to_string(),
                    ),
                    since: lunchbox_util::now(),
                });
            }
            None => diagnostics.clear(DiagnosticCode::MediaRefreshFailed, subject),
        }
    }

    /// Whether the cache's filesystem has room to spare. Warns once per sweep
    /// when it does not — a full disk on a kiosk is a support call, and the
    /// cache cap alone does not prevent one.
    fn have_disk_headroom(&self) -> bool {
        if self.settings.free_space_floor_bytes == 0 {
            return true;
        }
        let Some(dir) = lunchbox_media_cache::media_cache_dir("videos") else {
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

/// One library's sweep, plus whether an administrator's refresh actually
/// reached what it went to re-fetch (issue #165).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct SweepReport {
    tally: SweepTally,
    /// `Some(reason)` when a manual refresh fell back to cached data — the
    /// playlist would not load, the segment service did not answer. Always
    /// `None` on a scheduled sweep, which is reading from those caches by
    /// design rather than settling for them.
    stale: Option<String>,
}

/// What loading a library produced.
struct LoadedLibrary {
    library: Library,
    /// Why the listing below is the cached one rather than a fresh fetch. See
    /// [`SweepReport::stale`].
    stale: Option<String>,
}

/// Forget every download cooldown in the shared video cache directory, so a
/// following sweep retries items a failure is currently holding back. Returns
/// how many were cleared, or 0 when there is no cache directory to clear.
fn clear_download_cooldowns() -> usize {
    lunchbox_media_cache::media_cache_dir("videos")
        .map(|dir| lunchbox_media_cache::clear_all_failures(&dir))
        .unwrap_or(0)
}

/// Load `library_source` and queue every remote item in it. Returns what the
/// sweep amounted to, or the reason the library could not be read.
#[allow(clippy::too_many_arguments)]
fn queue_library(
    entry_id: &str,
    library_source: &str,
    ytdl_format: &str,
    only_item: Option<&str>,
    watched_grace: Duration,
    cache_max_bytes: u64,
    sponsorblock_api: Option<&str>,
    mode: SweepMode,
) -> Result<SweepReport, String> {
    let LoadedLibrary { library, mut stale } = match load_prefetch_library(library_source, mode) {
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
        return Ok(SweepReport::default());
    };

    let platform_info = PlatformInfo::current();
    let segments = sponsorblock_api.map(SponsorBlockCache::new);
    // A manual refresh re-asks for buckets the TTL would have served from
    // disk, so — unlike `warm`, which is free the second time a bucket is
    // wanted — it has to remember which prefixes it has already paid for.
    // One bucket covers around a hundred videos.
    let mut refreshed_prefixes: HashSet<String> = HashSet::new();
    let mut segment_failure: Option<String> = None;
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
            && lunchbox_media_cache::source_url(source).is_some()
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
                && let Some(video_id) = lunchbox_media_core::uri::youtube_video_id(url)
            {
                let warmed = match mode {
                    SweepMode::Scheduled => segments.warm(&video_id),
                    // A refresh is here because somebody thinks the segments
                    // are out of date, so the day-long TTL is exactly what it
                    // skips. A bucket that will not re-fetch is not an error
                    // the child ever sees — the cached one is still on disk
                    // and still skips — so it is recorded and reported once
                    // for the library rather than aborting the sweep.
                    SweepMode::Manual => {
                        if refreshed_prefixes.insert(segments.prefix_for(&video_id)) {
                            match segments.refresh(&video_id) {
                                Ok(()) => true,
                                Err(e) => {
                                    segment_failure.get_or_insert(e);
                                    false
                                }
                            }
                        } else {
                            true
                        }
                    }
                };
                if warmed {
                    tally.warmed += 1;
                }
            }
        }
    }

    // The library listing is what a parent is looking at, so a failure there
    // is the one worth naming; segments only ever downgrade to yesterday's.
    if stale.is_none()
        && let Some(e) = segment_failure
    {
        stale = Some(format!(
            "the sponsor-segment service could not be reached ({e})"
        ));
    }

    Ok(SweepReport { tally, stale })
}

/// Load a library from either a file path or a YouTube playlist URL. Mirrors
/// what `lunchbox-media` does at launch, minus the CLI ordering — prefetch
/// order follows the library's own, which is what browse shows by default.
///
/// A manual refresh bypasses the six-hour playlist cache; a file-backed library
/// has nothing to bypass, since it is re-read from disk either way. When the
/// forced fetch fails it falls back to the cached listing rather than failing
/// the sweep: the parent's activity is still there and still playable, and the
/// difference between "could not refresh" and "could not read" is exactly what
/// the returned reason carries up.
fn load_prefetch_library(source: &str, mode: SweepMode) -> Result<LoadedLibrary, String> {
    if is_youtube_playlist_url(source) {
        let (info, stale) = match mode {
            SweepMode::Scheduled => (fetch_playlist(source)?, None),
            SweepMode::Manual => match refetch_playlist(source) {
                Ok(info) => (info, None),
                Err(e) => (fetch_playlist(source)?, Some(e)),
            },
        };
        Ok(LoadedLibrary {
            library: build_library_from_entries(
                source,
                info.title,
                info.playlist_id.as_deref(),
                &info.entries,
            ),
            stale,
        })
    } else {
        load_library(Path::new(source))
            .map(|library| LoadedLibrary {
                library,
                stale: None,
            })
            .map_err(|e| e.to_string())
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
         they will fail to load or play. Install it with `lunchbox-admin media-deps install`."
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
            .is_some_and(|s| matches!(s.uri, lunchbox_media_core::ClassifiedUri::YouTube(_)))
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
pub fn youtube_entry_ids(policy: &Policy) -> Vec<lunchbox_util::EntryId> {
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
/// the `lunchbox-media` command line — otherwise the daemon would look for the
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

    /// `lunchbox-config` compiles to wasm for the config editor, so it cannot
    /// depend on the media crates and carries its own copy of the SponsorBlock
    /// category names. This is the guard: lunchboxd links both, and a category
    /// added to (or renamed in) the core must reach the config that validates
    /// what a parent typed, or one of the two would silently stop matching.
    #[test]
    fn the_configs_sponsorblock_categories_match_the_cores() {
        let core: Vec<&str> = lunchbox_media_core::sponsorblock::Category::all()
            .iter()
            .filter(|c| c.is_skippable())
            .map(|c| c.as_str())
            .collect();
        let config: Vec<&str> = lunchbox_config::RawSponsorBlockCategory::ALL
            .iter()
            .map(|c| c.as_str())
            .collect();
        assert_eq!(core, config);

        // And the shipped default must be a subset of what the service knows.
        for category in lunchbox_config::DEFAULT_SPONSORBLOCK_CATEGORIES {
            assert!(
                core.contains(&category.as_str()),
                "unknown default `{}`",
                category.as_str()
            );
        }
    }

    /// The selector lunchboxd prefetches with must be the one `lunchbox-media`
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
        lunchbox_config::parse_config(config).expect("test config parses")
    }

    const MEDIA_ENTRY: &str = r#"
        [[entries]]
        id = "movies"
        label = "Movies"
        [entries.kind]
        type = "media"
        library = "/etc/lunchbox/movies.toml"
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
            lunchbox_media_cache::grace_from_days(prefetcher.settings.watched_grace_days),
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

    // --- media refresh (issue #165) ---------------------------------------

    /// A publisher writing into a registry the test can read back.
    fn publisher() -> (
        crate::diagnostics::DiagnosticPublisher,
        Arc<crate::diagnostics::DiagnosticRegistry>,
    ) {
        let registry = Arc::new(crate::diagnostics::DiagnosticRegistry::new());
        let (changed, _rx) = tokio::sync::mpsc::unbounded_channel();
        (
            crate::diagnostics::DiagnosticPublisher::new(registry.clone(), changed),
            registry,
        )
    }

    fn codes(registry: &crate::diagnostics::DiagnosticRegistry) -> Vec<DiagnosticCode> {
        registry.current().items.iter().map(|d| d.code).collect()
    }

    /// The button has to answer for itself. Both gates a refresh keeps end in a
    /// diagnostic rather than a silent return, because the RPC has already told
    /// the caller "accepted" and this is the only channel left to say otherwise.
    #[tokio::test]
    async fn a_refresh_with_no_internet_says_so() {
        let (diagnostics, registry) = publisher();
        prefetcher_with(MediaServiceConfig::default())
            .refresh(&diagnostics, false)
            .await;
        assert_eq!(codes(&registry), vec![DiagnosticCode::MediaRefreshFailed]);
    }

    #[tokio::test]
    async fn a_refresh_with_prefetch_switched_off_says_so() {
        let (diagnostics, registry) = publisher();
        prefetcher_with(MediaServiceConfig {
            prefetch: false,
            ..Default::default()
        })
        .refresh(&diagnostics, true)
        .await;
        assert_eq!(codes(&registry), vec![DiagnosticCode::MediaRefreshFailed]);
    }

    /// A refresh that got through clears the last one's complaint, so the
    /// health screen is about now rather than about the last time it failed.
    #[tokio::test]
    async fn a_refresh_that_gets_through_clears_the_previous_failure() {
        let (diagnostics, registry) = publisher();
        let prefetcher = prefetcher_with(MediaServiceConfig::default());

        prefetcher.refresh(&diagnostics, false).await;
        assert_eq!(codes(&registry), vec![DiagnosticCode::MediaRefreshFailed]);

        // No targets, so the sweep after the gates is a no-op walk.
        prefetcher.refresh(&diagnostics, true).await;
        assert!(codes(&registry).is_empty());
    }

    fn target() -> PrefetchTarget {
        PrefetchTarget {
            entry_id: "movies".into(),
            library: "/etc/lunchbox/movies.toml".into(),
            quality: MediaQuality::Best,
            only_item: None,
            sponsorblock: false,
        }
    }

    #[test]
    fn a_manual_sweep_that_fell_back_to_cached_data_raises_and_a_clean_one_clears() {
        let (diagnostics, registry) = publisher();
        let prefetcher = prefetcher_with(MediaServiceConfig::default());
        let target = target();
        let subject = DiagnosticSubject::Entry {
            entry_id: EntryId::new(target.entry_id.clone()),
        };

        prefetcher.report_refresh(
            &diagnostics,
            &target,
            &subject,
            SweepMode::Manual,
            Some("yt-dlp could not reach the playlist".into()),
        );
        assert_eq!(codes(&registry), vec![DiagnosticCode::MediaRefreshFailed]);

        prefetcher.report_refresh(&diagnostics, &target, &subject, SweepMode::Manual, None);
        assert!(codes(&registry).is_empty());
    }

    /// A scheduled sweep reads from exactly the caches a refresh exists to
    /// bypass, so a clean one is no evidence the source is reachable and must
    /// not clear a standing complaint about it.
    #[test]
    fn a_scheduled_sweep_leaves_the_refresh_diagnostic_alone() {
        let (diagnostics, registry) = publisher();
        let prefetcher = prefetcher_with(MediaServiceConfig::default());
        let target = target();
        let subject = DiagnosticSubject::Entry {
            entry_id: EntryId::new(target.entry_id.clone()),
        };

        prefetcher.report_refresh(
            &diagnostics,
            &target,
            &subject,
            SweepMode::Manual,
            Some("offline".into()),
        );
        prefetcher.report_refresh(&diagnostics, &target, &subject, SweepMode::Scheduled, None);
        assert_eq!(codes(&registry), vec![DiagnosticCode::MediaRefreshFailed]);
    }

    /// A file-backed library is re-read from disk on every sweep, so a refresh
    /// has no cache to bypass and nothing to report as stale.
    #[test]
    fn a_file_library_reads_the_same_in_both_modes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movies.toml");
        std::fs::write(
            &path,
            r#"schema_version = 1
library_id = "movies"
title = "Movies"

[[items]]
id = "intro"
title = "Intro"
kind = "video"

[[items.sources]]
platforms = ["linux"]
uri = "file:///media/intro.mp4"
"#,
        )
        .unwrap();
        let source = path.to_str().unwrap();

        for mode in [SweepMode::Scheduled, SweepMode::Manual] {
            let loaded = load_prefetch_library(source, mode).expect("the library parses");
            assert_eq!(loaded.library.items.len(), 1);
            assert!(loaded.stale.is_none(), "nothing was served from a cache");
        }
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
        lunchbox_config::parse_config(&toml).expect("test policy parses")
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
