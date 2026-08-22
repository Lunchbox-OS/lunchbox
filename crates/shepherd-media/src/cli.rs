//! `clap`-derived CLI surface for `shepherd-media`.

use clap::{Parser, Subcommand, ValueEnum};

/// Video quality preset. Re-exported from `shepherd-media-app` (shared with the
/// Android app) so both front-ends map `--quality` to the same yt-dlp format
/// selector; the `clap` feature makes it usable directly as a clap value.
pub use shepherd_media_app::Quality;

#[derive(Debug, Parser)]
#[command(name = "shepherd-media")]
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
    /// `play`. Accepts the same format as shepherdd's `internet.check`
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
    /// shepherdd passes its `service.media.watched_grace_days` here, because
    /// the cache directory this writes to is the one shepherdd prefetches into:
    /// two processes valuing its contents differently would undo each other's
    /// trims.
    #[arg(long, default_value_t = shepherd_media_cache::DEFAULT_WATCHED_GRACE_DAYS, global = true)]
    pub watched_grace_days: u64,
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

    /// shepherdd builds this CLI's argv from `shepherd_api::EntryKind::Media`
    /// (issue #127), and its `MediaQuality` / `MediaSortBy` mirrors are
    /// hand-maintained — `shepherd-api` cannot depend on the media crates.
    /// This is the guard: every flag string those mirrors emit must still be a
    /// value this CLI accepts, so renaming or dropping one here fails the
    /// build rather than the child's launch.
    #[test]
    fn api_media_flags_are_accepted_by_this_cli() {
        for q in [
            shepherd_api::MediaQuality::Best,
            shepherd_api::MediaQuality::Q1080,
            shepherd_api::MediaQuality::Q720,
            shepherd_api::MediaQuality::Q480,
        ] {
            Quality::from_str(q.as_flag(), true)
                .unwrap_or_else(|e| panic!("--quality {}: {e}", q.as_flag()));
        }

        for s in [
            shepherd_api::MediaSortBy::Library,
            shepherd_api::MediaSortBy::Title,
            shepherd_api::MediaSortBy::Id,
            shepherd_api::MediaSortBy::Kind,
            shepherd_api::MediaSortBy::Category,
            shepherd_api::MediaSortBy::Duration,
        ] {
            SortBy::from_str(s.as_flag(), true)
                .unwrap_or_else(|e| panic!("--sort-by {}: {e}", s.as_flag()));
        }
    }

    /// The other direction: a preset added to the CLI without a mirror in
    /// `shepherd-api` would be unreachable from config, silently.
    #[test]
    fn this_cli_has_no_presets_the_api_cannot_name() {
        assert_eq!(Quality::value_variants().len(), 4);
        assert_eq!(SortBy::value_variants().len(), 6);
    }
}
