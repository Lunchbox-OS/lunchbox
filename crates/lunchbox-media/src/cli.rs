//! `clap`-derived CLI surface for `lunchbox-media`.

use clap::{Parser, Subcommand, ValueEnum};
use lunchbox_media_core::sponsorblock::Category;

/// Video quality preset. Re-exported from `lunchbox-media-app` (shared with the
/// Android app) so both front-ends map `--quality` to the same yt-dlp format
/// selector; the `clap` feature makes it usable directly as a clap value.
pub use lunchbox_media_app::Quality;

#[derive(Debug, Parser)]
#[command(name = "lunchbox-media")]
#[command(about = "shepherd-launcher media-library activity", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Logging verbosity for stderr.
    #[arg(long, value_enum, default_value_t = LogLevel::Info, global = true)]
    pub log_level: LogLevel,

    /// Suppress the stdout protocol stream. Useful when running by hand.
    #[arg(long, global = true)]
    pub no_protocol: bool,

    /// Maximum video quality for playback and background downloads.
    #[arg(long, value_enum, default_value = "1080p", global = true)]
    pub quality: Quality,

    /// Field used to sort library items before display or lookup.
    /// `library` preserves the order from the source file/playlist.
    /// Sort is stable, so library order breaks ties.
    #[arg(long, value_enum, default_value_t = SortBy::Library, global = true)]
    pub sort_by: SortBy,

    /// Reverse the final item order. Combines with `--sort-by`; on the
    /// default `--sort-by library` this just flips the file order.
    #[arg(long, global = true)]
    pub reverse: bool,

    /// Remember playback positions for this library, so an item re-opened
    /// later picks up where it stopped and `browse` offers to continue the
    /// item watched most recently. Off by default; with the flag absent
    /// nothing is recorded and no state file is written.
    ///
    /// Positions live in `$XDG_STATE_HOME/shepherd/media/resume/<library_id>.toml`
    /// (falling back to `~/.local/state`). An item watched to its end is
    /// forgotten, so the next play starts from the beginning.
    #[arg(long, global = true)]
    pub resume: bool,

    /// URL used to probe internet connectivity (e.g. `https://example.com`
    /// or `tcp://8.8.8.8:53`). When provided, browse mode polls
    /// reachability every 10 seconds and hides library items that are only
    /// available online when the check fails. Ignored by `validate` and
    /// `play`. Accepts the same format as lunchboxd's `internet.check`
    /// config field, so the value can be forwarded directly from the
    /// launcher. When absent, all items are shown regardless of
    /// connectivity.
    #[arg(long, global = true)]
    pub connectivity_check: Option<String>,

    /// How long, in days, watching a video protects its cached copy from being
    /// displaced by a speculative download.
    ///
    /// Inside the window a background download can never cost the viewer
    /// something they chose; past it, the file competes on age like anything
    /// else, so a video watched once months ago eventually yields to one added
    /// to the library this week. 0 drops the protection and orders purely by
    /// age.
    ///
    /// lunchboxd passes its `service.media.watched_grace_days` here, because
    /// the cache directory this writes to is the one lunchboxd prefetches into:
    /// two processes valuing its contents differently would undo each other's
    /// trims.
    #[arg(long, default_value_t = lunchbox_media_cache::DEFAULT_WATCHED_GRACE_DAYS, global = true)]
    pub watched_grace_days: u64,

    /// Maximum total size of the on-disk video cache, in bytes.
    ///
    /// lunchboxd passes its `service.media.cache_max_bytes` here, because the
    /// cache directory this trims is the one lunchboxd prefetches into: two
    /// processes disagreeing about how big it may be would undo each other's
    /// trims. `SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES` still overrides it, as a
    /// local debugging escape hatch.
    #[arg(long, default_value_t = lunchbox_media_cache::DEFAULT_MAX_CACHE_BYTES, global = true)]
    pub cache_max_bytes: u64,

    /// SponsorBlock categories to skip in YouTube videos, comma-separated
    /// (e.g. `sponsor,selfpromo,interaction,intro,outro`).
    ///
    /// Absent or empty turns the feature off entirely, which is the default:
    /// with no categories no lookup is started and nothing reaches
    /// sponsor.ajay.app. lunchboxd passes its
    /// `service.media.sponsorblock.categories` here when a parent has enabled
    /// it.
    ///
    /// Valid names are the service's own: sponsor, selfpromo, interaction,
    /// intro, outro, preview, filler, music_offtopic, hook. `poi_highlight`
    /// and `chapter` are markers rather than spans and are rejected.
    #[arg(long, value_delimiter = ',', global = true)]
    pub sponsorblock_categories: Vec<String>,

    /// SponsorBlock API base URL, for a self-hosted mirror.
    #[arg(long, default_value = lunchbox_media_cache::SPONSORBLOCK_API, global = true)]
    pub sponsorblock_api: String,
}

/// Resolve `--sponsorblock-categories` into the core's category type.
///
/// An unrecognised name is an error rather than a warning: it is a parent's
/// config saying "skip this" and silently not skipping it is the wrong failure.
/// The two marker categories are named explicitly in the message because
/// they *are* real SponsorBlock categories — they just do not describe a span
/// anything can jump over.
pub fn parse_categories(names: &[String]) -> Result<Vec<Category>, String> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        match Category::parse(name) {
            Some(c) if c.is_skippable() => {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
            Some(c) => {
                return Err(format!(
                    "`{}` is a SponsorBlock marker, not a skippable span",
                    c.as_str()
                ));
            }
            None => {
                let valid: Vec<&str> = Category::all()
                    .iter()
                    .filter(|c| c.is_skippable())
                    .map(|c| c.as_str())
                    .collect();
                return Err(format!(
                    "unknown SponsorBlock category `{name}`; valid categories are {}",
                    valid.join(", ")
                ));
            }
        }
    }
    Ok(out)
}

/// How to order library items before display or lookup.
///
/// Items missing the chosen field (e.g. no `category` or `duration_seconds`)
/// sort to the end in ascending order.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum SortBy {
    /// Preserve the order from the library file or playlist (default).
    Library,
    /// Display title, case-insensitive.
    Title,
    /// Stable item id.
    Id,
    /// Item kind (audio before video).
    Kind,
    /// Optional category string, case-insensitive.
    Category,
    /// Optional duration in seconds, ascending.
    Duration,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Parse and validate a library file or YouTube playlist URL.
    Validate {
        /// Path to a library `.toml`, `.m3u`, or `.m3u8`, or a YouTube
        /// playlist URL (e.g. `https://www.youtube.com/playlist?list=PL…`).
        /// YouTube URLs require `yt-dlp` to be installed.
        library: String,
    },
    /// Direct-play mode: launch a single item end-to-end.
    Play {
        /// Path to a library `.toml`, `.m3u`, or `.m3u8`, or a YouTube
        /// playlist URL.
        #[arg(long)]
        library: String,
        #[arg(long)]
        item: String,
    },
    /// Browse mode: open the poster-grid UI.
    Browse {
        /// Path to a library `.toml`, `.m3u`, or `.m3u8`, or a YouTube
        /// playlist URL (e.g. `https://www.youtube.com/playlist?list=PL…`).
        /// YouTube URLs require `yt-dlp` to be installed.
        #[arg(long)]
        library: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_filter(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    /// lunchboxd builds this CLI's argv from `lunchbox_api::EntryKind::Media`
    /// (issue #127), and its `MediaQuality` / `MediaSortBy` mirrors are
    /// hand-maintained — `lunchbox-api` cannot depend on the media crates.
    /// This is the guard: every flag string those mirrors emit must still be a
    /// value this CLI accepts, so renaming or dropping one here fails the
    /// build rather than the child's launch.
    #[test]
    fn api_media_flags_are_accepted_by_this_cli() {
        for q in [
            lunchbox_api::MediaQuality::Best,
            lunchbox_api::MediaQuality::Q1080,
            lunchbox_api::MediaQuality::Q720,
            lunchbox_api::MediaQuality::Q480,
        ] {
            Quality::from_str(q.as_flag(), true)
                .unwrap_or_else(|e| panic!("--quality {}: {e}", q.as_flag()));
        }

        for s in [
            lunchbox_api::MediaSortBy::Library,
            lunchbox_api::MediaSortBy::Title,
            lunchbox_api::MediaSortBy::Id,
            lunchbox_api::MediaSortBy::Kind,
            lunchbox_api::MediaSortBy::Category,
            lunchbox_api::MediaSortBy::Duration,
        ] {
            SortBy::from_str(s.as_flag(), true)
                .unwrap_or_else(|e| panic!("--sort-by {}: {e}", s.as_flag()));
        }
    }

    /// The other direction: a preset added to the CLI without a mirror in
    /// `lunchbox-api` would be unreachable from config, silently.
    #[test]
    fn this_cli_has_no_presets_the_api_cannot_name() {
        assert_eq!(Quality::value_variants().len(), 4);
        assert_eq!(SortBy::value_variants().len(), 6);
    }

    #[test]
    fn categories_parse_and_deduplicate() {
        let names = ["sponsor", "intro", "sponsor"].map(String::from);
        assert_eq!(
            parse_categories(&names).unwrap(),
            vec![Category::Sponsor, Category::Intro]
        );
    }

    #[test]
    fn no_categories_is_the_off_switch_not_an_error() {
        assert!(parse_categories(&[]).unwrap().is_empty());
        assert!(parse_categories(&[String::from("")]).unwrap().is_empty());
    }

    /// A typo in a parent's config must not quietly stop skipping.
    #[test]
    fn an_unknown_category_is_rejected_with_the_valid_ones() {
        let err = parse_categories(&[String::from("sponsors")]).unwrap_err();
        assert!(err.contains("sponsors"), "{err}");
        assert!(err.contains("selfpromo"), "{err}");
        assert!(
            !err.contains("poi_highlight"),
            "markers are not offered: {err}"
        );
    }

    #[test]
    fn a_marker_category_is_rejected_as_unskippable() {
        let err = parse_categories(&[String::from("chapter")]).unwrap_err();
        assert!(err.contains("marker"), "{err}");
    }
}
