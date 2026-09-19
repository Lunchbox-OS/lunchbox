//! Skipping SponsorBlock segments on Android (issue #159).
//!
//! The Linux binary's half of this is `lunchbox-media`'s `skipping.rs`, and
//! everything that decides anything is shared: the wire format, the filtering
//! and the skip state machine are `lunchbox_media_core::sponsorblock`, the disk
//! cache is `lunchbox_media_app::BucketStore`, and the request URL is built by
//! the core. What is left here is what is genuinely different — where the cache
//! directory is, and that the fetch uses this crate's `ureq` — plus the same
//! small state machine over the moments a player passes through.
//!
//! Android caches nothing about YouTube videos (see `video_cache.rs`), so there
//! is no offline case to serve: a YouTube video that plays at all has a network,
//! and the lookup rides along with it. The bucket cache still earns its place by
//! making the second play of anything free.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use lunchbox_media_app::BucketStore;
use lunchbox_media_app::sponsorblock::DEFAULT_TTL;
use lunchbox_media_core::sponsorblock::{
    Category, RawSegment, SegmentSkipper, Skip, bucket_url, parse_bucket, plan_skips,
};

/// The public SponsorBlock instance.
pub const DEFAULT_API: &str = "https://sponsor.ajay.app";

/// Connect and read timeout, matching the Linux side: something is on screen
/// waiting, so this fails fast and skips nothing rather than stalling.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

const USER_AGENT: &str = concat!("lunchbox-media/", env!("CARGO_PKG_VERSION"));

/// Bucket lookups against app-private storage, refreshed over HTTP.
pub struct SponsorBlockCache {
    store: BucketStore,
    api: String,
}

impl SponsorBlockCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            store: BucketStore::new(Some(dir), DEFAULT_TTL),
            api: DEFAULT_API.to_string(),
        }
    }

    /// The raw bucket covering `video_id`, from the cache or the network.
    pub fn bucket(&self, video_id: &str) -> Option<String> {
        self.store.resolve(video_id, |prefix| self.fetch(prefix))
    }

    fn fetch(&self, prefix: &str) -> Result<String, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(HTTP_TIMEOUT)
            .timeout_read(HTTP_TIMEOUT)
            .user_agent(USER_AGENT)
            .build();
        match agent.get(&bucket_url(&self.api, prefix)).call() {
            Ok(response) => response.into_string().map_err(|e| e.to_string()),
            // Nothing submitted for any video in this bucket. An answer, not a
            // failure — and caching it stops every play of an unsubmitted video
            // from going back to the network.
            Err(ureq::Error::Status(404, _)) => Ok("[]".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// How far a reported duration must move before the plan is rebuilt. Well under
/// the tolerance the duration filter itself applies, so a jitter of a few
/// milliseconds does not churn, and far below a real correction.
const DURATION_EPSILON: f64 = 0.25;

/// Per-playback state: the lookup in flight, and what to skip once it lands.
///
/// The same three moments as the Linux watcher — an item starts, the bucket and
/// the duration arrive in either order, and then every frame feeds a position in
/// and may get a seek back.
pub struct SkipWatcher {
    cache: Arc<SponsorBlockCache>,
    categories: Vec<Category>,
    video_id: Option<String>,
    pending: Option<Receiver<Option<String>>>,
    /// The submissions, parsed once on arrival — the plan is rebuilt whenever
    /// the duration changes, and re-parsing 40 KB of JSON to do it on a frame
    /// loop would not be free.
    submissions: Option<Vec<RawSegment>>,
    skipper: Option<SegmentSkipper>,
    /// The duration the current plan was built against, so a later, different
    /// one rebuilds it. A plan latched to a wrong duration is a plan that skips
    /// nothing for the rest of the video.
    planned_for: Option<f64>,
}

impl SkipWatcher {
    /// A watcher over `categories`, or `None` when none are enabled.
    ///
    /// The `None` is the off switch, shaped as an absent object rather than an
    /// empty list so there is no code path that could contact the service by
    /// accident. A library with the setting off has no watcher at all.
    pub fn new(cache: Arc<SponsorBlockCache>, categories: Vec<Category>) -> Option<Self> {
        if categories.is_empty() {
            return None;
        }
        Some(Self {
            cache,
            categories,
            video_id: None,
            pending: None,
            submissions: None,
            skipper: None,
            planned_for: None,
        })
    }

    /// An item started. `video_id` is `None` for anything that is not a YouTube
    /// video, which is every other source: the database is YouTube-only.
    pub fn note_item_started(&mut self, video_id: Option<String>) {
        // Re-opening the same video reuses the bucket already in hand; only the
        // skipper's memory of what the viewer has seen is reset.
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
        match std::thread::Builder::new()
            .name("sponsorblock".into())
            .spawn(move || {
                // The receiver is gone whenever playback moved on first, which
                // is ordinary — the lookup simply lost the race.
                let _ = tx.send(cache.bucket(&video_id));
            }) {
            Ok(_) => self.pending = Some(rx),
            Err(e) => log::warn!("could not start the SponsorBlock lookup: {e}"),
        }
    }

    /// Playback stopped. Drops the plan but keeps the submissions, so coming
    /// back to the same video does not fetch again.
    pub fn note_stopped(&mut self) {
        self.skipper = None;
        self.planned_for = None;
    }

    /// Feed the current position; seek to whatever comes back.
    pub fn poll(&mut self, position: Option<f64>, duration: Option<f64>) -> Option<Skip> {
        self.collect_pending();

        let duration = duration?;
        // Rebuilt whenever the duration changes, not once: every submission is
        // judged against the duration of the file playing, and a player can
        // report a provisional one first. Building the plan from whatever
        // arrived first silently disabled skipping for the rest of the video.
        if self
            .planned_for
            .is_none_or(|had| (had - duration).abs() > DURATION_EPSILON)
        {
            self.build_plan(duration);
        }
        self.skipper.as_mut()?.on_position(position?)
    }

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
                    Err(e) => {
                        log::warn!("could not parse a SponsorBlock bucket for {video_id}: {e}")
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            // The worker died without sending: nothing to skip, and nothing
            // worth retrying inside one playback.
            Err(TryRecvError::Disconnected) => self.pending = None,
        }
    }

    /// Rebuilding forgets which spans the viewer has already been skipped past,
    /// which is correct: a duration that changed is a different timeline, and
    /// the spans it produces are not the ones that were consumed.
    fn build_plan(&mut self, duration: f64) {
        let (Some(raw), Some(video_id)) = (self.submissions.as_ref(), self.video_id.as_ref())
        else {
            return;
        };
        let segments = plan_skips(raw, duration, &self.categories);
        // At `info`, because logcat is the only window into a device and this
        // one line answers the question that matters when a video did not skip:
        // whether anything was planned, and against which duration. The app
        // filters to `info` (see `lib.rs`), so a `debug` here would be invisible
        // exactly when it is wanted.
        log::info!(
            "planned {} SponsorBlock skips in {video_id} from {} submissions at {duration}s",
            segments.len(),
            raw.len()
        );
        self.skipper = Some(SegmentSkipper::new(segments, duration));
        self.planned_for = Some(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn watcher() -> SkipWatcher {
        let dir = std::env::temp_dir().join("lunchbox-media-sponsorblock-tests");
        let cache = Arc::new(SponsorBlockCache::new(dir));
        SkipWatcher::new(cache, vec![Category::Sponsor]).expect("categories were given")
    }

    /// The off switch, and the thing that keeps a library nobody enabled this
    /// for from ever contacting the service.
    #[test]
    fn no_enabled_categories_means_no_watcher() {
        let cache = Arc::new(SponsorBlockCache::new(std::env::temp_dir()));
        assert!(SkipWatcher::new(cache, Vec::new()).is_none());
    }

    #[test]
    fn a_non_youtube_item_starts_no_lookup() {
        let mut w = watcher();
        w.note_item_started(None);
        assert!(w.pending.is_none());
        assert!(w.poll(Some(1.0), Some(600.0)).is_none());
    }

    #[test]
    fn a_planned_segment_is_skipped_once_the_duration_is_known() {
        let mut w = watcher();
        w.video_id = Some("v".into());
        w.submissions = Some(sponsor_submission());

        // No duration yet: nothing to judge the submission against.
        assert!(w.poll(Some(31.0), None).is_none());

        let skip = w.poll(Some(31.0), Some(600.0)).expect("inside a span");
        assert_eq!(skip.target, 60.0);
        assert_eq!(skip.category, Category::Sponsor);
        assert!(w.poll(Some(31.0), Some(600.0)).is_none(), "skipped once");
    }

    /// The same rebuild the Linux watcher does, for the same reason: a
    /// provisional duration must not disable skipping for the whole video.
    #[test]
    fn a_corrected_duration_rebuilds_the_plan() {
        let mut w = watcher();
        w.video_id = Some("v".into());
        w.submissions = Some(sponsor_submission());

        assert!(w.poll(Some(31.0), Some(120.0)).is_none());
        assert!(w.skipper.as_ref().unwrap().is_empty());

        assert_eq!(w.poll(Some(31.0), Some(600.0)).unwrap().target, 60.0);
    }
}
