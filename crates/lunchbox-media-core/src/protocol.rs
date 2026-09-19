//! Stdout line protocol: one event per line, key=value pairs, percent-encoded
//! values where needed.
//!
//! Stderr is for human-readable logging and is not part of the protocol.

use std::io::{self, Write};

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::library::{ClassifiedUri, ItemKind};

/// The set of characters that must be percent-encoded inside a protocol value.
/// We escape spaces, `=`, `\n`, and `\r` so the line is unambiguously
/// `read`-line-able and tokenizable on whitespace.
const VALUE_ENCODE: &AsciiSet = &CONTROLS.add(b' ').add(b'=').add(b'\n').add(b'\r').add(b'%');

/// One event in the protocol stream.
///
/// `Display` formats the event as a single line without the trailing newline.
#[derive(Debug, Clone)]
pub enum ProtocolEvent {
    Ready {
        library_id: String,
        item_count: usize,
    },
    StartedPlayback {
        item_id: String,
        kind: ItemKind,
        source: UriClass,
    },
    ReturnedToMenu {
        item_id: String,
        reason: ReturnReason,
    },
    Warning {
        item_id: String,
        reason: String,
    },
    Error {
        item_id: String,
        message: String,
    },
    Exit {
        reason: ExitReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriClass {
    Local,
    DirectHttp,
    YouTube,
    Unknown,
}

impl UriClass {
    pub fn from_classified(c: &ClassifiedUri) -> Self {
        match c {
            ClassifiedUri::Local(_) => Self::Local,
            ClassifiedUri::DirectHttp(_) => Self::DirectHttp,
            ClassifiedUri::YouTube(_) => Self::YouTube,
            ClassifiedUri::Unknown(_) => Self::Unknown,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::DirectHttp => "direct-http",
            Self::YouTube => "youtube",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnReason {
    Eof,
    Closed,
    User,
    Error,
}

impl ReturnReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::Closed => "closed",
            Self::User => "user",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    User,
    Signal,
    Crash,
}

impl ExitReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Signal => "signal",
            Self::Crash => "crash",
        }
    }
}

fn kind_str(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Video => "video",
        ItemKind::Audio => "audio",
    }
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, VALUE_ENCODE).to_string()
}

impl std::fmt::Display for ProtocolEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolEvent::Ready {
                library_id,
                item_count,
            } => write!(
                f,
                "READY library_id={} item_count={}",
                encode(library_id),
                item_count
            ),
            ProtocolEvent::StartedPlayback {
                item_id,
                kind,
                source,
            } => write!(
                f,
                "STARTED_PLAYBACK item={} kind={} source={}",
                encode(item_id),
                kind_str(*kind),
                source.as_str()
            ),
            ProtocolEvent::ReturnedToMenu { item_id, reason } => write!(
                f,
                "RETURNED_TO_MENU item={} reason={}",
                encode(item_id),
                reason.as_str()
            ),
            ProtocolEvent::Warning { item_id, reason } => write!(
                f,
                "WARNING item={} reason={}",
                encode(item_id),
                encode(reason)
            ),
            ProtocolEvent::Error { item_id, message } => write!(
                f,
                "ERROR item={} message={}",
                encode(item_id),
                encode(message)
            ),
            ProtocolEvent::Exit { reason } => write!(f, "EXIT reason={}", reason.as_str()),
        }
    }
}

/// Writes events as protocol lines to a sink.
///
/// In the real binary the sink is `io::stdout()`; tests pipe a `Vec<u8>` in.
pub struct ProtocolEmitter {
    sink: Box<dyn Write + Send>,
    enabled: bool,
}

impl ProtocolEmitter {
    pub fn new<W: Write + Send + 'static>(sink: W) -> Self {
        Self {
            sink: Box::new(sink),
            enabled: true,
        }
    }

    pub fn stdout() -> Self {
        Self::new(io::stdout())
    }

    /// Build an emitter that swallows all events. Use when running with
    /// `--no-protocol`.
    pub fn disabled() -> Self {
        Self {
            sink: Box::new(io::sink()),
            enabled: false,
        }
    }

    pub fn emit(&mut self, event: ProtocolEvent) {
        if !self.enabled {
            return;
        }
        if let Err(e) = writeln!(self.sink, "{event}") {
            tracing::warn!("failed to emit protocol event: {e}");
        } else {
            let _ = self.sink.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_playback_format() {
        let ev = ProtocolEvent::StartedPlayback {
            item_id: "big-buck-bunny".into(),
            kind: ItemKind::Video,
            source: UriClass::Local,
        };
        assert_eq!(
            ev.to_string(),
            "STARTED_PLAYBACK item=big-buck-bunny kind=video source=local"
        );
    }

    #[test]
    fn error_message_is_encoded() {
        let ev = ProtocolEvent::Error {
            item_id: "x".into(),
            message: "lost connection: bad gateway".into(),
        };
        assert_eq!(
            ev.to_string(),
            "ERROR item=x message=lost%20connection:%20bad%20gateway"
        );
    }

    #[test]
    fn ready_includes_count() {
        let ev = ProtocolEvent::Ready {
            library_id: "kids".into(),
            item_count: 7,
        };
        assert_eq!(ev.to_string(), "READY library_id=kids item_count=7");
    }
}
