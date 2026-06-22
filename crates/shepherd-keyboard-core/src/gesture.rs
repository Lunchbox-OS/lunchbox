//! Gesture assembly: raw captured touch points (surface pixels) → a normalized gesture in
//! the layout's coordinate space, ready for the decoder.
//!
//! Capture geometry must match decode geometry, so the host normalizes every point against
//! the same key-area rectangle it used to *render* the letter keys. A short, near-stationary
//! path is a **tap** (a single key press), not a swipe; this module makes that call so both
//! backends classify identically.

use shepherd_swipe_core::{Gesture, TouchPoint};

/// The rectangle, in surface pixels, that the layout's normalized `[0,1]×[0,1]` key area
/// occupies. The host derives it from its rendered letter grid (excluding the suggestion
/// bar and any function-key rows that fall outside the decoder's letter geometry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyArea {
    /// X of the key area's left edge, in surface pixels.
    pub x: f32,
    /// Y of the key area's top edge, in surface pixels.
    pub y: f32,
    /// Key-area width in surface pixels (must be > 0).
    pub width: f32,
    /// Key-area height in surface pixels (must be > 0).
    pub height: f32,
}

impl KeyArea {
    /// Normalize a surface-pixel point into layout coordinates (top-left origin). Results
    /// may fall slightly outside `[0,1]` on overshoot, which the gesture format allows.
    pub fn normalize(&self, px: f32, py: f32) -> (f32, f32) {
        ((px - self.x) / self.width, (py - self.y) / self.height)
    }
}

/// A raw captured touch sample in surface pixels with a host timestamp (any monotonic
/// millisecond clock; rebased to gesture-start on assembly).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawPoint {
    /// X in surface pixels.
    pub x: f32,
    /// Y in surface pixels.
    pub y: f32,
    /// Host timestamp in milliseconds (absolute; only differences matter).
    pub t_ms: u32,
}

/// The classified result of one touch interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Stroke {
    /// A tap at a normalized layout coordinate (resolve to a key via `Layout::nearest_key`).
    Tap {
        /// Normalized x in layout coordinates.
        x: f32,
        /// Normalized y in layout coordinates.
        y: f32,
    },
    /// A swipe path, ready to decode.
    Swipe(Gesture),
}

/// Default arc-length threshold (in normalized layout units) below which a path is a tap.
/// One key is `key_width = 0.1` wide, so ~one key of travel still reads as a tap.
pub const DEFAULT_TAP_MAX_PATH: f32 = 0.12;

/// Accumulates raw touch samples for one interaction and assembles them into a [`Stroke`].
#[derive(Debug, Clone)]
pub struct GestureBuilder {
    area: KeyArea,
    points: Vec<TouchPoint>,
    start_t: Option<u32>,
    tap_max_path: f32,
}

impl GestureBuilder {
    /// Start assembling a stroke captured against `area`.
    pub fn new(area: KeyArea) -> Self {
        Self {
            area,
            points: Vec::new(),
            start_t: None,
            tap_max_path: DEFAULT_TAP_MAX_PATH,
        }
    }

    /// Override the tap/swipe arc-length threshold (normalized units).
    pub fn with_tap_max_path(mut self, tap_max_path: f32) -> Self {
        self.tap_max_path = tap_max_path;
        self
    }

    /// Append a raw sample, normalizing it and rebasing its time to the gesture start.
    /// Time is clamped non-decreasing so a noisy clock can't produce an invalid gesture.
    pub fn push(&mut self, raw: RawPoint) {
        let (x, y) = self.area.normalize(raw.x, raw.y);
        let start = *self.start_t.get_or_insert(raw.t_ms);
        let mut t_ms = raw.t_ms.saturating_sub(start);
        if let Some(prev) = self.points.last() {
            t_ms = t_ms.max(prev.t_ms);
        }
        self.points.push(TouchPoint { x, y, t_ms });
    }

    /// Whether no samples have been pushed yet.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Finish the interaction, classifying it as a tap or a swipe. Returns `None` only if no
    /// samples were captured.
    pub fn finish(self) -> Option<Stroke> {
        let last = *self.points.last()?;
        let tap = Stroke::Tap {
            x: last.x,
            y: last.y,
        };
        if self.points.len() < 2 || arc_length(&self.points) < self.tap_max_path {
            return Some(tap);
        }
        let gesture = Gesture {
            points: self.points,
        };
        // A path long enough to be a swipe but structurally invalid degrades to a tap.
        match gesture.validate() {
            Ok(()) => Some(Stroke::Swipe(gesture)),
            Err(_) => Some(tap),
        }
    }
}

/// Total arc length of a normalized point path.
fn arc_length(points: &[TouchPoint]) -> f32 {
    points
        .windows(2)
        .map(|w| {
            let dx = w[1].x - w[0].x;
            let dy = w[1].y - w[0].y;
            (dx * dx + dy * dy).sqrt()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> KeyArea {
        // 1000x300 px surface key area at the origin: 1 normalized unit == 1000px / 300px.
        KeyArea {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 300.0,
        }
    }

    #[test]
    fn normalize_maps_pixels_into_unit_space() {
        let (x, y) = area().normalize(500.0, 150.0);
        assert!((x - 0.5).abs() < 1e-6);
        assert!((y - 0.5).abs() < 1e-6);
    }

    #[test]
    fn normalize_with_offset_area() {
        let a = KeyArea {
            x: 100.0,
            y: 50.0,
            width: 800.0,
            height: 200.0,
        };
        let (x, y) = a.normalize(500.0, 150.0);
        assert!((x - 0.5).abs() < 1e-6);
        assert!((y - 0.5).abs() < 1e-6);
    }

    #[test]
    fn time_is_rebased_to_zero_and_non_decreasing() {
        let mut b = GestureBuilder::new(area());
        b.push(RawPoint {
            x: 100.0,
            y: 150.0,
            t_ms: 10_000,
        });
        b.push(RawPoint {
            x: 900.0,
            y: 150.0,
            t_ms: 10_050,
        });
        // A clock blip going backwards must not produce decreasing time.
        b.push(RawPoint {
            x: 905.0,
            y: 150.0,
            t_ms: 10_040,
        });
        if let Some(Stroke::Swipe(g)) = b.finish() {
            assert_eq!(g.points[0].t_ms, 0);
            assert_eq!(g.points[1].t_ms, 50);
            assert!(g.points[2].t_ms >= g.points[1].t_ms);
            g.validate().expect("assembled gesture is valid");
        } else {
            panic!("a long horizontal path should classify as a swipe");
        }
    }

    #[test]
    fn short_path_is_a_tap() {
        let mut b = GestureBuilder::new(area());
        // ~10px of jitter around one spot: well under one key.
        b.push(RawPoint {
            x: 300.0,
            y: 150.0,
            t_ms: 0,
        });
        b.push(RawPoint {
            x: 305.0,
            y: 152.0,
            t_ms: 20,
        });
        match b.finish() {
            Some(Stroke::Tap { x, y }) => {
                assert!((x - 0.305).abs() < 1e-3);
                assert!((y - 0.506).abs() < 1e-2);
            }
            other => panic!("expected a tap, got {other:?}"),
        }
    }

    #[test]
    fn single_point_is_a_tap() {
        let mut b = GestureBuilder::new(area());
        b.push(RawPoint {
            x: 300.0,
            y: 150.0,
            t_ms: 5,
        });
        assert!(matches!(b.finish(), Some(Stroke::Tap { .. })));
    }

    #[test]
    fn no_samples_yields_none() {
        assert!(GestureBuilder::new(area()).finish().is_none());
    }

    #[test]
    fn long_path_is_a_swipe() {
        let mut b = GestureBuilder::new(area());
        for i in 0..=10 {
            b.push(RawPoint {
                x: 100.0 + i as f32 * 80.0,
                y: 150.0,
                t_ms: i as u32 * 10,
            });
        }
        assert!(matches!(b.finish(), Some(Stroke::Swipe(_))));
    }
}
