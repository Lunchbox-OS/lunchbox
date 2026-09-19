//! Automatic-brightness policy — the pure, hardware-free core.
//!
//! Two pieces live here so they can be unit-tested without a light sensor,
//! a backlight, or a runtime:
//!
//! * [`AutoBrightnessCurve`] maps an ambient-light reading (lux) to a target
//!   brightness percent. The mapping is logarithmic because perceived
//!   brightness and typical indoor→outdoor lux both scale roughly with the
//!   log of illuminance, so a linear lux→percent ramp would spend almost its
//!   whole range on the last few percent of a sunny window.
//!
//! * [`AutoBrightnessState`] is the runtime state machine: whether auto mode
//!   is on, and the "phone-style" manual override — a manual brightness set
//!   temporarily wins, and auto resumes only once the ambient light has
//!   changed enough to suggest the room's lighting genuinely shifted.
//!
//! The actual sensor read, hardware write, policy clamp, and event broadcast
//! live in [`crate::service`]; this module only decides *what* should happen.

/// How much the ambient light must change, as a ratio, before a manual
/// override is abandoned and auto brightness resumes. `1.8` means the light
/// roughly has to drop below ~55% or rise above ~180% of what it was when the
/// user last set brightness by hand. Ratio (not absolute lux) so the same
/// threshold behaves sensibly in a dim room and a bright one.
const LUX_RESUME_RATIO: f32 = 1.8;

/// Minimum difference between the freshly-computed target and the brightness
/// currently on screen before auto mode bothers writing. Suppresses jitter:
/// without it, a sensor hovering on a curve boundary would rewrite the
/// backlight every tick by ±1%.
const APPLY_MIN_DELTA: u8 = 3;

/// Lux→percent curve. Field units: `dim_lux`/`bright_lux` are lux;
/// `min_percent`/`max_percent` are 0–100 and bound the auto range (further
/// clamping by policy restrictions happens in the service).
#[derive(Debug, Clone, PartialEq)]
pub struct AutoBrightnessCurve {
    /// At or below this many lux, target = `min_percent`.
    pub dim_lux: f32,
    /// At or above this many lux, target = `max_percent`.
    pub bright_lux: f32,
    /// Brightness percent at the dim end of the curve.
    pub min_percent: u8,
    /// Brightness percent at the bright end of the curve.
    pub max_percent: u8,
}

impl AutoBrightnessCurve {
    /// Map an illuminance in lux to a target brightness percent.
    pub fn lux_to_percent(&self, lux: f32) -> u8 {
        // `ln` needs a strictly-positive, well-ordered domain. Guard against
        // a misconfigured curve (dim >= bright) and against 0 lux.
        let lo = self.dim_lux.max(1.0);
        let hi = self.bright_lux.max(lo * 1.0001);
        let lux = lux.clamp(lo, hi);
        let t = (lux.ln() - lo.ln()) / (hi.ln() - lo.ln());
        let min = f32::from(self.min_percent);
        let max = f32::from(self.max_percent);
        (min + t * (max - min)).round().clamp(0.0, 100.0) as u8
    }
}

/// What the auto-brightness loop should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoAction {
    /// Write this brightness percent (already policy-clamped) to the panel.
    Apply(u8),
    /// Leave the panel alone.
    Hold,
}

/// Runtime state for automatic brightness.
#[derive(Debug, Default, Clone)]
pub struct AutoBrightnessState {
    enabled: bool,
    /// Lux at the moment the user last set brightness manually while auto was
    /// on. `Some` means an override is active; auto resumes when the light
    /// moves away from this baseline by [`LUX_RESUME_RATIO`].
    manual_baseline_lux: Option<f32>,
}

impl AutoBrightnessState {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            manual_baseline_lux: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turn auto mode on or off. Enabling always clears any stale manual
    /// override so the next tick applies immediately.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.manual_baseline_lux = None;
    }

    /// Record that the user just set brightness by hand at `lux`. No-op when
    /// auto is off (a manual set outside auto mode needs no override state).
    pub fn begin_manual_override(&mut self, lux: f32) {
        if self.enabled {
            self.manual_baseline_lux = Some(lux);
        }
    }

    /// Whether a manual override is currently suppressing auto adjustments.
    pub fn manual_override_active(&self) -> bool {
        self.manual_baseline_lux.is_some()
    }

    /// Decide this tick's action given the current ambient `lux`, the
    /// brightness `current_percent` already on screen, the `curve`, and a
    /// `clamp` closure applying policy restrictions (min/max) to a percent.
    ///
    /// Mutates `self` to expire a manual override once the light has shifted.
    pub fn tick(
        &mut self,
        curve: &AutoBrightnessCurve,
        lux: f32,
        current_percent: u8,
        clamp: impl Fn(u8) -> u8,
    ) -> AutoAction {
        if !self.enabled {
            return AutoAction::Hold;
        }

        // Honor a manual override until the room's light has moved enough.
        if let Some(baseline) = self.manual_baseline_lux {
            if lux_ratio_change(baseline, lux) >= LUX_RESUME_RATIO {
                self.manual_baseline_lux = None; // resume auto
            } else {
                return AutoAction::Hold;
            }
        }

        let target = clamp(curve.lux_to_percent(lux));
        if current_percent.abs_diff(target) < APPLY_MIN_DELTA {
            return AutoAction::Hold;
        }
        AutoAction::Apply(target)
    }
}

/// Multiplicative distance between two lux readings, symmetric and guarded
/// against zero (sensors legitimately read 0 in the dark).
fn lux_ratio_change(a: f32, b: f32) -> f32 {
    let a = a.max(1.0);
    let b = b.max(1.0);
    if a >= b { a / b } else { b / a }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> AutoBrightnessCurve {
        AutoBrightnessCurve {
            dim_lux: 10.0,
            bright_lux: 1000.0,
            min_percent: 20,
            max_percent: 100,
        }
    }

    #[test]
    fn curve_pins_endpoints_and_is_monotonic() {
        let c = curve();
        assert_eq!(c.lux_to_percent(0.0), 20);
        assert_eq!(c.lux_to_percent(10.0), 20);
        assert_eq!(c.lux_to_percent(5.0), 20); // below dim clamps to min
        assert_eq!(c.lux_to_percent(1000.0), 100);
        assert_eq!(c.lux_to_percent(50_000.0), 100); // above bright clamps to max
        // Midpoint on the log scale (100 lux) sits halfway: 20 + 0.5*80 = 60.
        assert_eq!(c.lux_to_percent(100.0), 60);
        // Monotonic non-decreasing.
        let mut prev = 0u8;
        for lux in [0.0, 8.0, 15.0, 40.0, 100.0, 300.0, 900.0, 2000.0] {
            let p = c.lux_to_percent(lux);
            assert!(p >= prev, "not monotonic at {lux}: {p} < {prev}");
            prev = p;
        }
    }

    #[test]
    fn degenerate_curve_does_not_panic() {
        let c = AutoBrightnessCurve {
            dim_lux: 500.0,
            bright_lux: 500.0, // dim == bright
            min_percent: 30,
            max_percent: 90,
        };
        // Just must not panic / must stay in range.
        for lux in [0.0, 500.0, 10_000.0] {
            assert!(c.lux_to_percent(lux) <= 100);
        }
    }

    #[test]
    fn disabled_state_always_holds() {
        let mut s = AutoBrightnessState::new(false);
        assert_eq!(s.tick(&curve(), 5000.0, 20, |p| p), AutoAction::Hold);
    }

    #[test]
    fn enabled_applies_target_with_hysteresis() {
        let mut s = AutoBrightnessState::new(true);
        // Bright room, screen currently dim → apply max.
        assert_eq!(s.tick(&curve(), 1000.0, 20, |p| p), AutoAction::Apply(100));
        // Already near target → hold (within APPLY_MIN_DELTA).
        assert_eq!(s.tick(&curve(), 1000.0, 99, |p| p), AutoAction::Hold);
    }

    #[test]
    fn clamp_closure_caps_target() {
        let mut s = AutoBrightnessState::new(true);
        // Bright room wants 100, but a bedtime policy caps at 40.
        let action = s.tick(&curve(), 1000.0, 10, |p| p.min(40));
        assert_eq!(action, AutoAction::Apply(40));
    }

    #[test]
    fn manual_override_holds_until_light_shifts() {
        let mut s = AutoBrightnessState::new(true);
        // User sets brightness by hand in a 100-lux room.
        s.begin_manual_override(100.0);
        assert!(s.manual_override_active());
        // Small light wiggle → still overridden, hold.
        assert_eq!(s.tick(&curve(), 120.0, 55, |p| p), AutoAction::Hold);
        assert!(s.manual_override_active());
        // Light drops well below threshold (100 → 40 is 2.5x) → resume auto.
        let action = s.tick(&curve(), 40.0, 55, |p| p);
        assert!(!s.manual_override_active());
        assert!(matches!(action, AutoAction::Apply(_)));
    }

    #[test]
    fn set_enabled_clears_override() {
        let mut s = AutoBrightnessState::new(true);
        s.begin_manual_override(100.0);
        assert!(s.manual_override_active());
        s.set_enabled(true); // re-enabling clears stale override
        assert!(!s.manual_override_active());
    }

    #[test]
    fn manual_override_ignored_when_disabled() {
        let mut s = AutoBrightnessState::new(false);
        s.begin_manual_override(100.0);
        assert!(!s.manual_override_active());
    }
}
