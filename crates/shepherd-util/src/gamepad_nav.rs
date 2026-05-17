//! Analog-stick navigation: translate a held gamepad stick into discrete
//! focus moves with auto-repeat. Used by both `shepherd-launcher-ui` and
//! `shepherd-media` so the launchers share the same feel.
//!
//! Pure Rust by design — the caller is responsible for reading the stick's
//! current value from gilrs (or wherever) and passing `(x, y, Instant)` in
//! each tick. The mapping from gilrs `Button`/`Axis` to focus moves stays
//! at the call site since the action vocabularies differ between apps.

use std::time::{Duration, Instant};

/// A single discrete navigation step in the focus grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavDir {
    Up,
    Down,
    Left,
    Right,
}

/// Translates a held analog stick into discrete focus moves: one fire on
/// crossing the deadzone, then [`Self::INITIAL_DELAY`] of pause, then
/// auto-repeat at [`Self::REPEAT_INTERVAL`]. Mirrors the cadence of
/// held-key auto-repeat in most game UIs.
///
/// Call [`Self::tick`] every frame (or polling tick) with the current
/// stick value and the current time; act on the returned [`NavDir`].
#[derive(Default)]
pub struct StickNav {
    /// Direction the stick is currently held in, or `None` when centered.
    active: Option<NavDir>,
    /// Wall-clock time of the most recent fire in the current hold. Used
    /// to throttle auto-repeat.
    last_fire: Option<Instant>,
    /// Fires emitted since the stick last crossed the deadzone — the first
    /// fire is immediate, the second waits `INITIAL_DELAY`, the rest repeat.
    fires: u32,
}

impl StickNav {
    /// Magnitude below which a stick axis is treated as centered.
    pub const DEADZONE: f32 = 0.5;
    /// Pause between the first fire on a hold and the first auto-repeat.
    pub const INITIAL_DELAY: Duration = Duration::from_millis(400);
    /// Cadence of auto-repeat fires while the stick is held.
    pub const REPEAT_INTERVAL: Duration = Duration::from_millis(120);

    /// Returns the focus direction to fire this tick, if any.
    pub fn tick(&mut self, x: f32, y: f32, now: Instant) -> Option<NavDir> {
        let dir = stick_direction(x, y);
        if dir != self.active {
            self.active = dir;
            self.fires = 0;
            self.last_fire = None;
        }
        let dir = dir?;
        let elapsed = self.last_fire.map(|t| now.duration_since(t));
        let should_fire = match (self.fires, elapsed) {
            (0, _) => true,
            (1, Some(e)) => e >= Self::INITIAL_DELAY,
            (_, Some(e)) => e >= Self::REPEAT_INTERVAL,
            (_, None) => true,
        };
        if !should_fire {
            return None;
        }
        self.fires = self.fires.saturating_add(1);
        self.last_fire = Some(now);
        Some(dir)
    }

    /// Whether the stick is currently past the deadzone. Useful for asking
    /// the caller's main loop to keep ticking so auto-repeat can fire.
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

/// Classify a stick `(x, y)` into a cardinal direction, applying a square
/// deadzone and picking the dominant axis. Returns `None` when the stick
/// is within the deadzone on both axes.
///
/// Y convention: positive `y` is "up" (the gilrs convention for analog
/// sticks). Callers that work in screen coordinates must invert before
/// passing in.
pub fn stick_direction(x: f32, y: f32) -> Option<NavDir> {
    let ax = x.abs();
    let ay = y.abs();
    if ax < StickNav::DEADZONE && ay < StickNav::DEADZONE {
        return None;
    }
    if ax >= ay {
        Some(if x > 0.0 { NavDir::Right } else { NavDir::Left })
    } else {
        Some(if y > 0.0 { NavDir::Up } else { NavDir::Down })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fire_at(nav: &mut StickNav, x: f32, y: f32, t_ms: u64) -> Option<NavDir> {
        nav.tick(x, y, Instant::now() + Duration::from_millis(t_ms))
    }

    #[test]
    fn fires_once_then_waits_for_initial_delay() {
        let mut nav = StickNav::default();
        assert_eq!(fire_at(&mut nav, 1.0, 0.0, 0), Some(NavDir::Right));
        assert_eq!(fire_at(&mut nav, 1.0, 0.0, 100), None);
        assert_eq!(fire_at(&mut nav, 1.0, 0.0, 399), None);
        assert_eq!(fire_at(&mut nav, 1.0, 0.0, 400), Some(NavDir::Right));
    }

    #[test]
    fn auto_repeats_after_initial_delay() {
        let mut nav = StickNav::default();
        assert!(fire_at(&mut nav, 0.0, 1.0, 0).is_some());
        assert!(fire_at(&mut nav, 0.0, 1.0, 400).is_some());
        assert_eq!(fire_at(&mut nav, 0.0, 1.0, 519), None);
        assert_eq!(fire_at(&mut nav, 0.0, 1.0, 520), Some(NavDir::Up));
    }

    #[test]
    fn deadzone_suppresses_fires_and_resets_hold() {
        let mut nav = StickNav::default();
        assert!(fire_at(&mut nav, 1.0, 0.0, 0).is_some());
        assert!(fire_at(&mut nav, 0.0, 0.0, 100).is_none());
        // After returning to center, a new push fires immediately again.
        assert_eq!(fire_at(&mut nav, 1.0, 0.0, 150), Some(NavDir::Right));
    }

    #[test]
    fn changing_direction_fires_immediately() {
        let mut nav = StickNav::default();
        assert_eq!(fire_at(&mut nav, 1.0, 0.0, 0), Some(NavDir::Right));
        assert_eq!(fire_at(&mut nav, -1.0, 0.0, 50), Some(NavDir::Left));
    }

    #[test]
    fn dominant_axis_wins_diagonals() {
        assert_eq!(stick_direction(0.9, 0.6), Some(NavDir::Right));
        assert_eq!(stick_direction(0.6, 0.9), Some(NavDir::Up));
        assert_eq!(stick_direction(-0.9, -0.6), Some(NavDir::Left));
        assert_eq!(stick_direction(0.0, -0.9), Some(NavDir::Down));
    }

    #[test]
    fn is_active_reflects_deadzone() {
        let mut nav = StickNav::default();
        assert!(!nav.is_active());
        nav.tick(1.0, 0.0, Instant::now());
        assert!(nav.is_active());
        nav.tick(0.0, 0.0, Instant::now());
        assert!(!nav.is_active());
    }
}
