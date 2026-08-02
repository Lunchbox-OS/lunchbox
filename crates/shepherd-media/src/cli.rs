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
