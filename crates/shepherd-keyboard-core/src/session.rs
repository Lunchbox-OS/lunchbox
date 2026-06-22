//! The keyboard session: the backend-agnostic brain.
//!
//! A backend feeds this struct focus/content-type changes, surrounding text, and classified
//! input ([`Stroke`]s, function keys, suggestion taps); it returns the [`HostAction`]s the
//! backend must perform against the compositor (commit text, set a preedit, emit a keysym,
//! update the suggestion bar). Putting the whole policy here is what guarantees the wlroots
//! and GNOME backends behave identically.
//!
//! All safety gating happens here: in a sensitive field swipe and suggestions are off and
//! the surrounding text is never used.

use shepherd_swipe_core::{Context, Decoder, Layout};

use crate::gesture::Stroke;
use crate::policy::CandidatePolicy;
use crate::safety::{ContentType, SafetyGate, SafetyMode};

/// The rendering-only fallback geometry for tap-only degraded mode (see the TOML header).
const FALLBACK_LAYOUT_TOML: &str = include_str!("fallback-layout.toml");

/// Parse the built-in fallback QWERTY layout. Used when no signed bundle could be loaded so
/// a child can still tap letters (no predictions). Panics only if the vendored TOML is
/// malformed, which a unit test guards against.
pub fn fallback_layout() -> Layout {
    Layout::from_toml(FALLBACK_LAYOUT_TOML).expect("vendored fallback layout parses")
}

/// A non-text key the backend emits via the virtual keyboard (wlroots) or the IM channel
/// (GNOME). The core expresses intent; the backend chooses the mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySym {
    /// Return / Enter.
    Enter,
    /// Backspace (delete one grapheme before the cursor).
    Backspace,
    /// Tab.
    Tab,
}

/// An action the backend must perform against the focused field / its UI.
#[derive(Debug, Clone, PartialEq)]
pub enum HostAction {
    /// Set (or, with an empty string, clear) the transient preedit; cursor goes to the end.
    SetPreedit(String),
    /// Commit text into the field. This also finalizes/clears any preedit.
    CommitText(String),
    /// Emit a non-text key. Used for Enter and for Backspace when there is no preedit, so it
    /// works correctly (and leaks nothing) even in sensitive fields.
    KeyInput(KeySym),
    /// Replace the suggestion bar contents (empty hides it).
    SetSuggestions(Vec<String>),
}

/// A function key tapped outside the letter grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKey {
    /// Space; finalizes any pending preedit, then inserts a space.
    Space,
    /// Backspace.
    Backspace,
    /// Enter.
    Enter,
    /// Shift (cycles off → one-shot → locked → off).
    Shift,
    /// Toggle the symbols layer.
    Symbols,
}

/// Shift state, cycled by the Shift key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShiftState {
    /// Lowercase.
    #[default]
    Off,
    /// Uppercase for the next character only.
    OneShot,
    /// Caps lock.
    Locked,
}

impl ShiftState {
    fn next(self) -> Self {
        match self {
            ShiftState::Off => ShiftState::OneShot,
            ShiftState::OneShot => ShiftState::Locked,
            ShiftState::Locked => ShiftState::Off,
        }
    }

    fn applies(self) -> bool {
        matches!(self, ShiftState::OneShot | ShiftState::Locked)
    }
}

/// The keyboard session.
pub struct Keyboard {
    decoder: Option<Decoder>,
    layout: Layout,
    gate: SafetyGate,
    policy: CandidatePolicy,
    shift: ShiftState,
    symbols: bool,
    preedit: String,
    suggestions: Vec<String>,
    preceding_text: String,
}

impl Keyboard {
    /// A full session backed by a verified decoder (swipe + predictions enabled).
    pub fn new(decoder: Decoder) -> Self {
        let layout = decoder.layout().clone();
        Self::build(Some(decoder), layout)
    }

    /// A degraded, tap-only session with no decoder (fail-closed): letters can be tapped but
    /// nothing is predicted. `layout` is rendering geometry only — use [`fallback_layout`].
    pub fn tap_only(layout: Layout) -> Self {
        Self::build(None, layout)
    }

    fn build(decoder: Option<Decoder>, layout: Layout) -> Self {
        Self {
            decoder,
            layout,
            gate: SafetyGate::default(),
            policy: CandidatePolicy::default(),
            shift: ShiftState::default(),
            symbols: false,
            preedit: String::new(),
            suggestions: Vec::new(),
            preceding_text: String::new(),
        }
    }

    /// Override the candidate/commit policy.
    pub fn set_policy(&mut self, policy: CandidatePolicy) {
        self.policy = policy;
    }

    /// The layout to render and hit-test against.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The current safety mode.
    pub fn mode(&self) -> SafetyMode {
        self.gate.mode()
    }

    /// Whether swipe decoding is currently enabled (false when degraded or in a sensitive
    /// field).
    pub fn swipe_enabled(&self) -> bool {
        self.decoder.is_some() && self.gate.swipe_allowed()
    }

    /// The current shift state (for rendering).
    pub fn shift_state(&self) -> ShiftState {
        self.shift
    }

    /// Whether the symbols layer is active (for rendering).
    pub fn symbols_active(&self) -> bool {
        self.symbols
    }

    /// The current preedit string.
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// The current suggestion list.
    pub fn suggestions(&self) -> &[String] {
        &self.suggestions
    }

    /// Update the focused field's content type (focus change or field change). Switching into
    /// a sensitive field drops any preedit (without committing it) and clears suggestions.
    pub fn set_content_type(&mut self, content_type: ContentType) -> Vec<HostAction> {
        self.gate.set_content_type(content_type);
        let mut actions = Vec::new();
        if self.gate.mode() == SafetyMode::TapOnly {
            if !self.preedit.is_empty() {
                self.preedit.clear();
                actions.push(HostAction::SetPreedit(String::new()));
            }
            self.preceding_text.clear();
            self.clear_suggestions(&mut actions);
        }
        actions
    }

    /// Provide the text immediately preceding the cursor (for context-LM re-ranking). Ignored
    /// and not stored in a sensitive field.
    pub fn set_surrounding_text(&mut self, preceding_text: &str) {
        if self.gate.may_use_surrounding_text() {
            self.preceding_text = preceding_text.to_string();
        } else {
            self.preceding_text.clear();
        }
    }

    /// Handle a classified interaction inside the letter grid: a tap (→ nearest key) or a
    /// swipe (→ decode).
    pub fn on_stroke(&mut self, stroke: Stroke) -> Vec<HostAction> {
        match stroke {
            Stroke::Tap { x, y } => match self.layout.nearest_key(x, y) {
                Some(ch) => self.type_char(ch),
                None => Vec::new(),
            },
            Stroke::Swipe(gesture) => {
                // Swipe is disabled when degraded or in a sensitive field; ignore it.
                let Some(decoder) = self.decoder.as_ref() else {
                    return Vec::new();
                };
                if !self.gate.swipe_allowed() {
                    return Vec::new();
                }
                let preceding = if self.gate.may_use_surrounding_text() {
                    self.preceding_text.as_str()
                } else {
                    ""
                };
                let candidates = decoder.decode(
                    &gesture,
                    &self.layout,
                    Context {
                        preceding_text: preceding,
                    },
                );
                let decision = self.policy.decide(&candidates);

                let mut actions = Vec::new();
                // A new swipe finalizes the previous word first.
                self.finalize_preedit(&mut actions);
                if let Some(preedit) = decision.preedit {
                    self.preedit = preedit.clone();
                    actions.push(HostAction::SetPreedit(preedit));
                }
                self.set_suggestions(decision.suggestions, &mut actions);
                actions
            }
        }
    }

    /// Handle a function-key tap.
    pub fn on_function_key(&mut self, key: FunctionKey) -> Vec<HostAction> {
        let mut actions = Vec::new();
        match key {
            FunctionKey::Space => {
                self.finalize_preedit(&mut actions);
                actions.push(HostAction::CommitText(" ".to_string()));
                self.clear_suggestions(&mut actions);
            }
            FunctionKey::Enter => {
                self.finalize_preedit(&mut actions);
                actions.push(HostAction::KeyInput(KeySym::Enter));
                self.clear_suggestions(&mut actions);
            }
            FunctionKey::Backspace => {
                if self.preedit.pop().is_some() {
                    // Editing the preedit in place; nothing committed yet.
                    actions.push(HostAction::SetPreedit(self.preedit.clone()));
                } else {
                    // No preedit: a real Backspace keysym works in every field, sensitive
                    // included, with no surrounding-text byte counting.
                    actions.push(HostAction::KeyInput(KeySym::Backspace));
                }
            }
            FunctionKey::Shift => {
                self.shift = self.shift.next();
            }
            FunctionKey::Symbols => {
                self.symbols = !self.symbols;
            }
        }
        actions
    }

    /// Commit the suggestion at `index`, replacing the current preedit. Out-of-range or a
    /// sensitive field (no suggestions) is a no-op.
    pub fn select_suggestion(&mut self, index: usize) -> Vec<HostAction> {
        let Some(word) = self.suggestions.get(index).cloned() else {
            return Vec::new();
        };
        // The preedit is being replaced, not finalized: clear it so we don't double-commit.
        self.preedit.clear();
        let mut actions = vec![HostAction::CommitText(word)];
        self.clear_suggestions(&mut actions);
        actions
    }

    /// Type a literal character (tap), applying shift. Finalizes any pending preedit first.
    fn type_char(&mut self, ch: char) -> Vec<HostAction> {
        let mut actions = Vec::new();
        self.finalize_preedit(&mut actions);
        let ch = if self.shift.applies() {
            ch.to_ascii_uppercase()
        } else {
            ch
        };
        if self.shift == ShiftState::OneShot {
            self.shift = ShiftState::Off;
        }
        actions.push(HostAction::CommitText(ch.to_string()));
        self.clear_suggestions(&mut actions);
        actions
    }

    /// Commit any pending preedit as final text and clear it.
    fn finalize_preedit(&mut self, actions: &mut Vec<HostAction>) {
        if !self.preedit.is_empty() {
            let word = std::mem::take(&mut self.preedit);
            actions.push(HostAction::CommitText(word));
        }
    }

    /// Replace the suggestion list, emitting an update only when it changes.
    fn set_suggestions(&mut self, suggestions: Vec<String>, actions: &mut Vec<HostAction>) {
        if self.suggestions != suggestions {
            self.suggestions = suggestions.clone();
            actions.push(HostAction::SetSuggestions(suggestions));
        }
    }

    fn clear_suggestions(&mut self, actions: &mut Vec<HostAction>) {
        if !self.suggestions.is_empty() {
            self.suggestions.clear();
            actions.push(HostAction::SetSuggestions(Vec::new()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::InputPurpose;

    fn tap_only_kbd() -> Keyboard {
        Keyboard::tap_only(fallback_layout())
    }

    #[test]
    fn fallback_layout_parses_and_has_letters() {
        let layout = fallback_layout();
        assert_eq!(layout.layout_id, "qwerty-en-v1");
        assert_eq!(layout.num_keys(), 28);
        assert_eq!(layout.nearest_key(0.10047, 0.5), Some('a'));
    }

    #[test]
    fn tap_types_nearest_key() {
        let mut kbd = tap_only_kbd();
        // 'a' center.
        let actions = kbd.on_stroke(Stroke::Tap { x: 0.10047, y: 0.5 });
        assert_eq!(actions, vec![HostAction::CommitText("a".to_string())]);
    }

    #[test]
    fn shift_one_shot_uppercases_then_resets() {
        let mut kbd = tap_only_kbd();
        assert!(kbd.on_function_key(FunctionKey::Shift).is_empty());
        assert_eq!(kbd.shift_state(), ShiftState::OneShot);
        let a = kbd.on_stroke(Stroke::Tap { x: 0.10047, y: 0.5 });
        assert_eq!(a, vec![HostAction::CommitText("A".to_string())]);
        assert_eq!(kbd.shift_state(), ShiftState::Off);
        let a = kbd.on_stroke(Stroke::Tap { x: 0.10047, y: 0.5 });
        assert_eq!(a, vec![HostAction::CommitText("a".to_string())]);
    }

    #[test]
    fn shift_locked_stays_until_cycled_off() {
        let mut kbd = tap_only_kbd();
        kbd.on_function_key(FunctionKey::Shift); // OneShot
        kbd.on_function_key(FunctionKey::Shift); // Locked
        assert_eq!(kbd.shift_state(), ShiftState::Locked);
        for _ in 0..2 {
            let a = kbd.on_stroke(Stroke::Tap { x: 0.10047, y: 0.5 });
            assert_eq!(a, vec![HostAction::CommitText("A".to_string())]);
        }
        kbd.on_function_key(FunctionKey::Shift); // Off
        assert_eq!(kbd.shift_state(), ShiftState::Off);
    }

    #[test]
    fn space_inserts_a_space() {
        let mut kbd = tap_only_kbd();
        assert_eq!(
            kbd.on_function_key(FunctionKey::Space),
            vec![HostAction::CommitText(" ".to_string())]
        );
    }

    #[test]
    fn backspace_without_preedit_emits_keysym() {
        let mut kbd = tap_only_kbd();
        assert_eq!(
            kbd.on_function_key(FunctionKey::Backspace),
            vec![HostAction::KeyInput(KeySym::Backspace)]
        );
    }

    #[test]
    fn enter_emits_keysym() {
        let mut kbd = tap_only_kbd();
        assert_eq!(
            kbd.on_function_key(FunctionKey::Enter),
            vec![HostAction::KeyInput(KeySym::Enter)]
        );
    }

    #[test]
    fn swipe_is_ignored_in_degraded_mode() {
        let mut kbd = tap_only_kbd();
        // Build any swipe via the gesture assembler.
        use crate::gesture::{GestureBuilder, KeyArea, RawPoint};
        let mut b = GestureBuilder::new(KeyArea {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        for i in 0..=5 {
            b.push(RawPoint {
                x: i as f32 * 0.2,
                y: 0.5,
                t_ms: i as u32 * 10,
            });
        }
        let stroke = b.finish().unwrap();
        assert!(matches!(stroke, Stroke::Swipe(_)));
        assert!(
            kbd.on_stroke(stroke).is_empty(),
            "no decoder => swipe no-op"
        );
    }

    #[test]
    fn sensitive_field_drops_preedit_and_disables_swipe() {
        // Simulate a session that has a preedit by hand-driving the state via tap_only? We
        // need a decoder to make a preedit, which tests can't load here, so verify the gate
        // surface directly: switching to a password field clears suggestions/preedit and
        // forbids swipe + surrounding text.
        let mut kbd = tap_only_kbd();
        kbd.set_surrounding_text("dear ");
        // tap_only has no decoder, so swipe already disabled; assert the gate too.
        let actions = kbd.set_content_type(ContentType::new(InputPurpose::Password));
        assert_eq!(kbd.mode(), SafetyMode::TapOnly);
        assert!(!kbd.swipe_enabled());
        // No preedit/suggestions existed, so no UI-reset actions are emitted.
        assert!(actions.is_empty());
        // Surrounding text must not be retained for a sensitive field.
        kbd.set_surrounding_text("secret-prefix ");
        // (No public getter for preceding text by design; covered by the decode-path test.)
    }
}
