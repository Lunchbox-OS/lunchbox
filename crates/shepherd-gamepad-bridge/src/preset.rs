//! Preset state machines: translate gamepad state into a stream of output
//! events (pointer motion, button, scroll, key).
//!
//! All of this module is plain data and arithmetic — no I/O, no Wayland —
//! so the tests don't need a compositor or any real controller. The main
//! loop feeds the state with `ingest_button` / `ingest_axis` calls
//! sourced from gilrs events, then drives one `tick(dt)` per frame to get
//! the events to forward.
//!
//! Sign conventions follow gilrs:
//!   * Stick axes are "natural" — positive Y means stick pushed *up*.
//!   * D-pad / hat axes are "screen" — positive Y means D-pad *down*.
//!
//! The mappers in this file flip signs as needed so that mouse motion and
//! scroll come out in Wayland's screen-coord convention (positive = down /
//! right), matching what users expect: stick up → mouse up.

use std::collections::HashMap;
use std::time::Duration;

use gilrs::{Axis, Button};

// Linux evdev keycodes — see /usr/include/linux/input-event-codes.h.
// We avoid pulling the full evdev KeyCode type into this layer because the
// Wayland virtual-keyboard protocol takes raw evdev codes.
pub mod keycode {
    pub const KEY_ESC: u32 = 1;
    pub const KEY_W: u32 = 17;
    pub const KEY_E: u32 = 18;
    pub const KEY_R: u32 = 19;
    pub const KEY_ENTER: u32 = 28;
    pub const KEY_A: u32 = 30;
    pub const KEY_S: u32 = 31;
    pub const KEY_D: u32 = 32;
    pub const KEY_F: u32 = 33;
    pub const KEY_SPACE: u32 = 57;
    pub const KEY_UP: u32 = 103;
    pub const KEY_LEFT: u32 = 105;
    pub const KEY_RIGHT: u32 = 106;
    pub const KEY_DOWN: u32 = 108;
}

pub mod btncode {
    pub const BTN_LEFT: u32 = 0x110;
    pub const BTN_RIGHT: u32 = 0x111;
    pub const BTN_MIDDLE: u32 = 0x112;
}

// The output event vocabulary and scroll-axis type live in the shared
// `shepherd-bridge` crate, since both sidecars feed the same uinput backend.
pub use shepherd_bridge::{OutputEvent, ScrollAxis};

/// Tunables forwarded from the host adapter.
#[derive(Debug, Clone, Copy)]
pub struct Tunables {
    /// Radial deadzone for sticks, fraction of full deflection.
    pub deadzone: f32,
    /// Pixels per second at full stick deflection (mouse mode).
    pub mouse_speed: f32,
    /// Wheel notches per second at full stick deflection (scroll mode).
    pub scroll_speed: f32,
    /// Trigger threshold above which the trigger counts as pressed.
    pub trigger_threshold: f32,
    /// Stick magnitude above which a stick-as-directional-key counts as
    /// pressed.
    pub stick_key_threshold: f32,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            deadzone: 0.15,
            mouse_speed: 800.0,
            scroll_speed: 10.0,
            trigger_threshold: 0.5,
            stick_key_threshold: 0.5,
        }
    }
}

/// Current state of every input we care about. Updated incrementally as
/// the main loop drains gilrs events.
#[derive(Debug, Default, Clone)]
pub struct GamepadState {
    pub buttons: HashMap<Button, bool>,
    pub axes: HashMap<Axis, f32>,
}

impl GamepadState {
    pub fn set_button(&mut self, button: Button, pressed: bool) {
        self.buttons.insert(button, pressed);
    }

    pub fn set_axis(&mut self, axis: Axis, value: f32) {
        self.axes.insert(axis, value);
    }

    pub fn axis(&self, axis: Axis) -> f32 {
        self.axes.get(&axis).copied().unwrap_or(0.0)
    }

    pub fn button(&self, button: Button) -> bool {
        self.buttons.get(&button).copied().unwrap_or(false)
    }
}

/// Apply a radial deadzone + linear curve, returning the post-deadzone
/// vector. Inside the deadzone the result is `(0, 0)`; outside, the magnitude
/// is rescaled so that the deadzone boundary becomes the new zero point and
/// full deflection still reaches magnitude 1.
pub fn apply_radial_deadzone(x: f32, y: f32, deadzone: f32) -> (f32, f32) {
    let mag = (x * x + y * y).sqrt();
    if mag <= deadzone {
        return (0.0, 0.0);
    }
    let denom = (1.0 - deadzone).max(0.0001);
    let adjusted = ((mag - deadzone) / denom).min(1.0);
    let scale = adjusted / mag;
    (x * scale, y * scale)
}

/// Convert a stick deflection over a tick into integer mouse motion,
/// retaining fractional residue between ticks so sub-pixel motion still
/// adds up.
#[derive(Debug, Default, Clone, Copy)]
pub struct MouseAccum {
    pub residue_x: f32,
    pub residue_y: f32,
}

impl MouseAccum {
    pub fn step(&mut self, dx: f32, dy: f32) -> (i32, i32) {
        let total_x = dx + self.residue_x;
        let total_y = dy + self.residue_y;
        let out_x = total_x.trunc() as i32;
        let out_y = total_y.trunc() as i32;
        self.residue_x = total_x - out_x as f32;
        self.residue_y = total_y - out_y as f32;
        (out_x, out_y)
    }
}

/// Edge-triggered emitter for digital state: only push press/release when
/// the previous tick disagreed.
#[derive(Debug, Default, Clone)]
struct DigitalLatch<K: std::hash::Hash + Eq + Clone> {
    held: HashMap<K, bool>,
}

impl<K: std::hash::Hash + Eq + Clone> DigitalLatch<K> {
    fn set(&mut self, key: K, pressed: bool) -> Option<bool> {
        let prev = self.held.get(&key).copied().unwrap_or(false);
        if prev == pressed {
            return None;
        }
        self.held.insert(key, pressed);
        Some(pressed)
    }

    /// Release everything that is currently held — used at shutdown so we
    /// don't leave a key or button stuck down in the compositor.
    fn drain(&mut self) -> Vec<(K, bool)> {
        let mut out = Vec::new();
        for (k, v) in self.held.iter_mut() {
            if *v {
                *v = false;
                out.push((k.clone(), false));
            }
        }
        out
    }
}

/// Which stick currently drives the mouse vs. scroll (productivity preset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StickMode {
    LeftMouseRightScroll,
    LeftScrollRightMouse,
}

impl StickMode {
    fn toggle(self) -> Self {
        match self {
            Self::LeftMouseRightScroll => Self::LeftScrollRightMouse,
            Self::LeftScrollRightMouse => Self::LeftMouseRightScroll,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Productivity,
    Gpd,
}

/// Combined state for one running bridge: gamepad input snapshot, residue
/// accumulators, latched digital outputs, and (productivity preset) which
/// stick currently drives the mouse.
pub struct PresetState {
    preset: Preset,
    tunables: Tunables,
    gamepad: GamepadState,
    mouse_accum: MouseAccum,
    scroll_v_accum: f32,
    scroll_h_accum: f32,
    stick_mode: StickMode,
    /// Tracks the previous "edge-triggered" state of button outputs so the
    /// loop only emits press/release on transitions.
    button_latch: DigitalLatch<u32>,
    key_latch: DigitalLatch<u32>,
    /// Tracks the previous pressed state of the stick-click buttons so we
    /// only swap modes on the press edge, not on hold or release.
    prev_left_thumb: bool,
    prev_right_thumb: bool,
}

impl PresetState {
    pub fn new(preset: Preset, tunables: Tunables) -> Self {
        Self {
            preset,
            tunables,
            gamepad: GamepadState::default(),
            mouse_accum: MouseAccum::default(),
            scroll_v_accum: 0.0,
            scroll_h_accum: 0.0,
            stick_mode: StickMode::LeftMouseRightScroll,
            button_latch: DigitalLatch::default(),
            key_latch: DigitalLatch::default(),
            prev_left_thumb: false,
            prev_right_thumb: false,
        }
    }

    pub fn ingest_button(&mut self, button: Button, pressed: bool) {
        self.gamepad.set_button(button, pressed);
    }

    pub fn ingest_axis(&mut self, axis: Axis, value: f32) {
        self.gamepad.set_axis(axis, value);
    }

    /// Produce all output events for one tick. The caller passes `dt`, the
    /// wall-clock elapsed since the previous tick.
    pub fn tick(&mut self, dt: Duration) -> Vec<OutputEvent> {
        let dt_s = dt.as_secs_f32();
        match self.preset {
            Preset::Productivity => self.tick_productivity(dt_s),
            Preset::Gpd => self.tick_gpd(dt_s),
        }
    }

    /// Release everything currently held. Call at shutdown so the compositor
    /// doesn't see a phantom key/button stuck down after the bridge exits.
    pub fn drain_held(&mut self) -> Vec<OutputEvent> {
        let mut out = Vec::new();
        for (button, _) in self.button_latch.drain() {
            out.push(OutputEvent::PointerButton {
                button,
                pressed: false,
            });
        }
        for (keycode, _) in self.key_latch.drain() {
            out.push(OutputEvent::Key {
                keycode,
                pressed: false,
            });
        }
        out
    }

    // ---- Productivity preset --------------------------------------------

    fn tick_productivity(&mut self, dt: f32) -> Vec<OutputEvent> {
        let mut out = Vec::new();

        // Toggle on stick-click press edge (either thumb).
        let lt = self.gamepad.button(Button::LeftThumb);
        let rt = self.gamepad.button(Button::RightThumb);
        if (lt && !self.prev_left_thumb) || (rt && !self.prev_right_thumb) {
            self.stick_mode = self.stick_mode.toggle();
        }
        self.prev_left_thumb = lt;
        self.prev_right_thumb = rt;

        // Mouse and scroll sticks.
        let (mouse_x_axis, mouse_y_axis, scroll_x_axis, scroll_y_axis) = match self.stick_mode {
            StickMode::LeftMouseRightScroll => (
                Axis::LeftStickX,
                Axis::LeftStickY,
                Axis::RightStickX,
                Axis::RightStickY,
            ),
            StickMode::LeftScrollRightMouse => (
                Axis::RightStickX,
                Axis::RightStickY,
                Axis::LeftStickX,
                Axis::LeftStickY,
            ),
        };

        self.emit_mouse_from_stick(mouse_x_axis, mouse_y_axis, dt, &mut out);
        self.emit_scroll_from_stick(scroll_x_axis, scroll_y_axis, dt, &mut out);

        // Triggers + bumpers → mouse buttons.
        let lmb = self.trigger_pressed(Axis::LeftZ, Button::LeftTrigger2)
            || self.trigger_pressed(Axis::RightZ, Button::RightTrigger2);
        let rmb =
            self.gamepad.button(Button::LeftTrigger) || self.gamepad.button(Button::RightTrigger);
        self.emit_button(btncode::BTN_LEFT, lmb, &mut out);
        self.emit_button(btncode::BTN_RIGHT, rmb, &mut out);

        // D-pad → arrow keys. Screen-coord convention: positive Y = down,
        // so negative Y is "up". `dpad()` reads whichever form gilrs
        // delivers (hat axis or DPad buttons).
        let (hat_x, hat_y) = self.dpad();
        self.emit_key(keycode::KEY_LEFT, hat_x < -0.5, &mut out);
        self.emit_key(keycode::KEY_RIGHT, hat_x > 0.5, &mut out);
        self.emit_key(keycode::KEY_UP, hat_y < -0.5, &mut out);
        self.emit_key(keycode::KEY_DOWN, hat_y > 0.5, &mut out);

        // Face buttons.
        self.emit_key(
            keycode::KEY_ENTER,
            self.gamepad.button(Button::South),
            &mut out,
        );
        self.emit_key(
            keycode::KEY_ESC,
            self.gamepad.button(Button::Start),
            &mut out,
        );

        out
    }

    // ---- GPD preset -----------------------------------------------------

    fn tick_gpd(&mut self, dt: f32) -> Vec<OutputEvent> {
        let mut out = Vec::new();

        // Right stick = mouse motion.
        self.emit_mouse_from_stick(Axis::RightStickX, Axis::RightStickY, dt, &mut out);

        // Left stick = WASD. Apply radial deadzone, then per-axis
        // threshold so the user can hold W+A diagonally. gilrs sticks are
        // "natural": positive Y = stick up = W (forward).
        let lx = self.gamepad.axis(Axis::LeftStickX);
        let ly = self.gamepad.axis(Axis::LeftStickY);
        let (lx, ly) = apply_radial_deadzone(lx, ly, self.tunables.deadzone);
        let t = self.tunables.stick_key_threshold;
        self.emit_key(keycode::KEY_W, ly > t, &mut out);
        self.emit_key(keycode::KEY_S, ly < -t, &mut out);
        self.emit_key(keycode::KEY_A, lx < -t, &mut out);
        self.emit_key(keycode::KEY_D, lx > t, &mut out);

        // D-pad = scroll. Values are screen-coord and already discrete, so
        // we can drive scroll directly without flipping. `dpad()` reads
        // whichever form gilrs delivers (hat axis or DPad buttons).
        let (hat_x, hat_y) = self.dpad();
        if hat_y != 0.0 {
            self.scroll_v_accum += hat_y * self.tunables.scroll_speed * dt;
        }
        if hat_x != 0.0 {
            self.scroll_h_accum += hat_x * self.tunables.scroll_speed * dt;
        }
        self.flush_scroll(&mut out);

        // Triggers + bumper → mouse buttons.
        let lmb = self.trigger_pressed(Axis::LeftZ, Button::LeftTrigger2);
        let rmb = self.trigger_pressed(Axis::RightZ, Button::RightTrigger2);
        let mmb = self.gamepad.button(Button::LeftTrigger);
        self.emit_button(btncode::BTN_LEFT, lmb, &mut out);
        self.emit_button(btncode::BTN_RIGHT, rmb, &mut out);
        self.emit_button(btncode::BTN_MIDDLE, mmb, &mut out);

        // Face buttons.
        self.emit_key(
            keycode::KEY_SPACE,
            self.gamepad.button(Button::South),
            &mut out,
        );
        self.emit_key(keycode::KEY_E, self.gamepad.button(Button::East), &mut out);
        self.emit_key(keycode::KEY_R, self.gamepad.button(Button::North), &mut out);
        self.emit_key(keycode::KEY_F, self.gamepad.button(Button::West), &mut out);

        out
    }

    // ---- Shared helpers --------------------------------------------------

    /// Resolve the D-pad as an `(x, y)` pair in `{-1.0, 0.0, 1.0}` using the
    /// screen-coordinate convention (positive x = right, positive y = down).
    ///
    /// gilrs delivers the D-pad in one of two forms depending on the
    /// controller, and we must read both:
    ///   * As the `Axis::DPadX`/`DPadY` hat axes, or
    ///   * As `Button::DPad{Up,Down,Left,Right}` — gilrs's default
    ///     `axis_dpad_to_button` filter rewrites a hat-only D-pad (e.g. the
    ///     Legion Go S, whose D-pad is `ABS_HAT0X/0Y` with no D-pad buttons)
    ///     into button events and drops the original axis event. Reading only
    ///     the axes would miss the D-pad entirely on those controllers.
    ///
    /// A pressed button wins over the axis so either delivery form works.
    fn dpad(&self) -> (f32, f32) {
        let mut x = self.gamepad.axis(Axis::DPadX);
        let mut y = self.gamepad.axis(Axis::DPadY);
        if self.gamepad.button(Button::DPadRight) {
            x = 1.0;
        } else if self.gamepad.button(Button::DPadLeft) {
            x = -1.0;
        }
        if self.gamepad.button(Button::DPadDown) {
            y = 1.0;
        } else if self.gamepad.button(Button::DPadUp) {
            y = -1.0;
        }
        (x, y)
    }

    fn emit_mouse_from_stick(
        &mut self,
        x_axis: Axis,
        y_axis: Axis,
        dt: f32,
        out: &mut Vec<OutputEvent>,
    ) {
        let x = self.gamepad.axis(x_axis);
        let y = self.gamepad.axis(y_axis);
        let (x, y) = apply_radial_deadzone(x, y, self.tunables.deadzone);
        if x == 0.0 && y == 0.0 {
            // No motion this tick. Reset residue — sub-pixel motion accrued
            // under deflection would warp the pointer on stick release.
            self.mouse_accum.residue_x = 0.0;
            self.mouse_accum.residue_y = 0.0;
            return;
        }
        // Flip Y: gilrs sticks are positive-up; Wayland motion is
        // positive-down. Stick up → mouse up.
        let dx = x * self.tunables.mouse_speed * dt;
        let dy = -y * self.tunables.mouse_speed * dt;
        let (out_x, out_y) = self.mouse_accum.step(dx, dy);
        if out_x != 0 || out_y != 0 {
            out.push(OutputEvent::PointerMotion {
                dx: out_x as f32,
                dy: out_y as f32,
            });
        }
    }

    fn emit_scroll_from_stick(
        &mut self,
        x_axis: Axis,
        y_axis: Axis,
        dt: f32,
        out: &mut Vec<OutputEvent>,
    ) {
        let x = self.gamepad.axis(x_axis);
        let y = self.gamepad.axis(y_axis);
        let (x, y) = apply_radial_deadzone(x, y, self.tunables.deadzone);
        // Same Y flip as mouse motion: stick up = scroll up (negative axis
        // value in Wayland's convention).
        self.scroll_h_accum += x * self.tunables.scroll_speed * dt;
        self.scroll_v_accum += -y * self.tunables.scroll_speed * dt;
        if x == 0.0 && y == 0.0 {
            // Reset accumulators on release so slow accrued scroll doesn't
            // teleport when the user lets go.
            self.scroll_h_accum = 0.0;
            self.scroll_v_accum = 0.0;
            return;
        }
        self.flush_scroll(out);
    }

    fn flush_scroll(&mut self, out: &mut Vec<OutputEvent>) {
        let v_notches = self.scroll_v_accum.trunc() as i32;
        if v_notches != 0 {
            self.scroll_v_accum -= v_notches as f32;
            out.push(OutputEvent::PointerScroll {
                axis: ScrollAxis::Vertical,
                discrete: v_notches,
            });
        }
        let h_notches = self.scroll_h_accum.trunc() as i32;
        if h_notches != 0 {
            self.scroll_h_accum -= h_notches as f32;
            out.push(OutputEvent::PointerScroll {
                axis: ScrollAxis::Horizontal,
                discrete: h_notches,
            });
        }
    }

    fn trigger_pressed(&self, axis: Axis, fallback: Button) -> bool {
        let analog = self.gamepad.axis(axis);
        if analog > self.tunables.trigger_threshold {
            return true;
        }
        // Some controllers expose triggers only as digital buttons; honor
        // those too so the bridge works across the controller landscape.
        self.gamepad.button(fallback)
    }

    fn emit_button(&mut self, code: u32, pressed: bool, out: &mut Vec<OutputEvent>) {
        if let Some(new_state) = self.button_latch.set(code, pressed) {
            out.push(OutputEvent::PointerButton {
                button: code,
                pressed: new_state,
            });
        }
    }

    fn emit_key(&mut self, code: u32, pressed: bool, out: &mut Vec<OutputEvent>) {
        if let Some(new_state) = self.key_latch.set(code, pressed) {
            out.push(OutputEvent::Key {
                keycode: code,
                pressed: new_state,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tunables() -> Tunables {
        Tunables {
            deadzone: 0.1,
            mouse_speed: 100.0,
            scroll_speed: 10.0,
            trigger_threshold: 0.5,
            stick_key_threshold: 0.5,
        }
    }

    #[test]
    fn deadzone_inside_returns_zero() {
        let (x, y) = apply_radial_deadzone(0.05, 0.05, 0.1);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn deadzone_outside_rescales_to_unit() {
        let (x, _) = apply_radial_deadzone(1.0, 0.0, 0.1);
        assert!((x - 1.0).abs() < 0.001);
    }

    #[test]
    fn deadzone_just_outside_starts_at_zero() {
        let (x, _) = apply_radial_deadzone(0.1 + 1e-4, 0.0, 0.1);
        assert!(x.abs() < 0.01, "got {x}");
    }

    #[test]
    fn mouse_accum_preserves_subpixel_motion() {
        let mut accum = MouseAccum::default();
        // Three ticks of 0.4 each should yield ~1 px total motion.
        let (a, _) = accum.step(0.4, 0.0);
        let (b, _) = accum.step(0.4, 0.0);
        let (c, _) = accum.step(0.4, 0.0);
        assert_eq!(a + b + c, 1);
    }

    #[test]
    fn productivity_stick_drives_mouse_by_default() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_axis(Axis::LeftStickX, 1.0);
        let out = state.tick(Duration::from_millis(100));
        // 100 px/sec * 0.1 sec = 10 px on X
        let motion: i32 = out
            .iter()
            .filter_map(|e| match e {
                OutputEvent::PointerMotion { dx, .. } => Some(*dx as i32),
                _ => None,
            })
            .sum();
        assert_eq!(motion, 10);
    }

    #[test]
    fn productivity_stick_up_moves_mouse_up() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        // gilrs: stick fully up = +1 on Y.
        state.ingest_axis(Axis::LeftStickY, 1.0);
        let out = state.tick(Duration::from_millis(100));
        let dy: i32 = out
            .iter()
            .filter_map(|e| match e {
                OutputEvent::PointerMotion { dy, .. } => Some(*dy as i32),
                _ => None,
            })
            .sum();
        // Up should be negative dy in Wayland's screen coords.
        assert_eq!(dy, -10);
    }

    #[test]
    fn productivity_left_thumb_press_swaps_mouse_to_right_stick() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_button(Button::LeftThumb, true);
        let _ = state.tick(Duration::from_millis(100));
        state.ingest_button(Button::LeftThumb, false);

        // Left stick deflected — should NOT drive the mouse anymore.
        state.ingest_axis(Axis::LeftStickX, 1.0);
        let out = state.tick(Duration::from_millis(100));
        let motion: i32 = out
            .iter()
            .filter_map(|e| match e {
                OutputEvent::PointerMotion { dx, .. } => Some(*dx as i32),
                _ => None,
            })
            .sum();
        // The left stick now drives scroll, not motion.
        assert_eq!(motion, 0);

        // Right stick deflection now drives the mouse.
        state.ingest_axis(Axis::RightStickX, 1.0);
        let out = state.tick(Duration::from_millis(100));
        let motion: i32 = out
            .iter()
            .filter_map(|e| match e {
                OutputEvent::PointerMotion { dx, .. } => Some(*dx as i32),
                _ => None,
            })
            .sum();
        assert_eq!(motion, 10);
    }

    #[test]
    fn productivity_thumb_held_toggles_only_once() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_button(Button::LeftThumb, true);
        let _ = state.tick(Duration::from_millis(10));
        // Stays held across many ticks — must NOT keep flipping.
        for _ in 0..5 {
            let _ = state.tick(Duration::from_millis(10));
        }
        state.ingest_axis(Axis::RightStickX, 1.0);
        let out = state.tick(Duration::from_millis(100));
        let motion: i32 = out
            .iter()
            .filter_map(|e| match e {
                OutputEvent::PointerMotion { dx, .. } => Some(*dx as i32),
                _ => None,
            })
            .sum();
        // After one toggle, right stick should drive the mouse.
        assert_eq!(motion, 10);
    }

    #[test]
    fn productivity_trigger_press_emits_left_click_once() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_axis(Axis::LeftZ, 1.0);
        let out1 = state.tick(Duration::from_millis(10));
        let out2 = state.tick(Duration::from_millis(10));
        // Press on tick 1, no event on tick 2 (still held).
        assert!(out1.iter().any(
            |e| matches!(e, OutputEvent::PointerButton { button, pressed: true } if *button == btncode::BTN_LEFT)
        ));
        assert!(
            !out2
                .iter()
                .any(|e| matches!(e, OutputEvent::PointerButton { .. }))
        );
    }

    #[test]
    fn productivity_face_south_emits_enter() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_button(Button::South, true);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: true } if *keycode == keycode::KEY_ENTER)
        ));
    }

    #[test]
    fn productivity_dpad_up_emits_up_arrow() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        // gilrs DPadY is screen-coord: negative = up.
        state.ingest_axis(Axis::DPadY, -1.0);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: true } if *keycode == keycode::KEY_UP)
        ));
    }

    #[test]
    fn productivity_dpad_button_up_emits_up_arrow() {
        // Controllers whose hat D-pad gilrs rewrites to buttons (e.g. Legion
        // Go S) deliver Button::DPadUp instead of an axis change. The preset
        // must still emit the up arrow.
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_button(Button::DPadUp, true);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: true } if *keycode == keycode::KEY_UP)
        ));
    }

    #[test]
    fn productivity_dpad_button_right_emits_right_arrow() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_button(Button::DPadRight, true);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: true } if *keycode == keycode::KEY_RIGHT)
        ));
    }

    #[test]
    fn productivity_dpad_button_release_emits_arrow_release() {
        let mut state = PresetState::new(Preset::Productivity, tunables());
        state.ingest_button(Button::DPadUp, true);
        let _ = state.tick(Duration::from_millis(10));
        state.ingest_button(Button::DPadUp, false);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: false } if *keycode == keycode::KEY_UP)
        ));
    }

    #[test]
    fn gpd_dpad_button_down_scrolls() {
        // GPD preset maps the D-pad to scroll; the button-delivery form must
        // produce a downward vertical scroll notch.
        let mut state = PresetState::new(Preset::Gpd, tunables());
        state.ingest_button(Button::DPadDown, true);
        // scroll_speed=10/s, so 0.2s accrues 2 notches (>=1 flushes).
        let out = state.tick(Duration::from_millis(200));
        assert!(out.iter().any(|e| matches!(
            e,
            OutputEvent::PointerScroll {
                axis: ScrollAxis::Vertical,
                discrete
            } if *discrete > 0
        )));
    }

    #[test]
    fn gpd_left_stick_up_emits_w() {
        let mut state = PresetState::new(Preset::Gpd, tunables());
        // gilrs: stick fully up = +1 (natural coord).
        state.ingest_axis(Axis::LeftStickY, 1.0);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: true } if *keycode == keycode::KEY_W)
        ));
    }

    #[test]
    fn gpd_left_stick_release_emits_w_release() {
        let mut state = PresetState::new(Preset::Gpd, tunables());
        state.ingest_axis(Axis::LeftStickY, 1.0);
        let _ = state.tick(Duration::from_millis(10));
        state.ingest_axis(Axis::LeftStickY, 0.0);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: false } if *keycode == keycode::KEY_W)
        ));
    }

    #[test]
    fn gpd_right_stick_drives_mouse_not_left() {
        let mut state = PresetState::new(Preset::Gpd, tunables());
        state.ingest_axis(Axis::LeftStickX, 1.0);
        state.ingest_axis(Axis::RightStickX, 1.0);
        let out = state.tick(Duration::from_millis(100));
        let motion: i32 = out
            .iter()
            .filter_map(|e| match e {
                OutputEvent::PointerMotion { dx, .. } => Some(*dx as i32),
                _ => None,
            })
            .sum();
        assert_eq!(motion, 10);
    }

    #[test]
    fn gpd_left_bumper_emits_middle_click() {
        let mut state = PresetState::new(Preset::Gpd, tunables());
        state.ingest_button(Button::LeftTrigger, true);
        let out = state.tick(Duration::from_millis(10));
        assert!(out.iter().any(
            |e| matches!(e, OutputEvent::PointerButton { button, pressed: true } if *button == btncode::BTN_MIDDLE)
        ));
    }

    #[test]
    fn drain_held_emits_releases() {
        let mut state = PresetState::new(Preset::Gpd, tunables());
        state.ingest_button(Button::South, true);
        let _ = state.tick(Duration::from_millis(10));
        let releases = state.drain_held();
        assert!(releases.iter().any(
            |e| matches!(e, OutputEvent::Key { keycode, pressed: false } if *keycode == keycode::KEY_SPACE)
        ));
    }
}
