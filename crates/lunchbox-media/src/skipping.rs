//! Skipping SponsorBlock segments during playback (issue #159).
//!
//! The decisions all live in `lunchbox_media_core::sponsorblock`, and the fetch
//! and its cache in `lunchbox_media_cache`. What is left — and what is here — is
//! the timing: a UI thread that must not block, a lookup that takes a network
//! round-trip, and a duration that mpv does not know until after the file is
//! open.
//!
//! So [`SkipWatcher`] is a small state machine over three moments:
//!
//! 1. **An item starts.** Its YouTube video id (if it has one) goes to a worker
//!    thread, which fetches the bucket and hands it back over a channel. A cache
//!    hit takes no measurable time; a cold fetch takes as long as it takes, and
//!    the video plays normally in the meantime.
//! 2. **The bucket arrives and a duration exists.** Only then can the
//!    submissions be judged, because every one of them is checked against the
//!    duration of the file actually loaded. Either can land first.
//! 3. **Every frame after that**, the position goes to the skipper and a seek
//!    may come back.
//!
//! A watcher exists at all only when a parent enabled the feature. With no
//! categories configured the whole thing is `None` from
//! [`crate::main`](crate) down, no worker starts, and nothing reaches the
//! network — which is the behaviour the default-off setting has to mean.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use lunchbox_media_cache::SponsorBlockCache;
use lunchbox_media_core::sponsorblock::{
    Category, RawSegment, SegmentSkipper, Skip, parse_bucket, plan_skips,
};
use lunchbox_media_core::{ClassifiedUri, Item, PlatformInfo, resolve_source, uri};
use tracing::{debug, warn};

/// How far a reported duration must move before the plan is rebuilt. Well under
/// the tolerance the duration filter itself applies, so a jitter of a few
/// milliseconds does not churn, and far below a real correction.
const DURATION_EPSILON: f64 = 0.25;

/// Per-playback SponsorBlock state: what is being fetched, and what to skip.
pub struct SkipWatcher {
    cache: Arc<SponsorBlockCache>,
    categories: Vec<Category>,
    /// The video whose bucket is in flight or in hand.
    video_id: Option<String>,
    /// Set while a worker thread is fetching; taken when it answers.
    pending: Option<Receiver<Option<String>>>,
    /// The submissions, parsed once on arrival. Kept as the parsed form because
    /// the plan is rebuilt whenever the duration changes, and re-parsing 40 KB
    /// of JSON on a frame loop to do it would not be free.
    submissions: Option<Vec<RawSegment>>,
    skipper: Option<SegmentSkipper>,
    /// The duration the current plan was built against, so a later, different
    /// one rebuilds it. mpv's first answer for a network stream is not always
    /// its last, and a plan latched to a wrong duration is a plan that skips
    /// nothing for the rest of the video.
    planned_for: Option<f64>,
}

impl SkipWatcher {
    /// A watcher over `categories`, or `None` if none are enabled.
    ///
    /// The `None` is the off switch, and it is deliberately shaped as an absent
    /// object rather than an empty list: there is then no code path that could
    /// contact the service by accident.
    pub fn new(cache: SponsorBlockCache, categories: Vec<Category>) -> Option<Self> {
        if categories.is_empty() {
            return None;
        }
        debug!(
            categories = ?categories.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "SponsorBlock skipping is on"
        );
        Some(Self {
            cache: Arc::new(cache),
            categories,
            video_id: None,
            pending: None,
            submissions: None,
            skipper: None,
            planned_for: None,
        })
    }

    /// An item has started playing. Begins a lookup if it is a YouTube video.
    ///
    /// The id comes from the *library item*, not from whatever the player was
    /// handed: a cached copy plays from a local path, and it is still the same
    /// video with the same segments.
    pub fn note_item_started(&mut self, item: &Item) {
        let video_id = youtube_video_id(item);
        // Re-opening the same video (a retry, or the viewer coming back to it)
        // reuses the bucket already in hand; only the skipper's memory of what
        // the viewer has seen is reset.
        if video_id.is_some() && video_id == self.video_id {
            self.skipper = None;
            self.planned_for = None;
            return;
        }

        self.video_id = video_id;
        self.pending = None;
        self.submissions = None;
        self.skipper = None;
        self.planned_for = None;

        let Some(video_id) = self.video_id.clone() else {
            return;
        };

        // Off the UI thread: this may go to the network, and a frame loop that
        // waits on it would stutter the video it is trying to improve.
        let (tx, rx) = std::sync::mpsc::channel();
        let cache = self.cache.clone();
        let spawned = std::thread::Builder::new()
            .name("sponsorblock".into())
            .spawn(move || {
                let bucket = cache.bucket(&video_id);
                // The receiver is gone whenever playback moved on first, which
                // is ordinary — the lookup simply lost the race.
                let _ = tx.send(bucket);
            });
        match spawned {
            Ok(_) => self.pending = Some(rx),
            Err(e) => warn!("could not start the SponsorBlock lookup: {e}"),
        }
    }

    /// Playback stopped. Drops the plan but keeps the submissions, so returning
    /// to the same video does not fetch again.
    pub fn note_stopped(&mut self) {
        self.skipper = None;
        self.planned_for = None;
    }

    /// Feed the current playback position; seek to what comes back.
    ///
    /// Call every frame while playing. `duration` is what the player reports,
    /// which is `None` until the file is open — until then there is nothing to
    /// judge submissions against and this does nothing but collect the bucket.
    pub fn poll(&mut self, position: Option<f64>, duration: Option<f64>) -> Option<Skip> {
        self.collect_pending();

        let duration = duration?;
        // Rebuild when the duration changes, not just once. Every submission is
        // judged against the duration of the file that is playing, and mpv can
        // report a provisional one before the real one for a network stream —
        // building the plan once, from whatever arrived first, silently disabled
        // skipping for the rest of the video whenever that happened.
        if self
            .planned_for
            .is_none_or(|had| (had - duration).abs() > DURATION_EPSILON)
        {
            self.build_plan(duration);
        }
        self.skipper.as_mut()?.on_position(position?)
    }

    /// Take the worker's answer if it has one, and parse it.
    fn collect_pending(&mut self) {
        let Some(rx) = self.pending.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(bucket) => {
                self.pending = None;
                let (Some(bucket), Some(video_id)) = (bucket, self.video_id.as_ref()) else {
                    return;
                };
                match parse_bucket(&bucket, video_id) {
                    Ok(raw) => self.submissions = Some(raw),
                    Err(e) => warn!(video_id, "could not parse a SponsorBlock bucket: {e}"),
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                // The worker died without sending. Nothing to skip, and nothing
                // worth retrying inside one playback.
                self.pending = None;
            }
        }
    }

    /// Resolve the submissions against this file's duration, once both are in
    /// hand.
    ///
    /// Rebuilding forgets which spans the viewer has already been skipped past,
    /// which is correct: a duration that changed is a different timeline, and
    /// the spans it produces are not the ones that were consumed.
    fn build_plan(&mut self, duration: f64) {
        let (Some(raw), Some(video_id)) = (self.submissions.as_ref(), self.video_id.as_ref())
        else {
            return;
        };
        let segments = plan_skips(raw, duration, &self.categories);
        debug!(
            video_id,
            duration,
            submissions = raw.len(),
            segments = segments.len(),
            "planned SponsorBlock skips"
        );
        self.skipper = Some(SegmentSkipper::new(segments, duration));
        self.planned_for = Some(duration);
    }
}

/// The YouTube video id behind a library item, if it has one.
fn youtube_video_id(item: &Item) -> Option<String> {
    let source = resolve_source(item, &PlatformInfo::current())?;
    match &source.uri {
        ClassifiedUri::YouTube(url) => uri::youtube_video_id(url),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_media_core::{ItemKind, Platform, PlayerHint, Source};
    use url::Url;

    /// One sponsor submission over 30s-60s of a 600s video.
    fn sponsor_submission() -> Vec<RawSegment> {
        parse_bucket(
            r#"[{"videoID":"v","segments":[{"category":"sponsor","actionType":"skip",
               "segment":[30,60],"UUID":"u","videoDuration":600.0,"locked":0,"votes":4,
               "description":""}]}]"#,
            "v",
        )
        .expect("fixture parses")
    }

    fn item(uri: ClassifiedUri) -> Item {
        Item {
            id: "one".into(),
            title: "One".into(),
            kind: ItemKind::Video,
            category: None,
            poster: None,
            duration_seconds: None,
            sources: vec![Source {
                platforms: vec![Platform::Any],
                uri,
                player_hint: Some(PlayerHint::Mpv),
            }],
        }
    }

    #[test]
    fn a_youtube_item_yields_its_video_id() {
        let url = Url::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap();
        assert_eq!(
            youtube_video_id(&item(ClassifiedUri::YouTube(url))).as_deref(),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn other_sources_have_no_video_id() {
        let http = Url::parse("https://example.com/a.mp4").unwrap();
        assert!(youtube_video_id(&item(ClassifiedUri::DirectHttp(http))).is_none());
        assert!(youtube_video_id(&item(ClassifiedUri::Local("/tmp/a.mp4".into()))).is_none());
    }

    /// The off switch. Nothing downstream can reach the network without a
    /// watcher, so this is the test that the default configuration is silent.
    #[test]
    fn no_enabled_categories_means_no_watcher() {
        let cache = SponsorBlockCache::with_dir(None, lunchbox_media_cache::SPONSORBLOCK_API);
        assert!(SkipWatcher::new(cache, Vec::new()).is_none());
    }

    /// A watcher whose lookup has not answered yet must not skip, seek, or
    /// block — the video just plays.
    #[test]
    fn nothing_is_skipped_before_the_bucket_arrives() {
        let cache = SponsorBlockCache::with_dir(None, lunchbox_media_cache::SPONSORBLOCK_API);
        let mut watcher = SkipWatcher::new(cache, vec![Category::Sponsor]).unwrap();
        assert!(watcher.poll(Some(1.0), Some(600.0)).is_none());
    }

    /// A non-YouTube item never starts a lookup at all.
    #[test]
    fn a_local_item_starts_no_lookup() {
        let cache = SponsorBlockCache::with_dir(None, lunchbox_media_cache::SPONSORBLOCK_API);
        let mut watcher = SkipWatcher::new(cache, vec![Category::Sponsor]).unwrap();
        watcher.note_item_started(&item(ClassifiedUri::Local("/tmp/a.mp4".into())));
        assert!(watcher.pending.is_none());
        assert!(watcher.video_id.is_none());
    }

    /// Once a plan exists, a position inside a segment produces the seek.
    #[test]
    fn a_planned_segment_is_skipped() {
        let cache = SponsorBlockCache::with_dir(None, lunchbox_media_cache::SPONSORBLOCK_API);
        let mut watcher = SkipWatcher::new(cache, vec![Category::Sponsor]).unwrap();
        watcher.video_id = Some("v".into());
        watcher.submissions = Some(sponsor_submission());

        // No duration yet: nothing to judge against, so nothing happens.
        assert!(watcher.poll(Some(31.0), None).is_none());

        let skip = watcher
            .poll(Some(31.0), Some(600.0))
            .expect("inside a span");
        assert_eq!(skip.target, 60.0);
        assert_eq!(skip.category, Category::Sponsor);
    }

    /// The bug this rebuild exists for: mpv can report a provisional duration
    /// for a network stream before the real one. A plan built once, from
    /// whatever arrived first, judged every submission against the wrong
    /// timeline and then skipped nothing for the rest of the video.
    #[test]
    fn a_corrected_duration_rebuilds_the_plan() {
        let cache = SponsorBlockCache::with_dir(None, lunchbox_media_cache::SPONSORBLOCK_API);
        let mut watcher = SkipWatcher::new(cache, vec![Category::Sponsor]).unwrap();
        watcher.video_id = Some("v".into());
        watcher.submissions = Some(sponsor_submission());

        // A duration nothing can be judged against: the submission is for a
        // 600s cut, so it is refused and the plan is empty.
        assert!(watcher.poll(Some(31.0), Some(120.0)).is_none());
        assert!(watcher.skipper.as_ref().unwrap().is_empty());

        // The real duration arrives a frame later, and the span is skipped.
        let skip = watcher
            .poll(Some(31.0), Some(600.0))
            .expect("the corrected duration must be planned against");
        assert_eq!(skip.target, 60.0);
    }

    /// ...but an unchanged duration must not rebuild, or every frame would
    /// forget which spans the viewer has already been skipped past.
    #[test]
    fn a_steady_duration_keeps_what_the_viewer_has_seen() {
        let cache = SponsorBlockCache::with_dir(None, lunchbox_media_cache::SPONSORBLOCK_API);
        let mut watcher = SkipWatcher::new(cache, vec![Category::Sponsor]).unwrap();
        watcher.video_id = Some("v".into());
        watcher.submissions = Some(sponsor_submission());

        assert!(watcher.poll(Some(31.0), Some(600.0)).is_some());
        // The viewer rewinds into the span they were just skipped past.
        assert!(watcher.poll(Some(45.0), Some(600.0)).is_none());
        // And a duration that only jitters is still the same timeline.
        assert!(watcher.poll(Some(46.0), Some(600.05)).is_none());
    }

    /// Replaying the same video re-arms the skipper without re-fetching.
    #[test]
    fn restarting_the_same_video_keeps_the_bucket_and_skips_again() {
        let cache = SponsorBlockCache::with_dir(None, lunchbox_media_cache::SPONSORBLOCK_API);
        let mut watcher = SkipWatcher::new(cache, vec![Category::Sponsor]).unwrap();
        let url = Url::parse("https://www.youtube.com/watch?v=v").unwrap();
        let it = item(ClassifiedUri::YouTube(url));

        watcher.video_id = Some("v".into());
        watcher.submissions = Some(sponsor_submission());
        assert!(watcher.poll(Some(31.0), Some(600.0)).is_some());
        assert!(
            watcher.poll(Some(31.0), Some(600.0)).is_none(),
            "skipped once"
        );

        watcher.note_item_started(&it);
        assert!(
            watcher.pending.is_none(),
            "no second fetch for the same video"
        );
        assert!(watcher.submissions.is_some());
        assert!(watcher.poll(Some(31.0), Some(600.0)).is_some());
    }
}
