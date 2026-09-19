//! Placeholder playback backend.
//!
//! The Android player will be a libmpv-backed [`PlayerHandle`] (built/bundled
//! as `libmpv.so` and rendered into the eframe GL surface, mirroring the Linux
//! binary). Until that lands, [`StubPlayer`] lets the rest of the app —
//! navigation, session wiring, settings — be exercised without a real decoder:
//! it accepts `play`/`stop` and reports an immediate end-of-file.

use lunchbox_media_core::{PlayerError, PlayerEvent, PlayerHandle, Source};

#[derive(Default)]
pub struct StubPlayer {
    playing: bool,
    pending: Option<PlayerEvent>,
}

impl PlayerHandle for StubPlayer {
    fn play(&mut self, _source: &Source) -> Result<(), PlayerError> {
        self.playing = true;
        // Emit Started now, then end-of-file on the next poll so the session
        // state machine transitions just as it would with a real backend.
        self.pending = Some(PlayerEvent::Started);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.playing = false;
        self.pending = Some(PlayerEvent::Closed);
        Ok(())
    }

    fn is_playing(&self) -> bool {
        self.playing
    }

    fn poll_event(&mut self) -> Option<PlayerEvent> {
        match self.pending.take() {
            Some(PlayerEvent::Started) => {
                // Next poll ends playback.
                self.pending = Some(PlayerEvent::EndOfFile);
                Some(PlayerEvent::Started)
            }
            Some(other) => {
                self.playing = false;
                Some(other)
            }
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_media_core::{ClassifiedUri, Platform};
    use url::Url;

    fn dummy_source() -> Source {
        Source {
            platforms: vec![Platform::Any],
            uri: ClassifiedUri::DirectHttp(Url::parse("https://example.com/a.mp4").unwrap()),
            player_hint: None,
        }
    }

    #[test]
    fn play_then_poll_runs_to_eof() {
        let mut p = StubPlayer::default();
        p.play(&dummy_source()).unwrap();
        assert!(p.is_playing());
        assert!(matches!(p.poll_event(), Some(PlayerEvent::Started)));
        assert!(matches!(p.poll_event(), Some(PlayerEvent::EndOfFile)));
        assert!(!p.is_playing());
        assert!(p.poll_event().is_none());
    }

    #[test]
    fn stop_emits_closed() {
        let mut p = StubPlayer::default();
        p.play(&dummy_source()).unwrap();
        p.stop().unwrap();
        assert!(!p.is_playing());
        assert!(matches!(p.poll_event(), Some(PlayerEvent::Closed)));
    }
}
