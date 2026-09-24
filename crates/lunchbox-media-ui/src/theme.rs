//! The Lunchbox look, for egui: the "enamel tin" palette, Baloo 2, and the few
//! shapes every media screen is built from.
//!
//! Colours come from `assets/branding/tokens.json` through `lunchbox-branding`,
//! the same crate the launcher and the HUD read them from, so a change to the
//! design file moves the media app with them. What the design file does not
//! decide — the media app's own measurements — lives with the view that uses
//! it; the brief it came from is `docs/ai/history/2026-09-23 001
//! media-app-reskin (#224).md`.
//!
//! Every measurement in this crate is in the design's own pixels, at the
//! 1280×664 field the mockups were drawn on (1280×720 less the HUD), and is
//! multiplied by a [`Scale`] worked out from the space the view is given — the
//! launcher's rule of scaling rather than reflowing.

use egui::{Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Stroke, StrokeKind, vec2};
use lunchbox_branding::tokens;

/// A token's `(r, g, b)` in 0‥1, as `lunchbox-branding` emits it for cairo, in
/// the form egui paints with.
const fn rgb((r, g, b): (f64, f64, f64)) -> Color32 {
    Color32::from_rgb(
        (r * 255.0 + 0.5) as u8,
        (g * 255.0 + 0.5) as u8,
        (b * 255.0 + 0.5) as u8,
    )
}

/// The field: the tin's body.
pub const ENAMEL: Color32 = rgb(tokens::COLOR_ENAMEL_RGB);
/// The watched check.
pub const ENAMEL_DEEP: Color32 = rgb(tokens::COLOR_ENAMEL_DEEP_RGB);
/// A compartment's floor.
pub const COMPARTMENT: Color32 = rgb(tokens::COLOR_COMPARTMENT_RGB);
/// Type on ink.
pub const CREAM: Color32 = rgb(tokens::COLOR_CREAM_RGB);
/// The only outline colour, and type.
pub const INK: Color32 = rgb(tokens::COLOR_INK_RGB);
/// "You can": selection, play, the Continue button.
pub const YELLOW: Color32 = rgb(tokens::COLOR_YELLOW_RGB);
/// "Not yet": the unwatched part of a progress bar, a missing thumbnail.
pub const PUTTY: Color32 = rgb(tokens::COLOR_PUTTY_RGB);
/// Secondary type on a light surface.
pub const MUTED: Color32 = rgb(tokens::COLOR_MUTED_RGB);

/// The field and compartment metrics the media screens share with the
/// launcher, from the design file.
pub const OUTLINE: f32 = tokens::STROKE_OUTLINE as f32;
pub const PILL_STROKE: f32 = tokens::STROKE_PILL as f32;
pub const COMPARTMENT_RADIUS: f32 = tokens::RADIUS_COMPARTMENT as f32;
pub const SELECTION_RADIUS: f32 = tokens::RADIUS_SELECTION as f32;
pub const FIELD_X: f32 = tokens::SPACE_FIELD_X as f32;
pub const FIELD_Y: f32 = tokens::SPACE_FIELD_Y as f32;
/// The field's bottom margin. The design file says it in words ("28 at the
/// bottom", on `space.field-y`) rather than as a token of its own.
pub const FIELD_BOTTOM: f32 = 28.0;
pub const COMPARTMENT_GAP: f32 = tokens::SPACE_COMPARTMENT_GAP as f32;
pub const COMPARTMENT_PAD: f32 = tokens::SPACE_COMPARTMENT_PAD as f32;

/// The canvas the mockups were drawn on: a 1280×720 screen less the 56px HUD
/// they show. Lunchbox's HUD has since come down to 48px (issue #209), and the
/// Android app has none, which is why the views scale to what they are given
/// rather than assuming either.
pub const DESIGN_SIZE: egui::Vec2 = vec2(1280.0, 664.0);

/// How much bigger than the design a view is drawing: the launcher's rule,
/// by the narrower axis, so nothing reflows and nothing is cut off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale(pub f32);

impl Scale {
    /// The scale that fits the design canvas into `size`.
    ///
    /// Floored so a tall, narrow window (a phone held upright) still gets type
    /// that can be read; there the views have more rows than the design's two
    /// rather than smaller ones.
    pub fn fit(size: egui::Vec2) -> Self {
        let s = (size.x / DESIGN_SIZE.x).min(size.y / DESIGN_SIZE.y);
        Self(s.max(0.5))
    }

    /// A design length, in points.
    pub fn px(self, design: f32) -> f32 {
        design * self.0
    }

    /// A design size, in points.
    pub fn vec(self, x: f32, y: f32) -> egui::Vec2 {
        vec2(x, y) * self.0
    }
}

/// The weights the design sets type in. Baloo 2 is a variable font, and egui
/// picks a weight per registered face, so each weight is its own family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// 800: anything the child reads.
    ExtraBold,
    /// 700: secondary lines — durations, times, counts.
    Bold,
}

impl Weight {
    fn family_name(self) -> &'static str {
        match self {
            Weight::ExtraBold => "lunchbox-display-800",
            Weight::Bold => "lunchbox-display-700",
        }
    }

    fn wght(self) -> f32 {
        match self {
            Weight::ExtraBold => 800.0,
            Weight::Bold => 700.0,
        }
    }
}

/// Baloo 2 at `size` points.
///
/// The design sets secondary lines in the system sans at 600–700. egui has no
/// system fonts to reach, and the face it bundles is a light one that reads as
/// a different app next to Baloo 2, so those lines are Baloo 2 at 700 instead.
pub fn font(size: f32, weight: Weight) -> FontId {
    FontId::new(size, FontFamily::Name(weight.family_name().into()))
}

/// Baloo 2, compiled in: the kiosk is offline and Android has no fontconfig.
/// The launcher reaches the same file installed from `assets/fonts`.
const BALOO_2: &[u8] = include_bytes!("../../../assets/fonts/Baloo2-VariableFont_wght.ttf");

/// Register Baloo 2 at the weights [`font`] asks for.
///
/// Each weight falls back to egui's own proportional faces, so a glyph Baloo 2
/// lacks (an ellipsis in some other script, an emoji in a video title) still
/// draws. Takes effect from the next frame, as all of egui's font changes do.
pub fn install_fonts(ctx: &egui::Context) {
    let fallbacks = egui::FontDefinitions::default()
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    // egui's defaults plus ours: neither front-end installs faces of its own,
    // so starting from the defaults loses nothing.
    let mut fonts = egui::FontDefinitions::default();
    for weight in [Weight::ExtraBold, Weight::Bold] {
        let name = weight.family_name().to_string();
        let mut data = egui::FontData::from_static(BALOO_2);
        data.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", weight.wght())]);
        fonts.font_data.insert(name.clone(), data.into());
        let mut family = vec![name];
        family.extend(fallbacks.iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(weight.family_name().into()), family);
    }
    ctx.set_fonts(fonts);
}

/// The dark browse palette the views wore before the branding. Kept only until
/// the last of them has moved over.
pub const BG: Color32 = Color32::from_rgb(0x10, 0x12, 0x18);
pub const TILE: Color32 = Color32::from_rgb(0x1c, 0x20, 0x2c);
pub const TILE_FOCUSED: Color32 = Color32::from_rgb(0x2a, 0x33, 0x4a);
pub const TEXT: Color32 = Color32::from_rgb(0xea, 0xea, 0xea);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
pub const FOCUS_BORDER: Color32 = Color32::from_rgb(0xff, 0xd1, 0x66);

/// Apply the browse theme to a context that shows nothing but the media
/// screens: Baloo 2, and ink type on the enamel field.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.window_fill = ENAMEL;
    style.visuals.panel_fill = ENAMEL;
    ctx.set_global_style(style);
}

/// The field: enamel with the soft vertical sheen the launcher's field has
/// (`color.enamel-sheen`).
pub fn paint_field(painter: &egui::Painter, rect: Rect) {
    let stops = sheen_stops();
    let mut mesh = egui::Mesh::default();
    for (i, &(at, color)) in stops.iter().enumerate() {
        let x = rect.left() + rect.width() * at;
        mesh.colored_vertex(egui::pos2(x, rect.top()), color);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), color);
        if i > 0 {
            let base = (i as u32 - 1) * 2;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 2, base + 3);
        }
    }
    painter.add(mesh);
}

/// `color.enamel-sheen`, a CSS `linear-gradient(90deg, #rrggbb N%, …)`, as
/// `(fraction, colour)` stops. Read from the token rather than restated, so
/// the field keeps matching the launcher's; a token this cannot read falls back
/// to flat enamel.
fn sheen_stops() -> Vec<(f32, Color32)> {
    let sheen = tokens::CSS
        .iter()
        .find(|(name, _)| *name == "color-enamel-sheen")
        .map_or("", |(_, value)| *value);
    let stops: Vec<(f32, Color32)> = sheen
        .split('#')
        .skip(1)
        .filter_map(|stop| {
            let hex = stop.get(..6)?;
            let at = stop[6..]
                .trim()
                .split('%')
                .next()?
                .trim()
                .parse::<f32>()
                .ok()?;
            let n = u32::from_str_radix(hex, 16).ok()?;
            let [_, r, g, b] = n.to_be_bytes();
            Some((at / 100.0, Color32::from_rgb(r, g, b)))
        })
        .collect();
    if stops.len() < 2 {
        return vec![(0.0, ENAMEL), (1.0, ENAMEL)];
    }
    stops
}

/// A compartment: the cream well sunk into the tin, with a 4px ink rim. No
/// drop shadow, ever — it is a well, not a card.
///
/// `shadow.compartment-sunk` is a CSS recipe (four inset shadows and a ring),
/// which egui cannot take as written, so it is drawn by hand: the 3px ring
/// outside at 12% black, then inside the rim the 10px shade under the top lip
/// (10% black), the 4px light line along the bottom (70% white) and the faint
/// 4px sides (5% black). Each inner part is the well's own rounded shape cut
/// down to a band, so it follows the corners rather than crossing them.
pub fn paint_compartment(painter: &egui::Painter, rect: Rect, s: Scale) {
    let radius = s.px(COMPARTMENT_RADIUS);
    let outline = s.px(OUTLINE);
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(s.px(3.0), Color32::from_black_alpha(31)),
        StrokeKind::Outside,
    );
    painter.rect_filled(rect, radius, COMPARTMENT);

    let inner = rect.shrink(outline);
    let inner_radius = (radius - outline).max(0.0);
    let band = |band: Rect, color: Color32| {
        painter
            .with_clip_rect(band.intersect(painter.clip_rect()))
            .rect_filled(inner, inner_radius, color);
    };
    let (lip, line) = (s.px(10.0), s.px(4.0));
    band(
        Rect::from_min_size(inner.min, vec2(inner.width(), lip)),
        Color32::from_black_alpha(26),
    );
    band(
        Rect::from_min_max(egui::pos2(inner.left(), inner.bottom() - line), inner.max),
        Color32::from_white_alpha(178),
    );
    for x in [inner.left(), inner.right() - line] {
        band(
            Rect::from_min_size(egui::pos2(x, inner.top()), vec2(line, inner.height())),
            Color32::from_black_alpha(13),
        );
    }

    painter.rect_stroke(rect, radius, Stroke::new(outline, INK), StrokeKind::Inside);
}

/// The selection fill: yellow behind the focused thing, a 4px ink outline
/// around it, 18px corners — `.lb-item:focus-visible`.
pub fn paint_selection(painter: &egui::Painter, rect: Rect, s: Scale) {
    let radius = s.px(SELECTION_RADIUS);
    painter.rect_filled(rect, radius, YELLOW);
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(s.px(OUTLINE), INK),
        StrokeKind::Outside,
    );
}

/// A progress bar: an ink-rimmed putty track with the watched fraction in
/// yellow. `leading_edge` draws the ink line the player's track has where the
/// fill ends.
pub fn paint_progress_bar(
    painter: &egui::Painter,
    rect: Rect,
    fraction: f32,
    border: f32,
    leading_edge: bool,
) {
    let radius = CornerRadius::same((rect.height() / 2.0).round().min(255.0) as u8);
    painter.rect_filled(rect, radius, PUTTY);
    let inner = rect.shrink(border);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let fill = Rect::from_min_size(inner.min, vec2(inner.width() * fraction, inner.height()));
        let clip = painter.with_clip_rect(fill.intersect(painter.clip_rect()));
        clip.rect_filled(inner, radius, YELLOW);
        if leading_edge && fraction < 1.0 {
            painter.line_segment(
                [
                    egui::pos2(fill.right(), inner.top()),
                    egui::pos2(fill.right(), inner.bottom()),
                ],
                Stroke::new(border, INK),
            );
        }
    }
    painter.rect_stroke(rect, radius, Stroke::new(border, INK), StrokeKind::Inside);
}

/// The glyphs the media screens draw. Painted as shapes rather than set as
/// text: egui's bundled fonts have no media symbols of their own, and a glyph
/// drawn at its box's size stays centred at every scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Play,
    Pause,
    Back,
    Rewind,
    Forward,
    ChevronRight,
    ChevronLeft,
    Check,
}

/// Paint `glyph` to fill the square `rect` in `color`.
pub fn paint_glyph(painter: &egui::Painter, rect: Rect, glyph: Glyph, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    let at = |x: f32, y: f32| Pos2::new(c.x + x * r, c.y + y * r);
    let line = Stroke::new(r * 0.32, color);
    let triangle = |points: Vec<Pos2>| {
        painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    };
    match glyph {
        Glyph::Play => triangle(vec![at(-0.62, -0.8), at(0.86, 0.0), at(-0.62, 0.8)]),
        Glyph::Pause => {
            for x in [-0.55, 0.2] {
                painter.rect_filled(
                    Rect::from_min_max(at(x, -0.75), at(x + 0.35, 0.75)),
                    r * 0.08,
                    color,
                );
            }
        }
        Glyph::Rewind => {
            triangle(vec![at(0.0, -0.62), at(0.0, 0.62), at(-0.9, 0.0)]);
            triangle(vec![at(0.9, -0.62), at(0.9, 0.62), at(0.0, 0.0)]);
        }
        Glyph::Forward => {
            triangle(vec![at(0.0, -0.62), at(0.0, 0.62), at(0.9, 0.0)]);
            triangle(vec![at(-0.9, -0.62), at(-0.9, 0.62), at(0.0, 0.0)]);
        }
        Glyph::Back | Glyph::ChevronLeft => {
            painter.add(egui::Shape::line(
                vec![at(0.3, -0.65), at(-0.35, 0.0), at(0.3, 0.65)],
                line,
            ));
        }
        Glyph::ChevronRight => {
            painter.add(egui::Shape::line(
                vec![at(-0.3, -0.65), at(0.35, 0.0), at(-0.3, 0.65)],
                line,
            ));
        }
        Glyph::Check => {
            painter.add(egui::Shape::line(
                vec![at(-0.6, 0.05), at(-0.15, 0.5), at(0.65, -0.45)],
                line,
            ));
        }
    }
}

/// A round button: a disc with a 4px ink rim and a glyph in it. `focused`
/// gives it the selection look — yellow, lifted 2px, with the 5×6 ink offset
/// shadow of a pressed launcher item.
pub fn paint_disc(
    painter: &egui::Painter,
    rect: Rect,
    glyph: Glyph,
    fill: Color32,
    focused: bool,
    s: Scale,
) {
    let r = rect.width().min(rect.height()) / 2.0;
    let (center, fill) = if focused {
        let lifted = rect.center() - vec2(0.0, s.px(2.0));
        painter.circle_filled(lifted + s.vec(5.0, 6.0), r, INK);
        (lifted, YELLOW)
    } else {
        (rect.center(), fill)
    };
    painter.circle_filled(center, r, fill);
    painter.circle_stroke(
        center,
        r - s.px(OUTLINE) / 2.0,
        Stroke::new(s.px(OUTLINE), INK),
    );
    let glyph_rect = Rect::from_center_size(center, egui::Vec2::splat(r * 0.8));
    paint_glyph(painter, glyph_rect, glyph, INK);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_is_the_design_files() {
        // Spot-check the conversion against the hex the design file states.
        assert_eq!(ENAMEL, Color32::from_rgb(0x2f, 0xb5, 0xa5));
        assert_eq!(INK, Color32::from_rgb(0x1c, 0x1b, 0x18));
        assert_eq!(YELLOW, Color32::from_rgb(0xff, 0xd1, 0x66));
    }

    #[test]
    fn the_sheen_is_read_from_its_token() {
        let stops = sheen_stops();
        assert_eq!(stops.len(), 5, "{stops:?}");
        assert_eq!(stops[0], (0.0, Color32::from_rgb(0x27, 0xa0, 0x93)));
        assert_eq!(stops[2].0, 0.5);
        assert_eq!(stops[4].0, 1.0);
    }

    #[test]
    fn scale_follows_the_narrower_axis() {
        assert_eq!(Scale::fit(DESIGN_SIZE), Scale(1.0));
        // 1920×1080 less a 48px HUD at scale 1.5: height-limited by a hair.
        let s = Scale::fit(vec2(1920.0, 1032.0));
        assert!((s.0 - 1.5).abs() < 0.06, "{s:?}");
        // A phone held upright stops shrinking at the floor.
        assert_eq!(Scale::fit(vec2(400.0, 850.0)), Scale(0.5));
    }
}
