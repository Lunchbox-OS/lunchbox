//! Session state machine.
//!
//! `Session` owns the player handle and the protocol emitter, and runs the
//! browse-vs-play state machine described in the spec. It is the single piece
//! of `shepherd-media-core` that has any opinion about the order in which
//! things happen at runtime.

use std::ffi::{CStr, c_void};

use crate::library::{Item, Library, Source};
use crate::player::{PlayerError, PlayerEvent, PlayerHandle, RetryBudget, Transport};
use crate::protocol::{ExitReason, ProtocolEmitter, ProtocolEvent, ReturnReason, UriClass};
use crate::resolver::{PlatformInfo, resolve_source};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Browsing,
    Playing { item_id: String },
    Stopping { item_id: String },
    Exiting,
}

#[derive(Debug, Clone)]
pub enum SessionInput {
    SelectItem(String),
    StopPlayback,
    ExitSession,
    /// Raised when shepherdd or the OS sends SIGTERM. Drives the machine to
    /// `Exiting` regardless of current state, after attempting to stop the
    /// player cleanly.
    SignalTerminate,
}

pub struct Session {
    library: Library,
    state: SessionState,
    player: Box<dyn PlayerHandle>,
    protocol: ProtocolEmitter,
    platform: PlatformInfo,
    ready_emitted: bool,
    /// Bounded restart budget for the current item after a transient player
    /// `Error`, refilled when a new item starts.
    retries: RetryBudget,
}

impl Session {
    pub fn new(library: Library, player: Box<dyn PlayerHandle>) -> Self {
        Self::with_emitter(library, player, ProtocolEmitter::stdout())
    }

    pub fn with_emitter(
        library: Library,
        player: Box<dyn PlayerHandle>,
        protocol: ProtocolEmitter,
    ) -> Self {
        Self {
            library,
            state: SessionState::Browsing,
            player,
            protocol,
            platform: PlatformInfo::current(),
            ready_emitted: false,
            retries: RetryBudget::new(),
        }
    }

    /// Borrow the parsed library so the UI can render it.
    pub fn library(&self) -> &Library {
        &self.library
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Returns `true` once the session has reached `Exiting` and the caller
    /// should let the process unwind.
    pub fn is_exiting(&self) -> bool {
        matches!(self.state, SessionState::Exiting)
    }

    /// Emit `READY`. Must be called once after construction; the constructor
    /// can't do this because `protocol` may need to be shared with the UI
    /// layer first in some embeddings.
    pub fn announce_ready(&mut self) {
        if self.ready_emitted {
            return;
        }
        self.ready_emitted = true;
        self.protocol.emit(ProtocolEvent::Ready {
            library_id: self.library.library_id.clone(),
            item_count: self.library.items.len(),
        });
    }

    /// Find an item by id. The UI uses this to look up the user's selection.
    pub fn item_by_id(&self, id: &str) -> Option<&Item> {
        self.library.items.iter().find(|i| i.id == id)
    }

    // -----------------------------------------------------------------
    // Transport delegation. These are thin pass-throughs to the player
    // handle so the playback UI doesn't need to hold a separate borrow
    // alongside the session.
    // -----------------------------------------------------------------

    pub fn set_paused(&mut self, paused: bool) -> Result<(), PlayerError> {
        self.player.set_paused(paused)
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    pub fn seek_relative(&mut self, delta_seconds: f64) -> Result<(), PlayerError> {
        self.player.seek_relative(delta_seconds)
    }

    pub fn seek_absolute(&mut self, seconds: f64) -> Result<(), PlayerError> {
        self.player.seek_absolute(seconds)
    }

    pub fn position(&self) -> Option<f64> {
        self.player.position()
    }

    pub fn duration(&self) -> Option<f64> {
        self.player.duration()
    }

    pub fn set_volume(&mut self, percent: f64) -> Result<(), PlayerError> {
        self.player.set_volume(percent)
    }

    pub fn volume(&self) -> Option<f64> {
        self.player.volume()
    }

    // -----------------------------------------------------------------
    // Embedded render hooks. Delegated to the player; see PlayerHandle
    // docs for ordering requirements.
    // -----------------------------------------------------------------

    pub fn bind_gl(
        &mut self,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        native_display: Option<crate::NativeDisplay>,
    ) -> Result<(), PlayerError> {
        self.player.bind_gl(get_proc_address, native_display)
    }

    pub fn render(&self, fbo: i32, width: i32, height: i32) -> Result<(), PlayerError> {
        self.player.render(fbo, width, height)
    }

    pub fn set_redraw_callback(&mut self, cb: Box<dyn Fn() + Send + Sync + 'static>) {
        self.player.set_redraw_callback(cb);
    }

    pub fn handle_input(&mut self, input: SessionInput) {
        match (self.state.clone(), input) {
            (SessionState::Browsing, SessionInput::SelectItem(id)) => {
                self.try_start(&id);
            }
            (SessionState::Browsing, SessionInput::ExitSession) => {
                self.protocol.emit(ProtocolEvent::Exit {
                    reason: ExitReason::User,
                });
                self.state = SessionState::Exiting;
            }
            (SessionState::Playing { item_id }, SessionInput::StopPlayback) => {
                if let Err(e) = self.player.stop() {
                    self.handle_player_error(&item_id, e);
                } else {
                    self.state = SessionState::Stopping { item_id };
                }
            }
            (_, SessionInput::SignalTerminate) => {
                let _ = self.player.stop();
                self.protocol.emit(ProtocolEvent::Exit {
                    reason: ExitReason::Signal,
                });
                self.state = SessionState::Exiting;
            }
            // Inputs in states that don't accept them are ignored. e.g. the
            // user clicking a tile while a video is already playing — the UI
            // should prevent it, but if a click slips through we drop it.
            (_, _) => {}
        }
    }

    /// Drain any queued player events and apply them to the state machine.
    /// Call regularly (e.g. once per UI frame).
    pub fn tick(&mut self) {
        while let Some(event) = self.player.poll_event() {
            self.apply_player_event(event);
            if self.is_exiting() {
                break;
            }
        }
    }

    fn try_start(&mut self, item_id: &str) {
        let item = match self.library.items.iter().find(|i| i.id == item_id) {
            Some(i) => i,
            None => {
                self.protocol.emit(ProtocolEvent::Warning {
                    item_id: item_id.to_string(),
                    reason: "unknown-item".into(),
                });
                return;
            }
        };

        let source: &Source = match resolve_source(item, &self.platform) {
            Some(s) => s,
            None => {
                self.protocol.emit(ProtocolEvent::Warning {
                    item_id: item_id.to_string(),
                    reason: "no-source".into(),
                });
                return;
            }
        };

        let kind = item.kind;
        let class = UriClass::from_classified(&source.uri);
        match self.player.play(source) {
            Ok(()) => {
                self.protocol.emit(ProtocolEvent::StartedPlayback {
                    item_id: item_id.to_string(),
                    kind,
                    source: class,
                });
                self.retries.reset();
                self.state = SessionState::Playing {
                    item_id: item_id.to_string(),
                };
            }
            Err(e) => {
                self.handle_player_error(item_id, e);
            }
        }
    }

    /// Restart the currently-playing item in place after a transient error,
    /// without re-emitting `STARTED_PLAYBACK` (the state stays `Playing`).
    /// Returns whether the player accepted the restart.
    fn retry_play(&mut self, item_id: &str) -> bool {
        let source: &Source = match self.library.items.iter().find(|i| i.id == item_id) {
            Some(item) => match resolve_source(item, &self.platform) {
                Some(s) => s,
                None => return false,
            },
            None => return false,
        };
        self.player.play(source).is_ok()
    }

    fn handle_player_error(&mut self, item_id: &str, err: PlayerError) {
        let message = err.to_string();
        self.protocol.emit(ProtocolEvent::Error {
            item_id: item_id.to_string(),
            message,
        });
        self.protocol.emit(ProtocolEvent::ReturnedToMenu {
            item_id: item_id.to_string(),
            reason: ReturnReason::Error,
        });
        self.state = SessionState::Browsing;
    }

    fn apply_player_event(&mut self, event: PlayerEvent) {
        match (self.state.clone(), event) {
            (SessionState::Playing { item_id }, PlayerEvent::EndOfFile) => {
                self.protocol.emit(ProtocolEvent::ReturnedToMenu {
                    item_id,
                    reason: ReturnReason::Eof,
                });
                self.state = SessionState::Browsing;
            }
            (SessionState::Playing { item_id }, PlayerEvent::Closed) => {
                self.protocol.emit(ProtocolEvent::ReturnedToMenu {
                    item_id,
                    reason: ReturnReason::Closed,
                });
                self.state = SessionState::Browsing;
            }
            (SessionState::Playing { item_id }, PlayerEvent::Error(message)) => {
                // A transient stream error (e.g. a flaky network connection
                // dropping the stream just after it opens) ends the file with an
                // error; these usually clear on a retry. Restart the same item a
                // bounded number of times before surfacing the error and
                // returning to the menu.
                let recovered = self.retries.try_retry() && {
                    self.protocol.emit(ProtocolEvent::Warning {
                        item_id: item_id.clone(),
                        reason: "playback-retry".into(),
                    });
                    self.retry_play(&item_id)
                };
                if !recovered {
                    self.protocol.emit(ProtocolEvent::Error {
                        item_id: item_id.clone(),
                        message,
                    });
                    self.protocol.emit(ProtocolEvent::ReturnedToMenu {
                        item_id,
                        reason: ReturnReason::Error,
                    });
                    self.state = SessionState::Browsing;
                }
            }
            (SessionState::Stopping { item_id }, PlayerEvent::Closed)
            | (SessionState::Stopping { item_id }, PlayerEvent::EndOfFile) => {
                self.protocol.emit(ProtocolEvent::ReturnedToMenu {
                    item_id,
                    reason: ReturnReason::User,
                });
                self.state = SessionState::Browsing;
            }
            (SessionState::Stopping { item_id }, PlayerEvent::Error(message)) => {
                self.protocol.emit(ProtocolEvent::Error {
                    item_id: item_id.clone(),
                    message,
                });
                self.protocol.emit(ProtocolEvent::ReturnedToMenu {
                    item_id,
                    reason: ReturnReason::User,
                });
                self.state = SessionState::Browsing;
            }
            // Started events are advisory; we already emitted STARTED_PLAYBACK
            // when the user picked the item.
            (_, PlayerEvent::Started) => {}
            // Idle player events while browsing or exiting are ignored.
            (_, _) => {}
        }
    }
}

/// Drive the session's transport from a shared UI overlay. Delegates to the
/// inherent methods above (which forward to the underlying player).
impl Transport for Session {
    fn is_paused(&self) -> bool {
        Session::is_paused(self)
    }
    fn set_paused(&mut self, paused: bool) -> Result<(), PlayerError> {
        Session::set_paused(self, paused)
    }
    fn seek_relative(&mut self, delta_seconds: f64) -> Result<(), PlayerError> {
        Session::seek_relative(self, delta_seconds)
    }
    fn seek_absolute(&mut self, seconds: f64) -> Result<(), PlayerError> {
        Session::seek_absolute(self, seconds)
    }
    fn position(&self) -> Option<f64> {
        Session::position(self)
    }
    fn duration(&self) -> Option<f64> {
        Session::duration(self)
    }
}
