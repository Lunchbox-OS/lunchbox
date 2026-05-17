//! `clap`-derived CLI surface for `shepherd-media`.

use clap::{Parser, Subcommand, ValueEnum};

/// Video quality preset.  Maps to a yt-dlp format selector used both by the
/// live player (mpv's `ytdl-format` property) and the video cache downloader.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Quality {
    /// No height restriction — download the best available quality.
    Best,
    /// Up to 1080p (default).
    #[value(name = "1080p")]
    Q1080,
    /// Up to 720p.
    #[value(name = "720p")]
    Q720,
    /// Up to 480p.
    #[value(name = "480p")]
    Q480,
}

impl Quality {
    /// Returns the yt-dlp `--format` / mpv `ytdl-format` string for this preset.
    pub fn ytdl_format(self) -> &'static str {
        match self {
            Quality::Best => "bestvideo+bestaudio/best",
            Quality::Q1080 => "bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best",
            Quality::Q720 => "bestvideo[height<=?720]+bestaudio/best[height<=?720]/best",
            Quality::Q480 => "bestvideo[height<=?480]+bestaudio/best[height<=?480]/best",
        }
    }
}

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
