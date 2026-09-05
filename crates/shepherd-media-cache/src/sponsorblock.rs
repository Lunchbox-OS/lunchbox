//! Fetch SponsorBlock segments over HTTP for the Linux side (issue #159).
//!
//! Lives beside the video cache for the same reason `playlist.rs` does: two
//! processes need it. The player asks for a video's segments when it starts
//! playing one, and shepherdd's prefetcher warms them alongside the video it is
//! downloading — without that, a device that filled its cache while online skips
//! nothing when it plays that cache offline, which is the case the prefetcher
//! exists for.
//!
//! Only the HTTP fetch and the XDG directory are here, mirroring how the poster
//! cache is split. The disk policy is `shepherd_media_app::BucketStore` and the
//! request is [`shepherd_media_core::sponsorblock::bucket_url`], both shared
//! with the Android app so the two front-ends ask for the same thing and keep
//! it for the same length of time.
//!
//! ## Buckets, not videos
//!
//! The request names a *bucket*: the first four hex characters of the SHA-256 of
//! the video id. The service answers with every video that hashes into it —
//! around a hundred, ~40 KB — and the device picks out the one it wanted. That
//! local filter is what keeps the service from learning what a child is
//! watching, and this is the only endpoint used; the exact-video endpoint is a
//! byte-for-byte description of somebody's viewing.

use std::path::PathBuf;
use std::time::Duration;

use shepherd_media_app::BucketStore;
use shepherd_media_app::sponsorblock::DEFAULT_TTL;
use shepherd_media_core::sponsorblock::{Category, Segment, bucket_url, parse_bucket, plan_skips};
use tracing::{debug, warn};

/// The public SponsorBlock instance. Overridable in config so a household can
/// point at its own mirror.
pub const DEFAULT_API: &str = "https://sponsor.ajay.app";

/// Connect and read timeout. A player is waiting on this with a video already
/// on screen, so it fails fast and skips nothing rather than stalling.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Identifies the client to the service, as its API asks. Carries no user,
/// device or library identity — the version is here so an operator reading their
/// own mirror's logs can tell which build is asking.
const USER_AGENT: &str = concat!("shepherd-media/", env!("CARGO_PKG_VERSION"));

/// Segment lookups against a local bucket cache, refreshed over HTTP.
///
/// Cheap to construct and safe to keep around; holds no connection.
pub struct SponsorBlockCache {
    store: BucketStore,
    api: String,
    agent: ureq::Agent,
}

impl SponsorBlockCache {
    /// A cache under `$XDG_CACHE_HOME/shepherd/media/sponsorblock/`, talking to
    /// `api`.
    pub fn new(api: impl Into<String>) -> Self {
        Self::with_dir(crate::media_cache_dir("sponsorblock"), api)
    }

    /// A cache rooted at a given directory. For tests, and for a caller that
    /// keeps its media state somewhere else.
    pub fn with_dir(dir: Option<PathBuf>, api: impl Into<String>) -> Self {
        Self {
            store: BucketStore::new(dir, DEFAULT_TTL),
            api: api.into(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(HTTP_TIMEOUT)
                .timeout_read(HTTP_TIMEOUT)
                .user_agent(USER_AGENT)
                .build(),
        }
    }

    /// The spans to skip in `video_id`, for a file of `duration` seconds and the
    /// categories a parent enabled.
    ///
    /// Never fails: no segments is the ordinary answer for most videos, and it
    /// is also the answer when the service is unreachable and nothing was
    /// cached. The video simply plays through.
    pub fn segments(&self, video_id: &str, duration: f64, enabled: &[Category]) -> Vec<Segment> {
        let Some(json) = self.bucket(video_id) else {
            return Vec::new();
        };
        match parse_bucket(&json, video_id) {
            Ok(raw) => {
                let plan = plan_skips(&raw, duration, enabled);
                debug!(
                    video_id,
                    submissions = raw.len(),
                    planned = plan.len(),
                    "resolved SponsorBlock segments"
                );
                plan
            }
            Err(e) => {
                warn!(video_id, "could not parse a SponsorBlock bucket: {e}");
                Vec::new()
            }
        }
    }

    /// Make sure this video's bucket is on disk, fetching it if it is missing or
    /// stale. For the prefetcher, which wants the data present before the device
    /// goes offline and does not care what is in it.
    ///
    /// Returns whether a bucket is now available.
    pub fn warm(&self, video_id: &str) -> bool {
        self.bucket(video_id).is_some()
    }

    /// Re-fetch this video's bucket whatever its age, for an
    /// administrator-triggered refresh (issue #165).
    ///
    /// `Err` means the segments on disk are still yesterday's, which is what
    /// the caller raises a diagnostic about. A bucket that could not be
    /// replaced is kept, so a failed refresh never costs the device skips it
    /// already had — see [`BucketStore::refresh`].
    pub fn refresh(&self, video_id: &str) -> Result<(), String> {
        self.store.refresh(video_id, |prefix| self.fetch(prefix))
    }

    /// The hash prefix `video_id` falls in. A refresh sweeping a library uses
    /// it to ask for each bucket once rather than once per video: a bucket
    /// covers around a hundred videos, and unlike [`Self::warm`] a refresh
    /// cannot lean on the cache write to make the second ask free.
    pub fn prefix_for(&self, video_id: &str) -> String {
        self.store.prefix_for(video_id)
    }

    /// The raw bucket JSON covering `video_id`, from cache or the network.
    pub fn bucket(&self, video_id: &str) -> Option<String> {
        self.store.resolve(video_id, |prefix| self.fetch(prefix))
    }

    /// Fetch one bucket.
    fn fetch(&self, prefix: &str) -> Result<String, String> {
        match self.agent.get(&bucket_url(&self.api, prefix)).call() {
            Ok(response) => response.into_string().map_err(|e| e.to_string()),
            // No video in this bucket has a submission. That is an answer, not a
            // failure, and caching it is what stops every play of an unsubmitted
            // video from going back to the network.
            Err(ureq::Error::Status(404, _)) => Ok("[]".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const VIDEO: &str = "dQw4w9WgXcQ";
    const PREFIX: &str = "5f6b";

    fn cache_in(dir: &Path) -> SponsorBlockCache {
        SponsorBlockCache::with_dir(Some(dir.to_path_buf()), DEFAULT_API)
    }

    fn bucket_json(video_id: &str, start: f64, end: f64) -> String {
        format!(
            r#"[{{"videoID":"{video_id}","segments":[{{"category":"sponsor","actionType":"skip",
               "segment":[{start},{end}],"UUID":"u","videoDuration":600.0,"locked":0,"votes":5,
               "description":""}}]}}]"#
        )
    }

    /// The whole chain from a cached file to a plan, which is what the player
    /// actually calls.
    #[test]
    fn segments_are_planned_from_the_cached_bucket() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(format!("{PREFIX}.json")),
            bucket_json(VIDEO, 30.0, 60.0),
        )
        .unwrap();
        let cache = cache_in(dir.path());

        let segments = cache.segments(VIDEO, 600.0, &[Category::Sponsor]);
        assert_eq!(segments.len(), 1);
        assert_eq!((segments[0].start, segments[0].end), (30.0, 60.0));

        assert!(
            cache.segments(VIDEO, 600.0, &[Category::Intro]).is_empty(),
            "a category nobody enabled is not skipped"
        );
    }

    /// A bucket that covers other videos but not this one is an ordinary
    /// answer, not an error and not a miss.
    #[test]
    fn a_video_with_no_submissions_yields_no_segments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(format!("{PREFIX}.json")),
            bucket_json("someone-else", 30.0, 60.0),
        )
        .unwrap();
        assert!(
            cache_in(dir.path())
                .segments(VIDEO, 600.0, &[Category::Sponsor])
                .is_empty()
        );
    }

    #[test]
    fn a_corrupt_bucket_is_treated_as_no_segments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{PREFIX}.json")), "{ truncated").unwrap();
        assert!(
            cache_in(dir.path())
                .segments(VIDEO, 600.0, &[Category::Sponsor])
                .is_empty()
        );
    }

    /// Talks to the real service, so it is not part of the suite — run it by
    /// hand (`cargo test -p shepherd-media-cache -- --ignored live_fetch`) when
    /// touching the request, which is the one thing the tests above cannot
    /// cover. It asserts the shape of the answer, not its content: submissions
    /// come and go.
    #[test]
    #[ignore = "requires network access to sponsor.ajay.app"]
    fn live_fetch_returns_a_parseable_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let json = cache.bucket(VIDEO).expect("the service should answer");
        assert!(json.starts_with('['), "a bucket is a JSON array");
        parse_bucket(&json, VIDEO).expect("the bucket should parse");
        assert!(dir.path().join(format!("{PREFIX}.json")).exists());
    }
}
