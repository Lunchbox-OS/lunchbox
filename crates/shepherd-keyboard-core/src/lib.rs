//! # shepherd-keyboard-core
//!
//! Backend-agnostic host logic for the swipe keyboard. Renders nothing, talks to no
//! compositor: it loads a signed bundle through the external host-agnostic decoder
//! ([`shepherd_swipe_core`]), assembles captured touch into normalized gestures, decodes
//! them into ranked candidates, and owns the candidate/commit policy, editing-key
//! semantics, and the safety-gate state machine. Both the wlroots backend and the GNOME
//! decoder daemon are thin adapters over this crate, so identical gestures yield identical
//! candidates everywhere.
//!
//! Everything here is fail-closed: a missing, unverified, or incompatible bundle yields an
//! [`Error`], and the caller degrades to tap-only entry with no predictions rather than
//! ever using an unverified decoder.

#![forbid(unsafe_code)]

pub mod bundle;
pub mod error;
pub mod gesture;
pub mod policy;
pub mod profile;
pub mod safety;
pub mod session;

pub use error::{Error, Result};
pub use gesture::{GestureBuilder, KeyArea, RawPoint, Stroke};
pub use policy::{CandidatePolicy, Decision};
pub use profile::Profile;
pub use safety::{ContentType, InputPurpose, SafetyGate, SafetyMode};
pub use session::{FunctionKey, HostAction, KeySym, Keyboard, ShiftState, fallback_layout};

// Re-export the decoder contract types host backends need, so a backend depends only on
// this crate (keeping the keyboard extractable — see the crate README).
pub use shepherd_swipe_core::{Candidate, Context, Decoder, Gesture, Layout, TouchPoint};

/// Decode a gesture supplied in the v1 `*.gesture.json` interchange format against a
/// loaded decoder, returning ranked candidates (best first).
///
/// This is the seam both backends and the GNOME daemon share: a gesture is captured,
/// normalized, serialized to the interchange format, and decoded here, guaranteeing
/// cross-backend parity. `preceding_text` conditions the context LM (pass `""` when none,
/// and **always** `""` in a sensitive/password field — see the safety gates).
pub fn decode_gesture_json(
    decoder: &Decoder,
    gesture_json: &str,
    preceding_text: &str,
) -> Result<Vec<Candidate>> {
    let (gesture, _record) = Gesture::from_json(gesture_json)?;
    let layout = decoder.layout().clone();
    Ok(decoder.decode(&gesture, &layout, Context { preceding_text }))
}
