//! Playback backend abstraction.
//!
//! The `PlayerHandle` trait is the seam that lets the same session state
//! machine drive Linux libmpv and a future Android JNI implementation.
//! Keep libmpv-specific types out of every other module — they belong here
//! and nowhere else.

use thiserror::Error;

use crate::library::Source;

/// Operations on a media player. Calls are non-blocking with respect to
/// playback: `play` returns once mpv has accepted the command, not when
/// playback ends.
pub trait PlayerHandle: Send {
    fn play(&mut self, source: &Source) -> Result<(), PlayerError>;
    fn stop(&mut self) -> Result<(), PlayerError>;
    fn is_playing(&self) -> bool;
    fn poll_event(&mut self) -> Option<PlayerEvent>;
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Started,
    EndOfFile,
    Error(String),
    Closed,
}

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("player backend error: {0}")]
    Backend(String),

    #[error("invalid source for backend: {0}")]
    InvalidSource(String),
}

#[cfg(feature = "libmpv")]
pub use libmpv_backend::LibmpvPlayer;

#[cfg(feature = "libmpv")]
mod libmpv_backend {
    use std::sync::atomic::{AtomicBool, Ordering};

    use libmpv2::Mpv;
    use libmpv2::events::{Event, PropertyData};

    use super::{PlayerError, PlayerEvent, PlayerHandle};
    use crate::library::{ClassifiedUri, Source};

    // Threshold mpv log levels at which we surface a player Error event back
    // to the session. mpv numbers log levels with FATAL=10, ERROR=20, WARN=30,
    // INFO=40 and so on — lower is more severe.
    const MPV_LOG_LEVEL_ERROR: u32 = 20;

    /// libmpv-backed `PlayerHandle`. Constructed once per session; `play`
    /// reuses the same mpv instance to swap files via `loadfile`.
    pub struct LibmpvPlayer {
        mpv: Mpv,
        playing: AtomicBool,
    }

    impl LibmpvPlayer {
        pub fn new() -> Result<Self, PlayerError> {
            let mpv = Mpv::with_initializer(|init| {
                // Suppress mpv's default keybindings; the launcher owns input.
                init.set_property("input-default-bindings", "no")?;
                init.set_property("input-vo-keyboard", "no")?;
                init.set_property("osc", "no")?;
                init.set_property("keep-open", "no")?;
                init.set_property("fullscreen", "yes")?;
                init.set_property("ytdl", "yes")?;
                init.set_property(
                    "ytdl-format",
                    "bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best",
                )?;
                Ok(())
            })
            .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;

            // Observe `idle-active` so we can detect mpv returning to idle
            // (window-closed or stop) as a Closed event.
            mpv.observe_property("idle-active", libmpv2::Format::Flag, 0)
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;

            Ok(Self {
                mpv,
                playing: AtomicBool::new(false),
            })
        }

        fn uri_for_source(source: &Source) -> Result<String, PlayerError> {
            match &source.uri {
                ClassifiedUri::Local(path) => {
                    path.to_str().map(|s| s.to_string()).ok_or_else(|| {
                        PlayerError::InvalidSource(format!("non-UTF-8 path: {}", path.display()))
                    })
                }
                ClassifiedUri::DirectHttp(url)
                | ClassifiedUri::YouTube(url)
                | ClassifiedUri::Unknown(url) => Ok(url.to_string()),
            }
        }
    }

    impl PlayerHandle for LibmpvPlayer {
        fn play(&mut self, source: &Source) -> Result<(), PlayerError> {
            let uri = Self::uri_for_source(source)?;
            self.mpv
                .command("loadfile", &[&uri, "replace"])
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;
            self.playing.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn stop(&mut self) -> Result<(), PlayerError> {
            // `stop` clears the playlist and returns mpv to idle. We then let
            // the event loop emit Closed once idle-active fires.
            self.mpv
                .command("stop", &[])
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;
            Ok(())
        }

        fn is_playing(&self) -> bool {
            self.playing.load(Ordering::SeqCst)
        }

        fn poll_event(&mut self) -> Option<PlayerEvent> {
            // Non-blocking poll. mpv returns None when the queue is empty.
            let event = self.mpv.wait_event(0.0)?;
            match event {
                Ok(ev) => match ev {
                    Event::StartFile => Some(PlayerEvent::Started),
                    Event::EndFile(_) => {
                        self.playing.store(false, Ordering::SeqCst);
                        Some(PlayerEvent::EndOfFile)
                    }
                    Event::Shutdown => {
                        self.playing.store(false, Ordering::SeqCst);
                        Some(PlayerEvent::Closed)
                    }
                    Event::PropertyChange { name, change, .. } => match (name, change) {
                        ("idle-active", PropertyData::Flag(true)) => {
                            // Reaching idle without an explicit EOF or shutdown
                            // means the user closed mpv's window or `stop` was
                            // issued. Either way the session machine treats it
                            // as Closed.
                            if self.playing.swap(false, Ordering::SeqCst) {
                                Some(PlayerEvent::Closed)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    },
                    Event::LogMessage {
                        log_level, text, ..
                    } if log_level <= MPV_LOG_LEVEL_ERROR => {
                        // mpv's log_level numbering: lower is more severe.
                        // FATAL=10, ERROR=20, WARN=30, ...
                        Some(PlayerEvent::Error(text.to_owned()))
                    }
                    _ => None,
                },
                Err(e) => Some(PlayerEvent::Error(e.to_string())),
            }
        }
    }
}
