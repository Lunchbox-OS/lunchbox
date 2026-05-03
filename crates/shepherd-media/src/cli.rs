//! `clap`-derived CLI surface for `shepherd-media`.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Parse and validate a library file.
    Validate {
        /// Path to the library `.toml`.
        library: PathBuf,
    },
    /// Direct-play mode: launch a single item end-to-end.
    Play {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        item: String,
    },
    /// Browse mode: open the poster-grid UI.
    Browse {
        #[arg(long)]
        library: PathBuf,
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
