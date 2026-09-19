//! The backend-agnostic synthetic-input vocabulary.
//!
//! Both bridges' pure mapping layers produce a stream of these; an
//! [`OutputSink`](crate::OutputSink) consumes them. All codes are raw Linux
//! evdev codes (`BTN_LEFT`, `KEY_W`, …), matching what `/dev/uinput` expects
//! directly.

/// Scroll axis for [`OutputEvent::PointerScroll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

/// One synthetic-input event ready to be emitted by a sink.
///
/// Sign conventions match the Wayland screen-coordinate model the preset
/// layer already used: positive scroll/motion is down/right. The uinput
/// backend flips signs as needed for the evdev wheel convention.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputEvent {
    /// Relative pointer motion, in compositor pixels (gamepad bridge).
    PointerMotion { dx: f32, dy: f32 },
    /// Absolute pointer position (touch bridge). `x`/`y` are raw values in
    /// `0..=*_extent`; the sink rescales into its device's declared range.
    PointerMotionAbsolute {
        x: u32,
        y: u32,
        x_extent: u32,
        y_extent: u32,
    },
    /// Mouse button press/release. `button` is an evdev code (`BTN_LEFT`, …).
    PointerButton { button: u32, pressed: bool },
    /// Discrete wheel scroll. `discrete` is in notches (positive = down/right).
    PointerScroll { axis: ScrollAxis, discrete: i32 },
    /// Keyboard key press/release. `keycode` is the Linux evdev code.
    Key { keycode: u32, pressed: bool },
    /// A touch contact begins (tablet bridge). `slot` identifies the contact
    /// (`0` for the first finger). `x`/`y` are raw absolute values in
    /// `0..=*_extent`; the sink rescales into its device's declared range, the
    /// same convention as [`PointerMotionAbsolute`](Self::PointerMotionAbsolute).
    TouchDown {
        slot: u32,
        x: u32,
        y: u32,
        x_extent: u32,
        y_extent: u32,
    },
    /// An existing contact moves. Same coordinate convention as
    /// [`TouchDown`](Self::TouchDown).
    TouchMotion {
        slot: u32,
        x: u32,
        y: u32,
        x_extent: u32,
        y_extent: u32,
    },
    /// A contact lifts.
    TouchUp { slot: u32 },
}
