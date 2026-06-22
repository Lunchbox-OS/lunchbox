//! Safety-gate state machine: focused-field content type → keyboard mode.
//!
//! The non-negotiable invariant (host spec §4.4): a password / PIN / sensitive field is
//! **plain tap only** — swipe disabled, suggestions disabled, and the surrounding text is
//! neither read nor used. Both backends feed their protocol's content-type into this gate
//! and obey the resulting capabilities; the decision lives here so the two backends cannot
//! drift.

/// The purpose of the focused text field. Mirrors the wlroots
/// `zwp_text_input_v3`/`input-method-v2` `content_purpose` enum (GNOME's input purpose maps
/// onto the same names), so a backend can translate its protocol value 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputPurpose {
    /// Default, allow any input.
    #[default]
    Normal,
    /// Single line of (almost) any text.
    Alpha,
    /// Digits only.
    Digits,
    /// Any number.
    Number,
    /// Phone number.
    Phone,
    /// URL.
    Url,
    /// Email address.
    Email,
    /// Name of a person.
    Name,
    /// Password (hidden, sensitive).
    Password,
    /// PIN (sensitive, digits).
    Pin,
    /// Date / time / datetime.
    Date,
    /// Time.
    Time,
    /// Date and time.
    Datetime,
    /// Terminal.
    Terminal,
}

impl InputPurpose {
    /// Whether this purpose is sensitive enough to force plain tap-only entry.
    fn is_sensitive(self) -> bool {
        matches!(self, InputPurpose::Password | InputPurpose::Pin)
    }
}

/// The interaction mode the keyboard runs in for the focused field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyMode {
    /// Full functionality: swipe, suggestions, and surrounding-text–conditioned decoding.
    Full,
    /// Plain tap only: no swipe, no suggestions, no surrounding-text reads. Used for
    /// password / PIN / sensitive fields.
    TapOnly,
}

/// The focused field's content type: its purpose plus whether the host flagged the data as
/// sensitive (wlroots `content_hint` `sensitive_data` bit / GNOME equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentType {
    /// The field's input purpose.
    pub purpose: InputPurpose,
    /// The host's "this is sensitive data" hint, independent of purpose.
    pub sensitive_hint: bool,
}

impl ContentType {
    /// Construct a content type from a purpose with no sensitive hint.
    pub fn new(purpose: InputPurpose) -> Self {
        Self {
            purpose,
            sensitive_hint: false,
        }
    }

    /// The safety mode this content type demands.
    pub fn mode(self) -> SafetyMode {
        if self.purpose.is_sensitive() || self.sensitive_hint {
            SafetyMode::TapOnly
        } else {
            SafetyMode::Full
        }
    }
}

/// The safety gate: tracks the focused field's content type and answers what the keyboard
/// is allowed to do. Defaults to `Normal`/`Full` before any field is focused.
#[derive(Debug, Clone, Copy, Default)]
pub struct SafetyGate {
    content_type: ContentType,
}

impl SafetyGate {
    /// A gate for the given content type.
    pub fn new(content_type: ContentType) -> Self {
        Self { content_type }
    }

    /// Update the gate when focus or the field's content type changes.
    pub fn set_content_type(&mut self, content_type: ContentType) {
        self.content_type = content_type;
    }

    /// The current content type.
    pub fn content_type(&self) -> ContentType {
        self.content_type
    }

    /// The current interaction mode.
    pub fn mode(&self) -> SafetyMode {
        self.content_type.mode()
    }

    /// Whether swipe gestures may be decoded right now.
    pub fn swipe_allowed(&self) -> bool {
        self.mode() == SafetyMode::Full
    }

    /// Whether word suggestions may be shown right now.
    pub fn suggestions_allowed(&self) -> bool {
        self.mode() == SafetyMode::Full
    }

    /// Whether the surrounding text may be read and used to condition decoding. **Must be
    /// false in a sensitive field** — the host must not even read it.
    pub fn may_use_surrounding_text(&self) -> bool {
        self.mode() == SafetyMode::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_field_is_full_functionality() {
        let gate = SafetyGate::new(ContentType::new(InputPurpose::Normal));
        assert_eq!(gate.mode(), SafetyMode::Full);
        assert!(gate.swipe_allowed());
        assert!(gate.suggestions_allowed());
        assert!(gate.may_use_surrounding_text());
    }

    #[test]
    fn password_and_pin_force_tap_only() {
        for purpose in [InputPurpose::Password, InputPurpose::Pin] {
            let gate = SafetyGate::new(ContentType::new(purpose));
            assert_eq!(gate.mode(), SafetyMode::TapOnly, "{purpose:?}");
            assert!(!gate.swipe_allowed(), "swipe must be off for {purpose:?}");
            assert!(
                !gate.suggestions_allowed(),
                "suggestions must be off for {purpose:?}"
            );
            assert!(
                !gate.may_use_surrounding_text(),
                "surrounding text must not be read for {purpose:?}"
            );
        }
    }

    #[test]
    fn sensitive_hint_forces_tap_only_even_for_normal_purpose() {
        let gate = SafetyGate::new(ContentType {
            purpose: InputPurpose::Normal,
            sensitive_hint: true,
        });
        assert_eq!(gate.mode(), SafetyMode::TapOnly);
        assert!(!gate.swipe_allowed());
    }

    #[test]
    fn focus_change_updates_mode_both_ways() {
        let mut gate = SafetyGate::default();
        assert_eq!(gate.mode(), SafetyMode::Full);
        gate.set_content_type(ContentType::new(InputPurpose::Password));
        assert_eq!(gate.mode(), SafetyMode::TapOnly);
        gate.set_content_type(ContentType::new(InputPurpose::Email));
        assert_eq!(gate.mode(), SafetyMode::Full);
    }
}
