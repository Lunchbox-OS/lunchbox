//! SponsorBlock segments: the wire format, what to do with it, and when to skip
//! (issue #159).
//!
//! Everything here is pure. Fetching the data is a platform concern — the Linux
//! side caches buckets in `lunchbox-media-cache`, the Android side fetches its
//! own — but *deciding* is not, and the two front-ends have to make the same
//! decision or a video skips differently depending on which one is playing it.
//! So the wire parsing, the filtering, and the skip state machine all live in
//! the core and are exercised by the same tests.
//!
//! ## The three stages
//!
//! 1. [`parse_bucket`] decodes a hash-prefix response and picks out one video.
//!    The response covers every video sharing the first four hex characters of
//!    `sha256(videoID)` — that is what makes the endpoint private, and it is why
//!    the caller caches by bucket rather than by video.
//! 2. [`plan_skips`] turns those raw submissions into the spans this player will
//!    actually jump over, given the video's real duration and the categories a
//!    parent enabled.
//! 3. [`SegmentSkipper`] watches the playback position and says when to seek.
//!
//! The split exists because the three stages know different things at different
//! times: the bucket is cacheable and category-agnostic (so changing the
//! configured categories must not invalidate a cache), the plan needs the
//! duration mpv reports for the file actually loaded, and only the skipper knows
//! where the viewer is or what they have already been shown.

use serde::Deserialize;
use thiserror::Error;
use tracing::debug;

/// Categories enabled when the feature is switched on and the config names none
/// (issue #159).
///
/// These five are the spans that are reliably *not* the video. The three that
/// are missing — `Preview`, `Filler`, `MusicOfftopic` — are judgement calls
/// whose submissions can cut real content, so they are opt-in: a recap is part
/// of the episode for a viewer who missed last week, and "filler tangent" is
/// one contributor's opinion about what a video is for.
pub const DEFAULT_CATEGORIES: &[Category] = &[
    Category::Sponsor,
    Category::SelfPromo,
    Category::Interaction,
    Category::Intro,
    Category::Outro,
];

/// A segment shorter than this is not worth a seek: the jump is more disruptive
/// than the content it removes, and it risks landing back inside the span it
/// just left.
const MIN_SKIP_SECONDS: f64 = 1.0;

/// Never seek closer than this to the end of the file. A skip that lands exactly
/// on the duration is a race with the backend's own end-of-file handling; a
/// quarter-second short of it ends the video the ordinary way.
const EOF_MARGIN: f64 = 0.25;

/// A drop in position larger than this is a seek (or a restart), not playback.
/// Playback only ever moves the position forwards, but the value is polled from
/// a UI frame loop, so small backwards jitter is normal and must not be read as
/// the viewer going back.
const BACKWARD_SEEK_TOLERANCE: f64 = 1.0;

/// A SponsorBlock category.
///
/// The list is open — the service adds categories — so an unrecognised one on
/// the wire is dropped rather than being an error. Two of these are marked
/// unskippable upstream and are represented only so that parsing does not lose
/// them silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Sponsor,
    SelfPromo,
    Interaction,
    Intro,
    Outro,
    Preview,
    Filler,
    MusicOfftopic,
    Hook,
    /// A point of interest, not a span. Never skipped.
    PoiHighlight,
    /// A chapter marker. Never skipped.
    Chapter,
}

impl Category {
    /// The wire and config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Sponsor => "sponsor",
            Category::SelfPromo => "selfpromo",
            Category::Interaction => "interaction",
            Category::Intro => "intro",
            Category::Outro => "outro",
            Category::Preview => "preview",
            Category::Filler => "filler",
            Category::MusicOfftopic => "music_offtopic",
            Category::Hook => "hook",
            Category::PoiHighlight => "poi_highlight",
            Category::Chapter => "chapter",
        }
    }

    /// Parse the wire and config spelling. `None` for anything unrecognised.
    pub fn parse(s: &str) -> Option<Category> {
        Some(match s {
            "sponsor" => Category::Sponsor,
            "selfpromo" => Category::SelfPromo,
            "interaction" => Category::Interaction,
            "intro" => Category::Intro,
            "outro" => Category::Outro,
            "preview" => Category::Preview,
            "filler" => Category::Filler,
            "music_offtopic" => Category::MusicOfftopic,
            "hook" => Category::Hook,
            "poi_highlight" => Category::PoiHighlight,
            "chapter" => Category::Chapter,
            _ => return None,
        })
    }

    /// Whether a span of this category may be jumped over at all.
    ///
    /// A highlight is a single point and a chapter is a label; neither describes
    /// content anybody wants removed.
    pub fn is_skippable(self) -> bool {
        !matches!(self, Category::PoiHighlight | Category::Chapter)
    }

    /// Short human name, for the "skipped" notice the player shows.
    pub fn label(self) -> &'static str {
        match self {
            Category::Sponsor => "sponsor",
            Category::SelfPromo => "promotion",
            Category::Interaction => "reminder",
            Category::Intro => "intro",
            Category::Outro => "end cards",
            Category::Preview => "recap",
            Category::Filler => "filler",
            Category::MusicOfftopic => "non-music section",
            Category::Hook => "hook",
            Category::PoiHighlight => "highlight",
            Category::Chapter => "chapter",
        }
    }

    /// Every category, in the order they are documented.
    pub fn all() -> &'static [Category] {
        &[
            Category::Sponsor,
            Category::SelfPromo,
            Category::Interaction,
            Category::Intro,
            Category::Outro,
            Category::Preview,
            Category::Filler,
            Category::MusicOfftopic,
            Category::Hook,
            Category::PoiHighlight,
            Category::Chapter,
        ]
    }
}

/// What the submitter says a player should *do* with a span.
///
/// Only [`ActionType::Skip`] is acted on. `Mute` is a volume change rather than
/// a seek and nobody has asked for it; `Poi` and `Chapter` are markers; `Full`
/// says the entire video is the category, which is a labelling decision, not a
/// playback one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionType {
    Skip,
    Mute,
    Poi,
    Chapter,
    Full,
}

impl ActionType {
    fn parse(s: &str) -> Option<ActionType> {
        Some(match s {
            "skip" => ActionType::Skip,
            "mute" => ActionType::Mute,
            "poi" => ActionType::Poi,
            "chapter" => ActionType::Chapter,
            "full" => ActionType::Full,
            _ => return None,
        })
    }
}

/// One submission, as it came off the wire, with the fields needed to judge it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub category: Category,
    pub action: ActionType,
    pub start: f64,
    pub end: f64,
    /// The duration of the video the submitter was watching. `0.0` means the
    /// service does not know it; anything else is checked against the file this
    /// player actually loaded.
    pub video_duration: f64,
    pub votes: i64,
    /// Confirmed by a moderator. A locked submission overrules unlocked ones
    /// covering the same span.
    pub locked: bool,
}

/// A span this player will jump over, resolved against a real duration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub category: Category,
    pub start: f64,
    pub end: f64,
}

impl Segment {
    fn contains(&self, position: f64) -> bool {
        position >= self.start && position < self.end
    }
}

/// A decision to seek, and why — the category is what the player's notice names.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Skip {
    pub target: f64,
    pub category: Category,
}

#[derive(Debug, Error)]
pub enum SponsorBlockError {
    #[error("could not parse SponsorBlock response: {0}")]
    Json(#[from] serde_json::Error),
}

// --- Wire format ---

#[derive(Deserialize)]
struct WireBucketEntry {
    #[serde(rename = "videoID")]
    video_id: String,
    #[serde(default)]
    segments: Vec<WireSegment>,
}

#[derive(Deserialize)]
struct WireSegment {
    category: String,
    #[serde(rename = "actionType")]
    action_type: String,
    /// `[start, end]` in seconds.
    segment: [f64; 2],
    #[serde(rename = "videoDuration", default)]
    video_duration: f64,
    #[serde(default)]
    votes: i64,
    /// 0 or 1 on the wire.
    #[serde(default)]
    locked: i64,
}

/// The URL that fetches one bucket.
///
/// Built here, in the core, because both front-ends fetch and neither should be
/// deciding what to ask for: the query is *every* skippable category and the
/// `skip` action, whatever a household has enabled. That keeps a cached bucket
/// independent of configuration — changing the enabled categories re-reads the
/// same file rather than invalidating it — and it keeps the shape of the
/// request from describing anybody's settings.
///
/// `api` is the instance's base URL, with or without a trailing slash.
pub fn bucket_url(api: &str, prefix: &str) -> String {
    let categories: Vec<&str> = Category::all()
        .iter()
        .filter(|c| c.is_skippable())
        .map(|c| c.as_str())
        .collect();
    // `serde_json` cannot fail on a list of string literals.
    let categories = serde_json::to_string(&categories).unwrap_or_else(|_| "[]".to_string());
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("categories", &categories)
        .append_pair("actionTypes", r#"["skip"]"#)
        .finish();
    format!(
        "{}/api/skipSegments/{prefix}?{query}",
        api.trim_end_matches('/')
    )
}

/// Pull one video's submissions out of a hash-prefix response.
///
/// `json` is the whole bucket — every video whose id hashes into the same
/// four-character prefix — and the filtering by `video_id` happens here, on the
/// device. That local filter is the entire privacy mechanism of the endpoint, so
/// it is not optional and it is not the caller's job to remember.
///
/// A video absent from the bucket yields an empty vector, which is the ordinary
/// "nothing submitted for this one" answer rather than an error. Submissions
/// naming a category or action this build does not recognise are dropped: the
/// service adds both over time, and an unknown span is one this player has no
/// opinion about.
pub fn parse_bucket(json: &str, video_id: &str) -> Result<Vec<RawSegment>, SponsorBlockError> {
    let bucket: Vec<WireBucketEntry> = serde_json::from_str(json)?;
    let Some(entry) = bucket.into_iter().find(|e| e.video_id == video_id) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(entry.segments.len());
    for w in entry.segments {
        let (Some(category), Some(action)) = (
            Category::parse(&w.category),
            ActionType::parse(&w.action_type),
        ) else {
            debug!(
                category = %w.category,
                action = %w.action_type,
                "ignoring a SponsorBlock segment this build does not understand"
            );
            continue;
        };
        out.push(RawSegment {
            category,
            action,
            start: w.segment[0],
            end: w.segment[1],
            video_duration: w.video_duration,
            votes: w.votes,
            locked: w.locked != 0,
        });
    }
    Ok(out)
}

/// Resolve raw submissions into the spans to skip in *this* file.
///
/// `duration` is the duration the player reports for the file it loaded, which
/// is the thing every judgement here hangs off. Callers without one — a live
/// stream, or a frame before the backend knows — must not guess: build the plan
/// once the duration arrives, or not at all.
///
/// Four things happen, in order:
///
/// **Only actionable submissions survive.** `skip` actions, skippable
/// categories, categories the config enabled, and a non-negative vote count. A
/// downvoted submission is one the community has already judged.
///
/// **Timestamps are checked against the duration.** A submission is made against
/// a particular cut of a video; if the upload was later replaced, its timestamps
/// point at the wrong content, and skipping on them removes something real. The
/// tolerance is yt-dlp's, deliberately: it is the reference implementation for
/// this API, its rule has been beaten on by a large number of videos, and two
/// tools disagreeing about whether a segment is stale would be worse than either
/// rule alone. A missing `videoDuration` (`0.0`) means the service does not know
/// and is accepted.
///
/// **Overlapping claims are resolved.** Two people submitting the same ad rarely
/// agree to the frame. Where submissions overlap, a moderator-locked one wins
/// outright and unlocked ones covering the same ground are dropped; what remains
/// is merged into its union, which is the span that removes the whole ad. The
/// union can over-reach by a second when a submission is sloppy — that is the
/// deliberate trade, bounded by the vote and lock filters above it.
///
/// **Everything is clamped to the file** and anything left shorter than a second
/// is dropped as not worth a seek.
pub fn plan_skips(raw: &[RawSegment], duration: f64, enabled: &[Category]) -> Vec<Segment> {
    if !(duration.is_finite() && duration > 0.0) {
        return Vec::new();
    }

    let mut candidates: Vec<RawSegment> = raw
        .iter()
        .filter(|s| s.action == ActionType::Skip)
        .filter(|s| s.category.is_skippable() && enabled.contains(&s.category))
        .filter(|s| s.votes >= 0)
        .filter_map(|s| snap_to_duration(s, duration))
        .collect();

    candidates.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out: Vec<Segment> = Vec::new();
    for group in overlapping_groups(&candidates) {
        // A moderator-confirmed span overrules the guesses around it.
        let authoritative: Vec<&RawSegment> = if group.iter().any(|s| s.locked) {
            group.iter().filter(|s| s.locked).copied().collect()
        } else {
            group
        };

        let start = authoritative
            .iter()
            .fold(f64::MAX, |acc, s| acc.min(s.start));
        let end = authoritative.iter().fold(f64::MIN, |acc, s| acc.max(s.end));
        // The notice names the best-supported claim in the group.
        let category = authoritative
            .iter()
            .max_by_key(|s| (s.locked, s.votes))
            .map(|s| s.category)
            .expect("a group is never empty");

        let end = end.min(duration);
        if end - start < MIN_SKIP_SECONDS {
            continue;
        }
        out.push(Segment {
            category,
            start,
            end,
        });
    }
    out
}

/// Apply the duration rules to one submission, or reject it.
///
/// Ported from yt-dlp's `SponsorBlockPP` (`postprocessor/sponsorblock.py`); see
/// [`plan_skips`] for why the tolerance is theirs rather than ours.
fn snap_to_duration(s: &RawSegment, duration: f64) -> Option<RawSegment> {
    let mut start = s.start;
    let mut end = s.end;

    // `[0, 0]` is the "the whole video is this category" marker, not a span.
    if start == 0.0 && end == 0.0 {
        return None;
    }
    if !(start.is_finite() && end.is_finite()) || end <= start {
        return None;
    }
    // Milliseconds of slop at either end are an artefact of how a submission was
    // made, not a claim about content.
    if start <= 1.0 {
        start = 0.0;
    }
    if duration - end <= 1.0 {
        end = duration;
    }
    if start >= duration {
        return None;
    }

    let diff = if s.video_duration > 0.0 {
        (duration - s.video_duration).abs()
    } else {
        0.0
    };
    let matches_this_cut = diff < 1.0 || (diff < 5.0 && diff / (end - start) < 0.05);
    if !matches_this_cut {
        debug!(
            category = s.category.as_str(),
            submitted_for = s.video_duration,
            playing = duration,
            "ignoring a SponsorBlock segment submitted against a different cut of this video"
        );
        return None;
    }

    Some(RawSegment {
        start,
        end,
        ..s.clone()
    })
}

/// Split start-sorted segments into runs of transitively overlapping ones.
fn overlapping_groups(sorted: &[RawSegment]) -> Vec<Vec<&RawSegment>> {
    let mut groups: Vec<Vec<&RawSegment>> = Vec::new();
    let mut reach = f64::MIN;
    for s in sorted {
        match groups.last_mut() {
            Some(group) if s.start < reach => {
                group.push(s);
                reach = reach.max(s.end);
            }
            _ => {
                groups.push(vec![s]);
                reach = s.end;
            }
        }
    }
    groups
}

/// Decides when playback should jump, given where it is.
///
/// Built from a plan and then fed the position each frame. It holds the two
/// pieces of state a plan cannot: which spans the viewer has already been
/// skipped past, and where they were a moment ago.
///
/// The rules it enforces are all about not fighting the viewer:
///
/// - **A span is skipped once.** Otherwise a viewer who rewinds ten seconds into
///   a sponsor is thrown forwards again the instant they get there, with no way
///   to watch what they went back for.
/// - **Seeking backwards past a span restores it.** Landing *before* a span is a
///   decision to approach it again — most often a restart, or a rewind past the
///   start of the video — and it should behave as it did the first time.
/// - **Never seek backwards, and never for less than a second.** Both are worse
///   than the content they would remove.
#[derive(Debug, Clone)]
pub struct SegmentSkipper {
    /// Start-sorted and disjoint, as [`plan_skips`] returns them.
    segments: Vec<Segment>,
    consumed: Vec<bool>,
    duration: f64,
    last_position: Option<f64>,
}

impl SegmentSkipper {
    pub fn new(segments: Vec<Segment>, duration: f64) -> Self {
        let consumed = vec![false; segments.len()];
        Self {
            segments,
            consumed,
            duration,
            last_position: None,
        }
    }

    /// Build straight from a cached bucket. Convenience for the front-ends,
    /// which all do exactly this.
    pub fn from_bucket(
        json: &str,
        video_id: &str,
        duration: f64,
        enabled: &[Category],
    ) -> Result<Self, SponsorBlockError> {
        let raw = parse_bucket(json, video_id)?;
        Ok(Self::new(plan_skips(&raw, duration, enabled), duration))
    }

    /// Whether there is anything at all to skip. A player can drop a skipper
    /// that says yes to this and stop polling it.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// The planned spans, for logging and tests.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Feed the current playback position; act on what comes back.
    ///
    /// Returns the position to seek to, or `None`. The caller is expected to
    /// call this every frame while playing and to perform the seek itself — this
    /// type never touches a player.
    pub fn on_position(&mut self, position: f64) -> Option<Skip> {
        if !position.is_finite() {
            return None;
        }
        if let Some(previous) = self.last_position
            && position < previous - BACKWARD_SEEK_TOLERANCE
        {
            self.restore_after(position);
        }
        self.last_position = Some(position);

        let index = self
            .segments
            .iter()
            .enumerate()
            .position(|(i, s)| s.contains(position) && !self.consumed[i])?;

        let segment = self.segments[index];
        let target = (segment.end).min(self.duration - EOF_MARGIN);
        if target - position < MIN_SKIP_SECONDS {
            // Close enough to the far side that seeking buys nothing. Mark it
            // consumed anyway: playing out the last moments of a span is not a
            // reason to keep testing it.
            self.consumed[index] = true;
            return None;
        }

        self.consumed[index] = true;
        self.last_position = Some(target);
        Some(Skip {
            target,
            category: segment.category,
        })
    }

    /// Un-consume every span that lies entirely ahead of `position`.
    ///
    /// A span the viewer landed *inside* stays consumed — they went back to see
    /// something in it, and skipping them forwards again would be the player
    /// arguing with them.
    fn restore_after(&mut self, position: f64) {
        for (i, segment) in self.segments.iter().enumerate() {
            if segment.start > position {
                self.consumed[i] = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One bucket entry, in the shape the service actually returns.
    fn bucket(video_id: &str, segments: &str) -> String {
        format!(r#"[{{"videoID":"{video_id}","segments":[{segments}]}}]"#)
    }

    fn seg(category: &str, start: f64, end: f64) -> String {
        format!(
            r#"{{"category":"{category}","actionType":"skip","segment":[{start},{end}],
               "UUID":"u","videoDuration":600.0,"locked":0,"votes":3,"description":""}}"#
        )
    }

    fn raw(category: Category, start: f64, end: f64) -> RawSegment {
        RawSegment {
            category,
            action: ActionType::Skip,
            start,
            end,
            video_duration: 600.0,
            votes: 3,
            locked: false,
        }
    }

    // --- parse_bucket ---

    #[test]
    fn a_bucket_is_filtered_down_to_the_video_asked_for() {
        let json = format!(
            r#"[{{"videoID":"other","segments":[{}]}},{{"videoID":"wanted","segments":[{}]}}]"#,
            seg("sponsor", 1.0, 2.0),
            seg("intro", 10.0, 20.0)
        );
        let parsed = parse_bucket(&json, "wanted").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].category, Category::Intro);
    }

    #[test]
    fn a_video_absent_from_the_bucket_is_not_an_error() {
        let json = bucket("someone-else", &seg("sponsor", 1.0, 2.0));
        assert!(parse_bucket(&json, "wanted").unwrap().is_empty());
    }

    #[test]
    fn an_unknown_category_or_action_is_dropped_rather_than_failing_the_parse() {
        let json = bucket(
            "v",
            &format!(
                r#"{{"category":"brand_new_thing","actionType":"skip","segment":[1,2],"videoDuration":600.0,"locked":0,"votes":1}},
                   {{"category":"sponsor","actionType":"levitate","segment":[3,4],"videoDuration":600.0,"locked":0,"votes":1}},
                   {}"#,
                seg("sponsor", 10.0, 20.0)
            ),
        );
        let parsed = parse_bucket(&json, "v").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].start, 10.0);
    }

    #[test]
    fn locked_and_votes_survive_parsing() {
        let json = bucket(
            "v",
            r#"{"category":"sponsor","actionType":"skip","segment":[5,15],"videoDuration":600.0,"locked":1,"votes":-2}"#,
        );
        let parsed = parse_bucket(&json, "v").unwrap();
        assert!(parsed[0].locked);
        assert_eq!(parsed[0].votes, -2);
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_bucket("not json", "v").is_err());
    }

    // --- plan_skips: what is actionable ---

    #[test]
    fn only_enabled_categories_are_planned() {
        let raws = vec![
            raw(Category::Sponsor, 10.0, 30.0),
            raw(Category::Filler, 40.0, 60.0),
        ];
        let plan = plan_skips(&raws, 600.0, DEFAULT_CATEGORIES);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].category, Category::Sponsor);
    }

    #[test]
    fn unskippable_categories_are_never_planned_even_when_enabled() {
        let raws = vec![
            raw(Category::PoiHighlight, 10.0, 30.0),
            raw(Category::Chapter, 40.0, 60.0),
        ];
        let plan = plan_skips(&raws, 600.0, Category::all());
        assert!(plan.is_empty());
    }

    #[test]
    fn non_skip_actions_are_ignored() {
        let mut muted = raw(Category::Sponsor, 10.0, 30.0);
        muted.action = ActionType::Mute;
        assert!(plan_skips(&[muted], 600.0, DEFAULT_CATEGORIES).is_empty());
    }

    #[test]
    fn downvoted_submissions_are_dropped() {
        let mut unloved = raw(Category::Sponsor, 10.0, 30.0);
        unloved.votes = -1;
        assert!(plan_skips(&[unloved], 600.0, DEFAULT_CATEGORIES).is_empty());
    }

    #[test]
    fn a_span_shorter_than_a_second_is_not_worth_a_seek() {
        let raws = vec![raw(Category::Sponsor, 100.0, 100.4)];
        assert!(plan_skips(&raws, 600.0, DEFAULT_CATEGORIES).is_empty());
    }

    #[test]
    fn no_duration_means_no_plan() {
        let raws = vec![raw(Category::Sponsor, 10.0, 30.0)];
        assert!(plan_skips(&raws, 0.0, DEFAULT_CATEGORIES).is_empty());
        assert!(plan_skips(&raws, f64::NAN, DEFAULT_CATEGORIES).is_empty());
    }

    // --- plan_skips: the duration rules ---

    #[test]
    fn the_whole_video_marker_is_not_a_span() {
        let raws = vec![raw(Category::Sponsor, 0.0, 0.0)];
        assert!(plan_skips(&raws, 600.0, DEFAULT_CATEGORIES).is_empty());
    }

    #[test]
    fn slop_at_the_start_snaps_to_zero_and_at_the_end_to_the_duration() {
        let raws = vec![
            raw(Category::Intro, 0.8, 20.0),
            raw(Category::Outro, 500.0, 599.5),
        ];
        let plan = plan_skips(&raws, 600.0, DEFAULT_CATEGORIES);
        assert_eq!(plan[0].start, 0.0);
        assert_eq!(plan[1].end, 600.0);
    }

    #[test]
    fn a_segment_submitted_against_a_different_cut_is_refused() {
        let mut stale = raw(Category::Sponsor, 10.0, 30.0);
        stale.video_duration = 500.0; // the file playing is 600s
        assert!(plan_skips(&[stale], 600.0, DEFAULT_CATEGORIES).is_empty());
    }

    #[test]
    fn a_small_duration_difference_is_tolerated_in_proportion_to_the_span() {
        // 4s out on a 600s video: refused for a 20s span (4/20 = 20%), accepted
        // for a 120s one (4/120 = 3.3%) — yt-dlp's rule, and the reason a long
        // span survives a re-encode that shifted the total slightly.
        let mut short = raw(Category::Sponsor, 10.0, 30.0);
        short.video_duration = 596.0;
        assert!(plan_skips(&[short], 600.0, DEFAULT_CATEGORIES).is_empty());

        let mut long = raw(Category::Sponsor, 10.0, 130.0);
        long.video_duration = 596.0;
        assert_eq!(plan_skips(&[long], 600.0, DEFAULT_CATEGORIES).len(), 1);
    }

    #[test]
    fn an_unknown_submitted_duration_is_accepted() {
        let mut unknown = raw(Category::Sponsor, 10.0, 30.0);
        unknown.video_duration = 0.0;
        assert_eq!(plan_skips(&[unknown], 600.0, DEFAULT_CATEGORIES).len(), 1);
    }

    #[test]
    fn a_span_past_the_end_of_the_file_is_dropped_and_one_that_runs_over_is_clamped() {
        let past = raw(Category::Sponsor, 700.0, 720.0);
        assert!(plan_skips(&[past], 600.0, DEFAULT_CATEGORIES).is_empty());

        let over = raw(Category::Sponsor, 550.0, 700.0);
        let plan = plan_skips(&[over], 600.0, DEFAULT_CATEGORIES);
        assert_eq!(plan[0].end, 600.0);
    }

    // --- plan_skips: overlapping claims ---

    #[test]
    fn overlapping_submissions_merge_into_their_union() {
        let raws = vec![
            raw(Category::Sponsor, 30.0, 60.0),
            raw(Category::Sponsor, 31.0, 62.0),
        ];
        let plan = plan_skips(&raws, 600.0, DEFAULT_CATEGORIES);
        assert_eq!(plan.len(), 1);
        assert_eq!((plan[0].start, plan[0].end), (30.0, 62.0));
    }

    #[test]
    fn a_locked_submission_overrules_the_unlocked_ones_it_overlaps() {
        let mut locked = raw(Category::Sponsor, 30.0, 45.0);
        locked.locked = true;
        let sprawling = raw(Category::Sponsor, 29.0, 90.0);
        let plan = plan_skips(&[locked, sprawling], 600.0, DEFAULT_CATEGORIES);
        assert_eq!(plan.len(), 1);
        assert_eq!((plan[0].start, plan[0].end), (30.0, 45.0));
    }

    #[test]
    fn disjoint_submissions_stay_separate_and_sorted() {
        let raws = vec![
            raw(Category::Outro, 500.0, 560.0),
            raw(Category::Sponsor, 30.0, 60.0),
        ];
        let plan = plan_skips(&raws, 600.0, DEFAULT_CATEGORIES);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].start, 30.0);
        assert_eq!(plan[1].start, 500.0);
    }

    // --- SegmentSkipper ---

    fn skipper(segments: &[(f64, f64)], duration: f64) -> SegmentSkipper {
        SegmentSkipper::new(
            segments
                .iter()
                .map(|&(start, end)| Segment {
                    category: Category::Sponsor,
                    start,
                    end,
                })
                .collect(),
            duration,
        )
    }

    #[test]
    fn playing_into_a_span_returns_its_far_side() {
        let mut s = skipper(&[(30.0, 60.0)], 600.0);
        assert!(s.on_position(29.5).is_none());
        assert_eq!(s.on_position(30.1).unwrap().target, 60.0);
    }

    #[test]
    fn a_span_is_skipped_only_once() {
        let mut s = skipper(&[(30.0, 60.0)], 600.0);
        assert!(s.on_position(30.1).is_some());
        // The viewer rewinds into the middle of it: they went back deliberately.
        assert!(s.on_position(45.0).is_none());
        assert!(s.on_position(50.0).is_none());
    }

    #[test]
    fn seeking_back_before_a_span_restores_it() {
        let mut s = skipper(&[(30.0, 60.0)], 600.0);
        assert!(s.on_position(30.1).is_some());
        assert!(s.on_position(10.0).is_none());
        assert_eq!(s.on_position(30.5).unwrap().target, 60.0);
    }

    #[test]
    fn position_jitter_is_not_a_seek() {
        let mut s = skipper(&[(30.0, 60.0)], 600.0);
        assert!(s.on_position(30.1).is_some());
        // mpv reports the post-seek position with a little wobble; the span
        // behind must not come back to life and re-fire.
        assert!(s.on_position(59.8).is_none());
        assert!(s.on_position(60.2).is_none());
    }

    #[test]
    fn a_restart_from_the_beginning_restores_everything() {
        let mut s = skipper(&[(30.0, 60.0), (200.0, 230.0)], 600.0);
        assert!(s.on_position(30.1).is_some());
        assert!(s.on_position(200.5).is_some());
        assert!(s.on_position(0.0).is_none());
        assert_eq!(s.on_position(30.1).unwrap().target, 60.0);
        assert_eq!(s.on_position(200.5).unwrap().target, 230.0);
    }

    #[test]
    fn an_intro_at_zero_is_skipped_on_the_first_frame() {
        let mut s = skipper(&[(0.0, 25.0)], 600.0);
        assert_eq!(s.on_position(0.0).unwrap().target, 25.0);
    }

    #[test]
    fn a_span_running_to_the_end_stops_short_of_it() {
        let mut s = skipper(&[(540.0, 600.0)], 600.0);
        let skip = s.on_position(541.0).unwrap();
        assert_eq!(skip.target, 600.0 - EOF_MARGIN);
        assert!(
            skip.target < 600.0,
            "a seek to the duration races the backend"
        );
    }

    #[test]
    fn arriving_near_the_far_side_does_not_seek() {
        let mut s = skipper(&[(30.0, 60.0)], 600.0);
        // A resume position landing in the last moments of a sponsor: seeking
        // half a second is more disruptive than playing it out.
        assert!(s.on_position(59.7).is_none());
        // And it does not keep asking on the way out.
        assert!(s.on_position(59.9).is_none());
    }

    #[test]
    fn adjacent_spans_are_skipped_in_turn() {
        let mut s = skipper(&[(30.0, 60.0), (60.0, 90.0)], 600.0);
        assert_eq!(s.on_position(30.1).unwrap().target, 60.0);
        assert_eq!(s.on_position(60.0).unwrap().target, 90.0);
    }

    #[test]
    fn a_skipper_with_no_segments_reports_itself_empty() {
        let mut s = skipper(&[], 600.0);
        assert!(s.is_empty());
        assert!(s.on_position(10.0).is_none());
    }

    #[test]
    fn a_non_finite_position_is_ignored() {
        let mut s = skipper(&[(30.0, 60.0)], 600.0);
        assert!(s.on_position(f64::NAN).is_none());
        assert_eq!(s.on_position(30.1).unwrap().target, 60.0);
    }

    #[test]
    fn from_bucket_wires_the_three_stages_together() {
        let json = bucket(
            "v",
            &format!(
                "{},{}",
                seg("sponsor", 30.0, 60.0),
                seg("filler", 100.0, 130.0)
            ),
        );
        let mut s = SegmentSkipper::from_bucket(&json, "v", 600.0, DEFAULT_CATEGORIES).unwrap();
        assert_eq!(s.segments().len(), 1, "filler is not a default category");
        assert_eq!(s.on_position(31.0).unwrap().category, Category::Sponsor);
    }

    // --- the request ---

    #[test]
    fn a_bucket_url_asks_for_every_skippable_category() {
        let url = bucket_url("https://sponsor.ajay.app", "5f6b");
        assert!(
            url.starts_with("https://sponsor.ajay.app/api/skipSegments/5f6b?"),
            "{url}"
        );
        assert!(url.contains("categories="), "{url}");
        assert!(url.contains("sponsor"), "{url}");
        assert!(url.contains("music_offtopic"), "{url}");
        assert!(
            !url.contains("poi_highlight") && !url.contains("chapter"),
            "markers are not requested: {url}"
        );
        assert!(url.contains("actionTypes="), "{url}");
        // The JSON array must be encoded, or the brackets and quotes travel raw.
        assert!(!url.contains('['), "{url}");
    }

    #[test]
    fn a_trailing_slash_on_the_api_does_not_double_up() {
        assert_eq!(
            bucket_url("https://sb.example/", "abcd"),
            bucket_url("https://sb.example", "abcd")
        );
    }

    // --- categories ---

    #[test]
    fn category_names_round_trip() {
        for &c in Category::all() {
            assert_eq!(Category::parse(c.as_str()), Some(c), "{}", c.as_str());
        }
        assert_eq!(Category::parse("not_a_category"), None);
    }
}
