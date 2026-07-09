//! Linux entry point for the `shepherd-media` library launcher.

mod cli;
mod connectivity;
mod ordering;
mod paths;
mod posters;
mod ui;
mod video_cache;
mod youtube;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use shepherd_media_core::{
    LibmpvPlayer, Library, ProtocolEmitter, Session, build_library_from_entries,
    is_youtube_playlist_url, load_library, resolve_source,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command, SortBy};
use crate::ordering::apply_ordering;
use crate::ui::StartMode;
use crate::video_cache::{CachingPlayer, VideoCache};

/// Exit codes per the spec; keep in sync with `docs/shepherd-media.md`.
const EXIT_OK: u8 = 0;
const EXIT_VALIDATION: u8 = 1;
const EXIT_INVOCATION: u8 = 2;
const EXIT_PLAYER: u8 = 3;
const EXIT_SIGNAL: u8 = 4;

fn main() -> ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(cli.log_level.as_filter())),
        )
        .init();

    let ytdl_format = cli.quality.ytdl_format();
    let sort_by = cli.sort_by;
    let reverse = cli.reverse;
    let result = match &cli.command {
        Command::Validate { library } => run_validate(library, sort_by, reverse),
        Command::Play { library, item } => run_play(
            library,
            item,
            cli.no_protocol,
            ytdl_format,
            sort_by,
            reverse,
        ),
        Command::Browse { library } => run_browse(
            library,
            cli.no_protocol,
            cli.connectivity_check.as_deref(),
            ytdl_format,
            sort_by,
            reverse,
        ),
    };

    ExitCode::from(result)
}

/// Load a library from either a file path or a YouTube playlist URL,
/// then apply the CLI-selected ordering.
fn load_library_from_source(
    source: &str,
    sort_by: SortBy,
    reverse: bool,
) -> Result<Library, (u8, String)> {
    let mut library = if is_youtube_playlist_url(source) {
        let info = youtube::fetch_playlist(source).map_err(|e| (EXIT_VALIDATION, e))?;
        build_library_from_entries(
            source,
            info.title,
            info.playlist_id.as_deref(),
            &info.entries,
        )
    } else {
        load_library(Path::new(source)).map_err(|e| (EXIT_VALIDATION, e.to_string()))?
    };
    apply_ordering(&mut library, sort_by, reverse);
    Ok(library)
}

fn run_validate(library_source: &str, sort_by: SortBy, reverse: bool) -> u8 {
    match load_library_from_source(library_source, sort_by, reverse) {
        Ok(lib) => {
            println!(
                "OK: library_id={} items={}",
                lib.library_id,
                lib.items.len()
            );
            EXIT_OK
        }
        Err((code, msg)) => {
            eprintln!("validation failed: {msg}");
            code
        }
    }
}

fn run_play(
    library_source: &str,
    item_id: &str,
    no_protocol: bool,
    ytdl_format: &str,
    sort_by: SortBy,
    reverse: bool,
) -> u8 {
    let library = match load_library_from_source(library_source, sort_by, reverse) {
        Ok(l) => l,
        Err((code, msg)) => {
            eprintln!("validation failed: {msg}");
            return code;
        }
    };

    let info = shepherd_media_core::PlatformInfo::current();
    let item = match library.items.iter().find(|i| i.id == item_id) {
        Some(i) => i,
        None => {
            eprintln!("item `{item_id}` not found in library");
            return EXIT_INVOCATION;
        }
    };
    if resolve_source(item, &info).is_none() {
        eprintln!("item `{item_id}` has no source for the current platform");
        return EXIT_INVOCATION;
    }

    // Direct-play mode shares the eframe shell with browse mode; the UI
    // opens straight into the playback view instead of the grid.
    let cache = VideoCache::new(ytdl_format);
    let session = build_session(library, no_protocol, ytdl_format, cache.clone());
    let session = match session {
        Ok(s) => s,
        Err(code) => return code,
    };

    let term = install_signal_handler();
    let online = Arc::new(AtomicBool::new(true));

    info!("starting direct-play UI");
    match ui::run(
        session,
        term,
        online,
        cache,
        StartMode::Playing(item_id.to_string()),
    ) {
        Ok(ui::ExitCause::User) => EXIT_OK,
        Ok(ui::ExitCause::Signal) => EXIT_SIGNAL,
        Err(e) => {
            error!("direct-play UI failed: {e}");
            EXIT_PLAYER
        }
    }
}

fn run_browse(
    library_source: &str,
    no_protocol: bool,
    connectivity_check: Option<&str>,
    ytdl_format: &str,
    sort_by: SortBy,
    reverse: bool,
) -> u8 {
    let library = match load_library_from_source(library_source, sort_by, reverse) {
        Ok(l) => l,
        Err((code, msg)) => {
            eprintln!("validation failed: {msg}");
            return code;
        }
    };

    let cache = VideoCache::new(ytdl_format);
    if let Some(ref c) = cache {
        c.queue_all(&library);
    }
    let session = build_session(library, no_protocol, ytdl_format, cache.clone());
    let session = match session {
        Ok(s) => s,
        Err(code) => return code,
    };

    // Connectivity check: start as "online" when no check URL is given so
    // that all items are shown by default. When a URL is given, start as
    // "offline" (pessimistic) and update within CHECK_TIMEOUT seconds.
    let online = match connectivity_check {
        Some(url) => connectivity::spawn_checker(url.to_string()),
        None => Arc::new(AtomicBool::new(true)),
    };

    info!("starting browse UI");
    let term = install_signal_handler();
    match ui::run(session, term, online, cache, StartMode::Browsing) {
        Ok(ui::ExitCause::User) => EXIT_OK,
        Ok(ui::ExitCause::Signal) => EXIT_SIGNAL,
        Err(e) => {
            error!("browse UI failed: {e}");
            EXIT_PLAYER
        }
    }
}

fn build_session(
    library: Library,
    no_protocol: bool,
    ytdl_format: &str,
    cache: Option<Arc<VideoCache>>,
) -> Result<Session, u8> {
    // Desktop GPUs render mpv's default (full-quality) path fine; only the
    // Android TV build needs the `fast` profile.
    let inner: Box<dyn shepherd_media_core::PlayerHandle> =
        match LibmpvPlayer::new(ytdl_format, false) {
            Ok(p) => Box::new(p),
            Err(e) => {
                error!("failed to construct libmpv player: {e}");
                return Err(EXIT_PLAYER);
            }
        };
    let player: Box<dyn shepherd_media_core::PlayerHandle> = match cache {
        Some(c) => Box::new(CachingPlayer::new(inner, c, &library)),
        None => inner,
    };
    let emitter = if no_protocol {
        ProtocolEmitter::disabled()
    } else {
        ProtocolEmitter::stdout()
    };
    Ok(Session::with_emitter(library, player, emitter))
}

fn install_signal_handler() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);
    // SAFETY: `signal_hook`-style installation isn't available here; use nix
    // directly. The handler only flips an atomic, which is async-signal-safe.
    unsafe {
        use nix::sys::signal::{SigAction, SigHandler, Signal, sigaction};
        extern "C" fn handler(_: nix::libc::c_int) {
            FLAG.store(true, Ordering::SeqCst);
        }
        static FLAG: AtomicBool = AtomicBool::new(false);
        let action = SigAction::new(
            SigHandler::Handler(handler),
            nix::sys::signal::SaFlags::empty(),
            nix::sys::signal::SigSet::empty(),
        );
        let _ = sigaction(Signal::SIGTERM, &action);
        let _ = sigaction(Signal::SIGINT, &action);

        std::thread::spawn(move || {
            loop {
                if FLAG.swap(false, Ordering::SeqCst) {
                    flag_clone.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
    flag
}
