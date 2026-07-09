//! Integration test: drive the session state machine through scripted inputs
//! and player events, then assert the captured protocol stream matches the
//! golden file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shepherd_media_core::{
    PlayerError, PlayerEvent, PlayerHandle, ProtocolEmitter, Session, SessionInput,
    library::{ClassifiedUri, Source},
    load_library,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

#[derive(Default, Clone)]
struct ScriptedPlayer {
    inner: Arc<Mutex<ScriptedInner>>,
}

#[derive(Default)]
struct ScriptedInner {
    pending_events: Vec<PlayerEvent>,
    last_played: Option<ClassifiedUri>,
    playing: bool,
    /// Force the next `play` call to return an error. Test fixture sets this
    /// to verify the error → menu transition.
    fail_next_play: bool,
}

impl ScriptedPlayer {
    fn queue(&self, ev: PlayerEvent) {
        self.inner.lock().unwrap().pending_events.push(ev);
    }
}

impl PlayerHandle for ScriptedPlayer {
    fn play(&mut self, source: &Source) -> Result<(), PlayerError> {
        let mut g = self.inner.lock().unwrap();
        if g.fail_next_play {
            g.fail_next_play = false;
            return Err(PlayerError::Backend("scripted failure".into()));
        }
        g.last_played = Some(source.uri.clone());
        g.playing = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        let mut g = self.inner.lock().unwrap();
        g.playing = false;
        Ok(())
    }

    fn is_playing(&self) -> bool {
        self.inner.lock().unwrap().playing
    }

    fn poll_event(&mut self) -> Option<PlayerEvent> {
        let mut g = self.inner.lock().unwrap();
        if g.pending_events.is_empty() {
            None
        } else {
            Some(g.pending_events.remove(0))
        }
    }
}

fn capture(session_steps: impl FnOnce(&mut Session, &ScriptedPlayer)) -> String {
    let library = load_library(&fixtures_dir().join("valid-mixed.toml")).unwrap();
    let player = ScriptedPlayer::default();
    let buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let emitter = ProtocolEmitter::new(Sink(buffer.clone()));
    let mut session = Session::with_emitter(library, Box::new(player.clone()), emitter);

    session.announce_ready();
    session_steps(&mut session, &player);

    let bytes = buffer.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn play_then_eof_returns_to_menu() {
    let captured = capture(|session, player| {
        session.handle_input(SessionInput::SelectItem("local-video".into()));
        player.queue(PlayerEvent::EndOfFile);
        session.tick();
        session.handle_input(SessionInput::ExitSession);
    });

    let golden = "\
READY library_id=valid-mixed item_count=5
STARTED_PLAYBACK item=local-video kind=video source=local
RETURNED_TO_MENU item=local-video reason=eof
EXIT reason=user
";
    assert_eq!(captured, golden);
}

#[test]
fn user_stop_emits_user_reason() {
    let captured = capture(|session, player| {
        session.handle_input(SessionInput::SelectItem("youtube-video".into()));
        session.handle_input(SessionInput::StopPlayback);
        player.queue(PlayerEvent::Closed);
        session.tick();
        session.handle_input(SessionInput::ExitSession);
    });

    let golden = "\
READY library_id=valid-mixed item_count=5
STARTED_PLAYBACK item=youtube-video kind=video source=youtube
RETURNED_TO_MENU item=youtube-video reason=user
EXIT reason=user
";
    assert_eq!(captured, golden);
}

#[test]
fn transient_player_error_retries_and_recovers() {
    // A single error during playback is treated as transient: the item is
    // restarted (WARNING playback-retry) and playback continues, so a later EOF
    // returns to the menu normally rather than as an error.
    let captured = capture(|session, player| {
        session.handle_input(SessionInput::SelectItem("direct-http".into()));
        player.queue(PlayerEvent::Error("flaky connection".into()));
        session.tick();
        player.queue(PlayerEvent::EndOfFile);
        session.tick();
        session.handle_input(SessionInput::ExitSession);
    });

    let golden = "\
READY library_id=valid-mixed item_count=5
STARTED_PLAYBACK item=direct-http kind=video source=direct-http
WARNING item=direct-http reason=playback-retry
RETURNED_TO_MENU item=direct-http reason=eof
EXIT reason=user
";
    assert_eq!(captured, golden);
}

#[test]
fn player_error_returns_to_menu_after_retries_exhausted() {
    // Errors that keep recurring exhaust the retry budget (MAX_PLAY_RETRIES = 2,
    // so two WARNING retries) and then surface as an ERROR + return to menu.
    let captured = capture(|session, player| {
        session.handle_input(SessionInput::SelectItem("direct-http".into()));
        player.queue(PlayerEvent::Error("backend exploded".into()));
        player.queue(PlayerEvent::Error("backend exploded".into()));
        player.queue(PlayerEvent::Error("backend exploded".into()));
        session.tick();
        session.handle_input(SessionInput::ExitSession);
    });

    let golden = "\
READY library_id=valid-mixed item_count=5
STARTED_PLAYBACK item=direct-http kind=video source=direct-http
WARNING item=direct-http reason=playback-retry
WARNING item=direct-http reason=playback-retry
ERROR item=direct-http message=backend%20exploded
RETURNED_TO_MENU item=direct-http reason=error
EXIT reason=user
";
    assert_eq!(captured, golden);
}

#[test]
fn signal_terminate_emits_signal_exit() {
    let captured = capture(|session, _player| {
        session.handle_input(SessionInput::SelectItem("local-video".into()));
        session.handle_input(SessionInput::SignalTerminate);
    });

    let golden = "\
READY library_id=valid-mixed item_count=5
STARTED_PLAYBACK item=local-video kind=video source=local
EXIT reason=signal
";
    assert_eq!(captured, golden);
}

#[test]
fn unresolved_item_emits_warning_and_stays_browsing() {
    // Item id that doesn't exist
    let captured = capture(|session, _player| {
        session.handle_input(SessionInput::SelectItem("does-not-exist".into()));
        session.handle_input(SessionInput::ExitSession);
    });

    let golden = "\
READY library_id=valid-mixed item_count=5
WARNING item=does-not-exist reason=unknown-item
EXIT reason=user
";
    assert_eq!(captured, golden);
}

#[test]
fn play_failure_in_backend_yields_error_and_back_to_menu() {
    let captured = capture(|session, player| {
        player.inner.lock().unwrap().fail_next_play = true;
        session.handle_input(SessionInput::SelectItem("local-video".into()));
        session.handle_input(SessionInput::ExitSession);
    });

    let golden = "\
READY library_id=valid-mixed item_count=5
ERROR item=local-video message=player%20backend%20error:%20scripted%20failure
RETURNED_TO_MENU item=local-video reason=error
EXIT reason=user
";
    assert_eq!(captured, golden);
}
