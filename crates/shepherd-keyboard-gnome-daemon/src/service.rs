//! The D-Bus decode service.
//!
//! GNOME exposes neither input-method-v2 nor virtual-keyboard, so the GNOME backend is a
//! GJS Shell extension (UI + commit through GNOME's own input-method object) plus this
//! headless Rust daemon, which owns the decoder. The extension sends a captured gesture
//! over D-Bus and gets ranked candidates back — reusing the **exact same**
//! `shepherd-keyboard-core` decode path as the wlroots backend, so candidates match.

use std::sync::Mutex;

use shepherd_keyboard_core::{Decoder, decode_gesture_json};

/// Holds the loaded decoder (or `None`, fail-closed) behind a lock so the service is
/// `Send + Sync` for the D-Bus object server.
pub struct SwipeDecoder {
    decoder: Mutex<Option<Decoder>>,
    profile: String,
}

impl SwipeDecoder {
    /// Build the service. `decoder` is `None` when no bundle could be verified (the daemon
    /// still runs and simply returns no candidates — the extension falls back to tap entry).
    pub fn new(decoder: Option<Decoder>, profile: String) -> Self {
        Self {
            decoder: Mutex::new(decoder),
            profile,
        }
    }

    /// Decode a v1 `gesture.json` to ranked `(word, score)` pairs (best first). Returns empty
    /// on a malformed gesture or when no decoder is loaded — never panics.
    pub fn decode_words(&self, gesture_json: &str, preceding_text: &str) -> Vec<(String, f64)> {
        let guard = self.decoder.lock().expect("decoder mutex poisoned");
        let Some(decoder) = guard.as_ref() else {
            return Vec::new();
        };
        match decode_gesture_json(decoder, gesture_json, preceding_text) {
            Ok(candidates) => candidates
                .into_iter()
                .map(|c| (c.word, c.score as f64))
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "gesture decode failed");
                Vec::new()
            }
        }
    }

    /// Whether a verified decoder is loaded.
    pub fn has_decoder(&self) -> bool {
        self.decoder
            .lock()
            .expect("decoder mutex poisoned")
            .is_some()
    }
}

/// The D-Bus interface. Mirrors the wlroots backend's decode path; it does **not** carry
/// editing or commit logic (the GJS extension commits via GNOME's input-method object). The
/// caller must pass `""` for `preceding_text` in sensitive fields — surrounding text must not
/// be read there.
#[zbus::interface(name = "com.armeafamily.ShepherdSwipe1")]
impl SwipeDecoder {
    /// Decode a gesture into ranked candidates, best first.
    async fn decode(&self, gesture_json: String, preceding_text: String) -> Vec<(String, f64)> {
        self.decode_words(&gesture_json, &preceding_text)
    }

    /// Whether predictions are available (false ⇒ tap-only, no decoder loaded).
    #[zbus(property)]
    async fn available(&self) -> bool {
        self.has_decoder()
    }

    /// The loaded profile id (`adult` / `child`).
    #[zbus(property)]
    async fn profile(&self) -> String {
        self.profile.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_decoder_returns_empty_and_unavailable() {
        let svc = SwipeDecoder::new(None, "adult".to_string());
        assert!(!svc.has_decoder());
        assert!(svc.decode_words("{}", "").is_empty());
    }
}
