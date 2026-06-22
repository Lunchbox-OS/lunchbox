//! Surface layout (regions + hit-testing) and software rendering into the SHM canvas.
//!
//! The surface is three horizontal bands: a suggestion bar on top, the decoder's letter
//! grid in the middle (this band is the `KeyArea` the core normalizes gestures against), and
//! a function-key row at the bottom.

use shepherd_keyboard_core::{FunctionKey, KeyArea, Layout};

/// A rectangle in surface pixels.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Function-key row, left→right, with relative widths summing to give Space the most room.
const FUNCTION_ROW: &[(FunctionKey, f32)] = &[
    (FunctionKey::Shift, 1.0),
    (FunctionKey::Symbols, 1.0),
    (FunctionKey::Space, 3.0),
    (FunctionKey::Backspace, 1.0),
    (FunctionKey::Enter, 1.0),
];

const SUGGESTION_BAR_FRACTION: f32 = 0.18;
const FUNCTION_ROW_FRACTION: f32 = 0.24;
/// Number of suggestion slots drawn in the bar.
pub const SUGGESTION_SLOTS: usize = 4;

/// What a touch point at a given location targets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    /// A suggestion slot (0-based).
    Suggestion(usize),
    /// A function key.
    Function(FunctionKey),
    /// Somewhere in the letter grid (decode/tap there).
    KeyArea,
}

/// The computed bands for a given surface size.
pub struct Regions {
    width: f32,
    suggestion_bar: Rect,
    key_area: Rect,
    function_row: Rect,
}

impl Regions {
    /// Compute the bands for a `width`×`height` surface.
    pub fn new(width: u32, height: u32) -> Self {
        let w = width as f32;
        let h = height as f32;
        let sug_h = (h * SUGGESTION_BAR_FRACTION).round();
        let fn_h = (h * FUNCTION_ROW_FRACTION).round();
        Self {
            width: w,
            suggestion_bar: Rect {
                x: 0.0,
                y: 0.0,
                w,
                h: sug_h,
            },
            key_area: Rect {
                x: 0.0,
                y: sug_h,
                w,
                h: (h - sug_h - fn_h).max(1.0),
            },
            function_row: Rect {
                x: 0.0,
                y: h - fn_h,
                w,
                h: fn_h,
            },
        }
    }

    /// The letter grid as the core's `KeyArea` (gestures normalize against this).
    pub fn key_area(&self) -> KeyArea {
        KeyArea {
            x: self.key_area.x,
            y: self.key_area.y,
            width: self.key_area.w,
            height: self.key_area.h,
        }
    }

    /// Classify a surface-local point.
    pub fn hit(&self, x: f32, y: f32) -> Hit {
        if self.suggestion_bar.contains(x, y) {
            let slot = ((x / self.width) * SUGGESTION_SLOTS as f32).floor() as usize;
            Hit::Suggestion(slot.min(SUGGESTION_SLOTS - 1))
        } else if self.function_row.contains(x, y) {
            Hit::Function(self.function_at(x))
        } else {
            Hit::KeyArea
        }
    }

    fn function_at(&self, x: f32) -> FunctionKey {
        let total: f32 = FUNCTION_ROW.iter().map(|(_, w)| w).sum();
        let mut cursor = 0.0;
        for (key, weight) in FUNCTION_ROW {
            let w = self.width * weight / total;
            if x < cursor + w {
                return *key;
            }
            cursor += w;
        }
        FunctionKey::Enter
    }

    fn function_rects(&self) -> Vec<(FunctionKey, Rect)> {
        let total: f32 = FUNCTION_ROW.iter().map(|(_, w)| w).sum();
        let mut cursor = 0.0;
        let mut out = Vec::with_capacity(FUNCTION_ROW.len());
        for (key, weight) in FUNCTION_ROW {
            let w = self.width * weight / total;
            out.push((
                *key,
                Rect {
                    x: cursor,
                    y: self.function_row.y,
                    w,
                    h: self.function_row.h,
                },
            ));
            cursor += w;
        }
        out
    }

    /// Pixel rect of a letter key, from its normalized layout center.
    fn letter_rect(&self, layout: &Layout, cx: f32, cy: f32) -> Rect {
        let kw = layout.key_width * self.key_area.w;
        let kh = layout.key_height * self.key_area.h;
        Rect {
            x: self.key_area.x + cx * self.key_area.w - kw / 2.0,
            y: self.key_area.y + cy * self.key_area.h - kh / 2.0,
            w: kw,
            h: kh,
        }
    }
}

/// Colors as little-endian ARGB8888 bytes `[B, G, R, A]`.
mod color {
    pub const BG: [u8; 4] = [0x20, 0x20, 0x20, 0xFF];
    pub const KEY: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xFF];
    pub const KEY_TAP_ONLY: [u8; 4] = [0x2a, 0x2a, 0x2a, 0xFF];
    pub const FUNC: [u8; 4] = [0x48, 0x48, 0x48, 0xFF];
    pub const SUGGESTION: [u8; 4] = [0x30, 0x30, 0x38, 0xFF];
    pub const TEXT: [u8; 4] = [0xf0, 0xf0, 0xf0, 0xFF];
}

/// An optional system font for labels (skipped if none is found).
pub struct Font(Option<fontdue::Font>);

impl Font {
    /// Probe common system font paths; labels are rendered only if one is found.
    pub fn load() -> Self {
        const CANDIDATES: &[&str] = &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
        ];
        for path in CANDIDATES {
            if let Ok(bytes) = std::fs::read(path)
                && let Ok(font) = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            {
                return Font(Some(font));
            }
        }
        tracing::warn!("no system font found; keys will be drawn without labels");
        Font(None)
    }
}

/// A drawable view over the SHM canvas.
struct Canvas<'a> {
    buf: &'a mut [u8],
    width: i32,
    height: i32,
}

impl Canvas<'_> {
    fn fill(&mut self, rect: Rect, c: [u8; 4]) {
        let x0 = rect.x.round().max(0.0) as i32;
        let y0 = rect.y.round().max(0.0) as i32;
        let x1 = ((rect.x + rect.w).round() as i32).min(self.width);
        let y1 = ((rect.y + rect.h).round() as i32).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * self.width + x) * 4) as usize;
                self.buf[i..i + 4].copy_from_slice(&c);
            }
        }
    }

    /// Inset border by filling `rect` with `border` then the interior with `inner`.
    fn key(&mut self, rect: Rect, inner: [u8; 4]) {
        self.fill(rect, color::BG);
        let pad = 2.0;
        self.fill(
            Rect {
                x: rect.x + pad,
                y: rect.y + pad,
                w: (rect.w - 2.0 * pad).max(0.0),
                h: (rect.h - 2.0 * pad).max(0.0),
            },
            inner,
        );
    }

    fn blend_px(&mut self, x: i32, y: i32, c: [u8; 4], coverage: u8) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height || coverage == 0 {
            return;
        }
        let i = ((y * self.width + x) * 4) as usize;
        let a = coverage as u32;
        for (ch, &src) in c.iter().enumerate().take(3) {
            let dst = self.buf[i + ch] as u32;
            self.buf[i + ch] = ((src as u32 * a + dst * (255 - a)) / 255) as u8;
        }
        self.buf[i + 3] = 0xFF;
    }

    /// Draw `text` centered in `rect` at `px` size using `font` (no-op without a font).
    fn text_centered(&mut self, font: &Font, rect: Rect, text: &str, px: f32, c: [u8; 4]) {
        let Some(font) = font.0.as_ref() else {
            return;
        };
        // Measure.
        let glyphs: Vec<_> = text.chars().map(|ch| font.rasterize(ch, px)).collect();
        let total_w: f32 = glyphs.iter().map(|(m, _)| m.advance_width).sum();
        let mut pen_x = rect.x + (rect.w - total_w) / 2.0;
        let baseline = rect.y + (rect.h + px * 0.7) / 2.0;
        for (metrics, bitmap) in &glyphs {
            let gx0 = pen_x + metrics.xmin as f32;
            let gy0 = baseline - (metrics.height as f32 + metrics.ymin as f32);
            for gy in 0..metrics.height {
                for gx in 0..metrics.width {
                    let cov = bitmap[gy * metrics.width + gx];
                    self.blend_px(gx0 as i32 + gx as i32, gy0 as i32 + gy as i32, c, cov);
                }
            }
            pen_x += metrics.advance_width;
        }
    }
}

/// Render the whole keyboard into `buf` (ARGB8888, `width`×`height`).
#[allow(clippy::too_many_arguments)]
pub fn draw(
    buf: &mut [u8],
    width: u32,
    height: u32,
    regions: &Regions,
    layout: &Layout,
    font: &Font,
    suggestions: &[String],
    swipe_enabled: bool,
) {
    let mut canvas = Canvas {
        buf,
        width: width as i32,
        height: height as i32,
    };
    canvas.fill(
        Rect {
            x: 0.0,
            y: 0.0,
            w: width as f32,
            h: height as f32,
        },
        color::BG,
    );

    // Suggestion bar.
    let slot_w = width as f32 / SUGGESTION_SLOTS as f32;
    for slot in 0..SUGGESTION_SLOTS {
        let rect = Rect {
            x: slot as f32 * slot_w,
            y: regions.suggestion_bar.y,
            w: slot_w,
            h: regions.suggestion_bar.h,
        };
        canvas.key(rect, color::SUGGESTION);
        if let Some(word) = suggestions.get(slot) {
            canvas.text_centered(
                font,
                rect,
                word,
                regions.suggestion_bar.h * 0.5,
                color::TEXT,
            );
        }
    }

    // Letter grid.
    let key_color = if swipe_enabled {
        color::KEY
    } else {
        color::KEY_TAP_ONLY
    };
    let label_px = (regions.key_area().height * layout.key_height * 0.5).max(8.0);
    for key in &layout.keys {
        let rect = regions.letter_rect(layout, key.x, key.y);
        canvas.key(rect, key_color);
        canvas.text_centered(font, rect, &key.label.to_string(), label_px, color::TEXT);
    }

    // Function row.
    for (key, rect) in regions.function_rects() {
        canvas.key(rect, color::FUNC);
        let label = function_label(key);
        canvas.text_centered(font, rect, label, rect.h * 0.35, color::TEXT);
    }
}

fn function_label(key: FunctionKey) -> &'static str {
    match key {
        FunctionKey::Shift => "Shift",
        FunctionKey::Symbols => "123",
        FunctionKey::Space => "space",
        FunctionKey::Backspace => "Bksp",
        FunctionKey::Enter => "Enter",
    }
}
