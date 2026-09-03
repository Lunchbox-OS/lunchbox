//! Fetch SponsorBlock segments over HTTP, with an on-disk cache (issue #159).
//!
//! Lives beside the video cache for the same reason `playlist.rs` does: two
//! processes need it. The player asks for a video's segments when it starts
//! playing one, and shepherdd's prefetcher warms them alongside the video it is
//! downloading — without that, a device that filled its cache while online skips
//! nothing when it plays that cache offline, which is the exact case the
//! prefetcher exists for.
//!
//! ## Buckets, not videos
//!
//! The request is for a *bucket*: the first four hex characters of the SHA-256
//! of the video id ([`shepherd_media_app::sponsorblock_prefix`]). The service
//! answers with every video whose id hashes into that bucket — around a hundred
//! of them, ~40 KB — and the device picks out the one it wanted. That is what
//! keeps the service from learning what a child is watching, and it is the only
//! endpoint this crate ever calls; the exact-video endpoint is a byte-for-byte
//! description of somebody's viewing.
//!
//! Caching the whole bucket turns that privacy cost into a saving. Prefetching a
//! playlist tends to hit far fewer buckets than it has videos, and a later
//! lookup that lands in a bucket already on disk costs nothing at all.
//!
//! ## What is stored
//!
//! Exactly what the server sent, under `<prefix>.json`, with the file's mtime as
//! the fetch time. Nothing is re-encoded: `shepherd-media-core` owns the wire
//! format and is the only thing that parses it, so a schema this crate invented
//! would be a second format to keep in step for no gain.
//!
//! The request always asks for every skippable category, whatever the config
//! enables. A cached bucket is therefore independent of configuration — changing
//! the enabled categories re-reads the same file rather than invalidating it —
//! and the shape of the request says nothing about the household's settings.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use shepherd_media_app::cache::{self, Freshness};
use shepherd_media_app::sponsorblock_prefix;
use shepherd_media_core::sponsorblock::{Category, Segment, parse_bucket, plan_skips};
use tracing::{debug, warn};

/// The public SponsorBlock instance. Overridable in config so a household can
/// point at its own mirror.
pub const DEFAULT_API: &str = "https://sponsor.ajay.app";

/// A cached bucket is trusted for this long. Submissions churn most in the days
/// after an upload, and a day-old bucket that skips one segment fewer is a much
/// smaller cost than a network round-trip in front of every video.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

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
    /// `None` when no cache home could be determined, in which case every
    /// lookup is a live fetch and nothing is written down.
    dir: Option<PathBuf>,
    api: String,
    agent: ureq::Agent,
}

impl SponsorBlockCache {
    /// A cache under `$XDG_CACHE_HOME/shepherd/media/sponsorblock/`, talking to
    /// `api`.
    pub fn new(api: impl Into<String>) -> Self {
        Self::with_dir(crate::media_cache_dir("sponsorblock"), api)
    }

    /// A cache rooted at a given directory. For tests and for a caller that
    /// keeps its media state somewhere else.
    pub fn with_dir(dir: Option<PathBuf>, api: impl Into<String>) -> Self {
        Self {
            dir,
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
    /// cached. A video simply plays through.
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
    /// Returns whether a bucket is now cached.
    pub fn warm(&self, video_id: &str) -> bool {
        self.bucket(video_id).is_some()
    }

    /// The raw bucket JSON covering `video_id`, from cache or the network.
    pub fn bucket(&self, video_id: &str) -> Option<String> {
        self.resolve(video_id, |prefix| self.fetch(prefix))
    }

    /// The fresh/fetch/stale-fallback policy, with the fetch injected so the
    /// caching behaviour is testable without a network.
    fn resolve(
        &self,
        video_id: &str,
        fetch: impl FnOnce(&str) -> Result<String, String>,
    ) -> Option<String> {
        let prefix = sponsorblock_prefix(video_id);
        let path = self.dir.as_ref().map(|d| d.join(format!("{prefix}.json")));
        let cached = path.as_deref().and_then(load_bucket);

        match cache::resolve(cached, || fetch(&prefix)) {
            cache::Resolution::Fresh(json) => {
                debug!(prefix, "SponsorBlock bucket cache hit");
                Some(json)
            }
            cache::Resolution::Fetched(json) => {
                if let Some(path) = path.as_deref() {
                    save_bucket(path, &json);
                }
                Some(json)
            }
            cache::Resolution::Stale(json, e) => {
                debug!(
                    prefix,
                    "SponsorBlock refresh failed: {e}; using stale bucket"
                );
                Some(json)
            }
            cache::Resolution::Miss(e) => {
                debug!(prefix, "no SponsorBlock segments available: {e}");
                None
            }
        }
    }

    /// Fetch one bucket.
    ///
    /// Every skippable category is requested regardless of what is enabled, so
    /// the cached bucket outlives a config change — and so the request reveals
    /// nothing about the configuration. `actionTypes` is narrowed to `skip`
    /// because that is all this player acts on; mutes and markers would be
    /// bytes nobody reads.
    fn fetch(&self, prefix: &str) -> Result<String, String> {
        let categories: Vec<&str> = Category::all()
            .iter()
            .filter(|c| c.is_skippable())
            .map(|c| c.as_str())
            .collect();
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair(
                "categories",
                &serde_json::to_string(&categories).map_err(|e| e.to_string())?,
            )
            .append_pair("actionTypes", r#"["skip"]"#)
            .finish();
        let url = format!("{}/api/skipSegments/{prefix}?{query}", self.api);

        match self.agent.get(&url).call() {
            Ok(response) => response.into_string().map_err(|e| e.to_string()),
            // No video in this bucket has a submission. That is an answer, not a
            // failure, and caching it is what stops every play of an unsubmitted
            // video from going back to the network.
            Err(ureq::Error::Status(404, _)) => Ok("[]".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Read a cached bucket and judge its age by the file's mtime.
///
/// A file that cannot be read is a miss; the caller re-fetches, which is always
/// safe. There is deliberately no parse here — the bytes are the server's, and
/// the core is the only thing that reads them.
fn load_bucket(path: &Path) -> Option<(String, Freshness)> {
    let json = std::fs::read_to_string(path).ok()?;
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or(Duration::ZERO);

    let freshness = if age >= CACHE_TTL {
        Freshness::Stale
    } else {
        Freshness::Fresh
    };
    Some((json, freshness))
}

/// Write a bucket, atomically.
///
/// Through a temporary file and a rename because shepherdd's prefetcher and the
/// running player share this directory: a reader must never see half a response,
/// and the alternative — a torn file that fails to parse — would present as "no
/// segments" rather than as an error anybody could see.
///
/// Failures are logged and swallowed. A cache that could not be written costs a
/// fetch next time and nothing else.
fn save_bucket(path: &Path, json: &str) {
    let Some(parent) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(parent) {
        warn!(
            "could not create the SponsorBlock cache dir {}: {e}",
            parent.display()
        );
        return;
    }

    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json) {
        warn!("could not write {}: {e}", tmp.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        warn!("could not commit {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    debug!(
        bytes = json.len(),
        "cached a SponsorBlock bucket at {}",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    const VIDEO: &str = "dQw4w9WgXcQ";
    const PREFIX: &str = "5f6b";

    fn bucket_json(video_id: &str, start: f64, end: f64) -> String {
        format!(
            r#"[{{"videoID":"{video_id}","segments":[{{"category":"sponsor","actionType":"skip",
               "segment":[{start},{end}],"UUID":"u","videoDuration":600.0,"locked":0,"votes":5,
               "description":""}}]}}]"#
        )
    }

    fn cache_in(dir: &Path) -> SponsorBlockCache {
        SponsorBlockCache::with_dir(Some(dir.to_path_buf()), DEFAULT_API)
    }

    /// Backdate a cached bucket so it reads as stale.
    fn age(path: &Path, seconds: u64) {
        let when = SystemTime::now() - Duration::from_secs(seconds);
        let ft = filetime::FileTime::from_unix_time(
            when.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
            0,
        );
        filetime::set_file_mtime(path, ft).unwrap();
    }

    #[test]
    fn a_fetched_bucket_is_written_under_its_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let json = cache
            .resolve(VIDEO, |prefix| {
                assert_eq!(prefix, PREFIX);
                Ok(bucket_json(VIDEO, 30.0, 60.0))
            })
            .unwrap();
        assert!(json.contains(VIDEO));

        let path = dir.path().join(format!("{PREFIX}.json"));
        assert!(path.exists(), "the response should be cached verbatim");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), json);
        assert!(
            std::fs::read_dir(dir.path()).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "the temporary file should not survive the rename"
        );
    }

    #[test]
    fn a_fresh_bucket_is_served_without_a_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache
            .resolve(VIDEO, |_| Ok(bucket_json(VIDEO, 30.0, 60.0)))
            .unwrap();
        let json = cache
            .resolve(VIDEO, |_| panic!("a fresh bucket must not be refetched"))
            .unwrap();
        assert!(json.contains("30"));
    }

    #[test]
    fn a_stale_bucket_is_refreshed_when_the_fetch_works() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache
            .resolve(VIDEO, |_| Ok(bucket_json(VIDEO, 30.0, 60.0)))
            .unwrap();
        age(&dir.path().join(format!("{PREFIX}.json")), 48 * 3600);

        let json = cache
            .resolve(VIDEO, |_| Ok(bucket_json(VIDEO, 45.0, 75.0)))
            .unwrap();
        assert!(json.contains("45"), "the refreshed bucket should win");
        assert!(
            std::fs::read_to_string(dir.path().join(format!("{PREFIX}.json")))
                .unwrap()
                .contains("45"),
            "and be written back"
        );
    }

    /// The offline case the whole cache exists for: a device past the TTL with
    /// no network still skips what it knew about yesterday.
    #[test]
    fn a_stale_bucket_is_served_when_the_fetch_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache
            .resolve(VIDEO, |_| Ok(bucket_json(VIDEO, 30.0, 60.0)))
            .unwrap();
        age(&dir.path().join(format!("{PREFIX}.json")), 48 * 3600);

        let json = cache
            .resolve(VIDEO, |_| Err("offline".into()))
            .expect("stale is better than nothing here");
        assert!(json.contains("30"));
    }

    #[test]
    fn a_miss_with_no_network_is_no_segments_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        assert!(cache.resolve(VIDEO, |_| Err("offline".into())).is_none());
    }

    #[test]
    fn segments_are_planned_from_the_cached_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache
            .resolve(VIDEO, |_| Ok(bucket_json(VIDEO, 30.0, 60.0)))
            .unwrap();

        let segments = cache.segments(VIDEO, 600.0, &[Category::Sponsor]);
        assert_eq!(segments.len(), 1);
        assert_eq!((segments[0].start, segments[0].end), (30.0, 60.0));

        assert!(
            cache.segments(VIDEO, 600.0, &[Category::Intro]).is_empty(),
            "a category nobody enabled is not skipped"
        );
    }

    /// A bucket that covers other videos but not this one is a perfectly normal
    /// answer, and must not read as an error or a miss.
    #[test]
    fn a_video_with_no_submissions_yields_no_segments() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache
            .resolve(VIDEO, |_| Ok(bucket_json("someone-else", 30.0, 60.0)))
            .unwrap();
        assert!(
            cache
                .segments(VIDEO, 600.0, &[Category::Sponsor])
                .is_empty()
        );
    }

    #[test]
    fn a_corrupt_bucket_is_treated_as_no_segments() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        std::fs::write(dir.path().join(format!("{PREFIX}.json")), "{ truncated").unwrap();
        assert!(
            cache
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

    #[test]
    fn with_no_cache_home_every_lookup_is_a_live_fetch() {
        let cache = SponsorBlockCache::with_dir(None, DEFAULT_API);
        let json = cache
            .resolve(VIDEO, |_| Ok(bucket_json(VIDEO, 30.0, 60.0)))
            .unwrap();
        assert!(json.contains(VIDEO));
        // And again — nothing was written down, so it is fetched afresh.
        assert!(cache.resolve(VIDEO, |_| Err("offline".into())).is_none());
    }
}
