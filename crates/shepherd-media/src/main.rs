//! Linux entry point for the `shepherd-media` library launcher.

mod cli;
mod platform;
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
    LibmpvPlayer, Library, ProtocolEmitter, Session, SessionInput, build_library_from_entries,
    is_youtube_playlist_url, load_library, resolve_source,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};
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

    let result = match &cli.command {
        Command::Validate { library } => run_validate(library),
        Command::Play { library, item } => run_play(library, item, cli.no_protocol),
        Command::Browse { library } => run_browse(library, cli.no_protocol),
    };

    ExitCode::from(result)
}

/// Load a library from either a file path or a YouTube playlist URL.
///
/// YouTube playlist URLs (those with a `list=` query parameter on a YouTube
/// host) are fetched via `yt-dlp`. Everything else is treated as a file path
/// and dispatched to `load_library`, which handles TOML, M3U, and M3U8.
fn load_library_from_source(source: &str) -> Result<Library, (u8, String)> {
    if is_youtube_playlist_url(source) {
        let info = youtube::fetch_playlist(source).map_err(|e| (EXIT_VALIDATION, e))?;
        Ok(build_library_from_entries(
            source,
            info.title,
            info.playlist_id.as_deref(),
            &info.entries,
        ))
    } else {
        load_library(Path::new(source)).map_err(|e| (EXIT_VALIDATION, e.to_string()))
    }
}

fn run_validate(library_source: &str) -> u8 {
    match load_library_from_source(library_source) {
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

fn run_play(library_source: &str, item_id: &str, no_protocol: bool) -> u8 {
    let library = match load_library_from_source(library_source) {
        Ok(l) => l,
        Err((code, msg)) => {
            eprintln!("validation failed: {msg}");
            return code;
        }
    };

    if !library.items.iter().any(|i| i.id == item_id) {
        eprintln!("item `{item_id}` not found in library");
        return EXIT_INVOCATION;
    }

    let info = platform::current();
    let item = library.items.iter().find(|i| i.id == item_id).unwrap();
    if resolve_source(item, &info).is_none() {
        eprintln!("item `{item_id}` has no source for the current platform");
        return EXIT_INVOCATION;
    }

    let inner: Box<dyn shepherd_media_core::PlayerHandle> = match LibmpvPlayer::new() {
        Ok(p) => Box::new(p),
        Err(e) => {
            error!("failed to construct libmpv player: {e}");
            return EXIT_PLAYER;
        }
    };

    // Cache lookup only — no queue_all for single-shot play.
    let player: Box<dyn shepherd_media_core::PlayerHandle> = match VideoCache::new() {
        Some(cache) => Box::new(CachingPlayer::new(inner, cache, &library)),
        None => inner,
    };

    let emitter = if no_protocol {
        ProtocolEmitter::disabled()
    } else {
        ProtocolEmitter::stdout()
    };
    let mut session = Session::with_emitter(library, player, emitter);

    let term = install_signal_handler();

    session.announce_ready();
    session.handle_input(SessionInput::SelectItem(item_id.to_string()));

    while !session.is_exiting() {
        if term.swap(false, Ordering::SeqCst) {
            session.handle_input(SessionInput::SignalTerminate);
            return EXIT_SIGNAL;
        }
        session.tick();
        // After processing, exit when the player returns to Browsing — direct
        // play mode is single-shot.
        if matches!(session.state(), shepherd_media_core::SessionState::Browsing) {
            session.handle_input(SessionInput::ExitSession);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    EXIT_OK
}

fn run_browse(library_source: &str, no_protocol: bool) -> u8 {
    let library = match load_library_from_source(library_source) {
        Ok(l) => l,
        Err((code, msg)) => {
            eprintln!("validation failed: {msg}");
            return code;
        }
    };

    let inner: Box<dyn shepherd_media_core::PlayerHandle> = match LibmpvPlayer::new() {
        Ok(p) => Box::new(p),
        Err(e) => {
            error!("failed to construct libmpv player: {e}");
            return EXIT_PLAYER;
        }
    };

    // Option A: queue every remote library item for background download so
    // subsequent plays serve from the local cache.
    let player: Box<dyn shepherd_media_core::PlayerHandle> = match VideoCache::new() {
        Some(cache) => {
            cache.queue_all(&library);
            Box::new(CachingPlayer::new(inner, cache, &library))
        }
        None => inner,
    };

    let emitter = if no_protocol {
        ProtocolEmitter::disabled()
    } else {
        ProtocolEmitter::stdout()
    };
    let session = Session::with_emitter(library, player, emitter);

    info!("starting browse UI");
    let term = install_signal_handler();
    match ui::run(session, term) {
        Ok(reason) => match reason {
            ui::ExitCause::User => EXIT_OK,
            ui::ExitCause::Signal => EXIT_SIGNAL,
        },
        Err(e) => {
            error!("browse UI failed: {e}");
            EXIT_PLAYER
        }
    }
}

fn install_signal_handler() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);
    // SAFETY: `signal_hook`-style installation isn't available here; use nix
    // directly. The handler only flips an atomic, which is async-signal-safe.
    unsafe {
        use nix::sys::signal::{SigAction, SigHandler, Signal, sigaction};
        extern "C" fn handler(_: nix::libc::c_int) {
            // Set the flag via a global atomic; we go through a static so the
            // handler doesn't need to capture state.
            FLAG.store(true, Ordering::SeqCst);
        }
        // A static atomic referenced by the C handler.
        static FLAG: AtomicBool = AtomicBool::new(false);
        let action = SigAction::new(
            SigHandler::Handler(handler),
            nix::sys::signal::SaFlags::empty(),
            nix::sys::signal::SigSet::empty(),
        );
        let _ = sigaction(Signal::SIGTERM, &action);
        let _ = sigaction(Signal::SIGINT, &action);

        // Bridge the static FLAG into the per-call Arc by spawning a watcher.
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
