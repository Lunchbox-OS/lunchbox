//! The library screen — the media app's icon at screen size: one sunk
//! compartment on the enamel field, holding the library's thumbnails, and,
//! when something was left half-watched, a "keep watching" row above it.
//!
//! Both front-ends draw it: the Linux binary over the library lunchboxd passed
//! in, the Android app over the library picked in its switcher. It is
//! platform-agnostic — the caller supplies the items, their posters and how
//! much of each has been watched, and gets back the id of an item to play. The
//! view owns its focus and its horizontal scroll; a caller translates its own
//! gamepad into [`Nav`] and [`LibraryView::activate`], and the keyboard (which
//! is what a TV remote's D-pad arrives as) is read here.
//!
//! The layout is the design's (see [`crate::theme`] for where it came from):
//! thumbnails in rows that fill column by column and spill to the right, so the
//! library scrolls sideways and never down.

use egui::{Align, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lunchbox_media_core::{Item, PlatformInfo, resolve_source};

use crate::theme::{self, Glyph, Scale, Weight};
use crate::video::format_time;

// The media app's own measurements (MEDIA.md §3–4), in design pixels.

/// Thumbnail width in the library; 16:9.
const THUMB_W: f32 = 340.0;
/// Thumbnail width when the library shares the screen with the "keep
/// watching" row and drops to one row.
const THUMB_W_BELOW_HERO: f32 = 280.0;
/// The thumbnail in the "keep watching" row.
const HERO_THUMB_W: f32 = 372.0;
const THUMB_RADIUS: f32 = 12.0;
const HERO_THUMB_RADIUS: f32 = 14.0;
/// Space between a tile's selection edge and its thumbnail, and between the
/// thumbnail and the title.
const TILE_PAD: f32 = 8.0;
const COLUMN_GAP: f32 = 12.0;
const ROW_GAP: f32 = 10.0;
const TITLE_SIZE: f32 = 16.0;
const TITLE_LINE_HEIGHT: f32 = 1.1;
const TITLE_LINES: usize = 2;
/// A library smaller than this is one row of full-size tiles, not two short
/// ones: the design does not scale tiles up to fill the space.
const TWO_ROWS_FROM: usize = 6;
/// Compartment padding above the header and below the grid; the sides use the
/// launcher's `space.compartment-pad`.
const PAD_TOP: f32 = 14.0;
const PAD_BOTTOM: f32 = 12.0;
const HEADER_NAME_SIZE: f32 = 22.0;
const HEADER_COUNT_SIZE: f32 = 13.0;
/// Header line to the first row: the header's 6px padding and the
/// compartment's 10px gap.
const HEADER_GAP: f32 = 16.0;
/// Between the "keep watching" row and the library below it.
const HERO_GAP: f32 = 22.0;
const HERO_ART_PAD: f32 = 16.0;
const CHIP_SIZE: f32 = 12.0;
const CHIP_INSET: f32 = 8.0;
/// The progress strip along a partly watched thumbnail's bottom edge, and the
/// ink rule on top of it.
const STRIP_H: f32 = 8.0;
const STRIP_RULE: f32 = 3.0;
/// The overflow fade and its yellow chevron chip.
const FADE_W: f32 = 90.0;
const MORE_SIZE: f32 = 44.0;
const MORE_INSET: f32 = 22.0;
/// A placeholder thumbnail's art, centred on putty.
const PLACEHOLDER_SIZE: f32 = 96.0;

/// Drawn on a thumbnail with no poster (`icon/placeholders/video.svg`, as
/// rendered in the design hand-off).
const PLACEHOLDER_PNG: &[u8] =
    include_bytes!("../../../assets/branding/icon/placeholders/video-256.png");

/// A direction on the D-pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
}

/// The item the library should offer to continue: the one watched most
/// recently, which has a saved position.
pub struct Hero<'a> {
    pub item: &'a Item,
    /// Where it was left off, in seconds.
    pub position: f64,
    /// Its length, when known.
    pub duration: Option<f64>,
}

/// What the library shows this frame.
pub struct Library<'a> {
    /// The library's name, as whoever launched it calls it.
    pub title: &'a str,
    /// The items to show, in order.
    pub items: &'a [Item],
    /// The "keep watching" row, if there is something to continue. Its item is
    /// left out of the thumbnails below it.
    pub hero: Option<Hero<'a>>,
    /// Encoded poster bytes for an item id, once fetched.
    pub poster: &'a dyn Fn(&str) -> Option<Vec<u8>>,
    /// How much of an item has been watched, 0–1, when it has been started and
    /// not finished.
    pub progress: &'a dyn Fn(&Item) -> Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// The "keep watching" row's Continue button.
    Continue,
    /// A thumbnail, by its index among the thumbnails shown.
    Tile(usize),
}

/// How the thumbnails were laid out last frame: what [`LibraryView::navigate`]
/// moves the focus through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Grid {
    tiles: usize,
    rows: usize,
    hero: bool,
}

/// The library screen's state: focus and scroll, carried across frames.
pub struct LibraryView {
    focus: Focus,
    grid: Grid,
    /// The focus the scroll last followed; the scroll only chases the focus
    /// when it moves, so a finger can browse without being pulled back.
    followed: Option<Focus>,
    activate: bool,
    scroll: Scroll,
    /// What the focus was on when the last frame was drawn.
    focused_item: Option<String>,
}

impl Default for LibraryView {
    fn default() -> Self {
        Self {
            // Continue if there turns out to be one (the design has it focused
            // first); `draw` falls back to the first thumbnail otherwise.
            focus: Focus::Continue,
            grid: Grid {
                tiles: 0,
                rows: 1,
                hero: false,
            },
            followed: None,
            activate: false,
            scroll: Scroll::default(),
            focused_item: None,
        }
    }
}

impl LibraryView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the focus a step, as the last frame laid the library out.
    ///
    /// Left and right cross columns and stop at the ends; up and down move
    /// within a column and do not wrap. Up from the top row reaches Continue,
    /// when there is one, and down from Continue comes back.
    pub fn navigate(&mut self, nav: Nav) {
        self.focus = step(self.focus, nav, self.grid);
    }

    /// Choose the focused thing. Takes effect in the next [`draw`](Self::draw),
    /// which returns what was chosen.
    pub fn activate(&mut self) {
        self.activate = true;
    }

    /// The id of the item that has the focus — a thumbnail, or the one
    /// Continue would resume — as of the last [`draw`](Self::draw), for a
    /// caller warming the item most likely to be played next.
    pub fn focused_item(&self) -> Option<&str> {
        self.focused_item.as_deref()
    }

    /// Draw the library over the space left in `ui` and return the id of an
    /// item to play: a thumbnail tapped or chosen, or Continue.
    ///
    /// Reads the arrow keys, Enter and Space itself. A caller that also has a
    /// gamepad calls [`navigate`](Self::navigate) and
    /// [`activate`](Self::activate) before this, in the same frame.
    pub fn draw(&mut self, ui: &mut egui::Ui, library: &Library<'_>) -> Option<String> {
        let rect = ui.available_rect_before_wrap();
        let s = Scale::fit(rect.size());
        let info = PlatformInfo::current();
        let tiles: Vec<&Item> = library
            .items
            .iter()
            .filter(|item| library.hero.as_ref().is_none_or(|h| h.item.id != item.id))
            .collect();

        ui.input(|i| {
            for (key, nav) in [
                (egui::Key::ArrowLeft, Nav::Left),
                (egui::Key::ArrowRight, Nav::Right),
                (egui::Key::ArrowUp, Nav::Up),
                (egui::Key::ArrowDown, Nav::Down),
            ] {
                if i.key_pressed(key) {
                    self.focus = step(self.focus, nav, self.grid);
                }
            }
            if i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space) {
                self.activate = true;
            }
        });

        let layout = Layout::new(rect, s, tiles.len(), library.hero.is_some());
        self.grid = Grid {
            tiles: tiles.len(),
            rows: layout.rows,
            hero: library.hero.is_some(),
        };
        self.focus = clamp_focus(self.focus, self.grid);

        let painter = ui.painter_at(rect);
        theme::paint_field(&painter, rect);

        let mut chosen: Option<String> = None;

        if let Some(hero) = &library.hero {
            let (continue_rect, art_rect) = self.draw_hero(ui, &layout, hero, library, s);
            for r in [continue_rect, art_rect] {
                let response = ui.interact(
                    r,
                    ui.id().with(("continue", r.min.x as i32)),
                    Sense::click(),
                );
                if pointer_moved(ui) && response.hovered() {
                    self.focus = Focus::Continue;
                }
                if response.clicked() {
                    self.focus = Focus::Continue;
                    chosen = Some(hero.item.id.clone());
                }
            }
        }

        // The scroll, then everything that moves with it. Scrolling is only
        // ever sideways; the drag covers the whole library compartment and
        // coexists with the thumbnails' taps.
        let max_scroll = layout.max_scroll();
        let drag = ui.interact(layout.library, ui.id().with("library-drag"), Sense::drag());
        let (dt, now) = ui.input(|i| (i.stable_dt.min(0.1), i.time));
        let wheel = if drag.hovered() {
            ui.input(|i| i.smooth_scroll_delta.x + i.smooth_scroll_delta.y)
        } else {
            0.0
        };
        if self.followed != Some(self.focus) {
            self.followed = Some(self.focus);
            if let Focus::Tile(i) = self.focus {
                self.scroll
                    .follow(layout.tile_rect(i, 0.0), &layout, max_scroll);
            }
        }
        let offset = self
            .scroll
            .update(&drag, wheel, dt, now, max_scroll, ui.ctx());

        let field = ui.new_child(egui::UiBuilder::new().max_rect(layout.field));
        let clip = painter.with_clip_rect(layout.field.intersect(rect));
        theme::paint_compartment(&clip, layout.compartment(offset), s);
        self.draw_header(
            &clip,
            &layout,
            offset,
            library.title,
            library.items.len(),
            s,
        );

        for (i, item) in tiles.iter().enumerate() {
            let tile = layout.tile_rect(i, offset);
            if !tile.intersects(layout.field) {
                continue;
            }
            let playable = resolve_source(item, &info).is_some();
            let response = field.interact(tile, field.id().with(("tile", i)), Sense::click());
            if pointer_moved(ui) && response.hovered() {
                self.focus = Focus::Tile(i);
            }
            let focused = self.focus == Focus::Tile(i);
            paint_tile(
                &field,
                &clip,
                tile,
                layout.thumb_w,
                item,
                (library.poster)(&item.id),
                (library.progress)(item),
                focused,
                playable,
                s,
            );
            if response.clicked() && playable {
                self.focus = Focus::Tile(i);
                chosen = Some(item.id.clone());
            }
        }

        // The overflow cues, on whichever sides have more to show.
        let page = layout.field.width() * 0.75;
        if offset < max_scroll - 0.5 && self.overflow_cue(ui, &clip, &layout, false, s) {
            self.scroll.jump_to((offset + page).min(max_scroll));
        }
        if offset > 0.5 && self.overflow_cue(ui, &clip, &layout, true, s) {
            self.scroll.jump_to((offset - page).max(0.0));
        }

        self.focused_item = match self.focus {
            Focus::Continue => library.hero.as_ref().map(|h| h.item.id.clone()),
            Focus::Tile(i) => tiles.get(i).map(|item| item.id.clone()),
        };
        if std::mem::take(&mut self.activate) {
            match self.focus {
                Focus::Continue => chosen = library.hero.as_ref().map(|h| h.item.id.clone()),
                Focus::Tile(i) => {
                    if let Some(item) = tiles.get(i)
                        && resolve_source(item, &info).is_some()
                    {
                        chosen = Some(item.id.clone());
                    }
                }
            }
        }
        chosen
    }

    /// The "keep watching" row: the thumbnail in a compartment of its own, and
    /// beside it the title, how far through it is, and Continue. Returns the
    /// Continue button's and the thumbnail's rectangles, which both continue.
    fn draw_hero(
        &self,
        ui: &egui::Ui,
        layout: &Layout,
        hero: &Hero<'_>,
        library: &Library<'_>,
        s: Scale,
    ) -> (Rect, Rect) {
        let painter = ui.painter_at(layout.field);
        let row = layout.hero.expect("draw_hero without a hero row");

        let art = Rect::from_min_size(
            row.min,
            vec2(
                s.px(HERO_THUMB_W + 2.0 * (HERO_ART_PAD + theme::OUTLINE)),
                row.height(),
            ),
        );
        theme::paint_compartment(&painter, art, s);
        let thumb = Rect::from_min_size(
            art.min
                + vec2(
                    s.px(HERO_ART_PAD + theme::OUTLINE),
                    s.px(HERO_ART_PAD + theme::OUTLINE),
                ),
            s.vec(HERO_THUMB_W, HERO_THUMB_W * 9.0 / 16.0),
        );
        let fraction = watched_fraction(hero.position, hero.duration);
        paint_thumb(
            ui,
            &painter,
            thumb,
            s.px(HERO_THUMB_RADIUS),
            hero.item,
            (library.poster)(&hero.item.id),
            fraction,
            s,
        );

        let info = Rect::from_min_max(
            pos2(art.right() + s.px(theme::COMPARTMENT_GAP), row.top()),
            row.max,
        );
        theme::paint_compartment(&painter, info, s);
        let inner = info.shrink2(s.vec(24.0 + theme::OUTLINE, 20.0 + theme::OUTLINE));

        // Stacked from the top, then centred as a block.
        let kicker = painter.layout_job(egui::text::LayoutJob::single_section(
            "KEEP WATCHING".to_string(),
            egui::TextFormat {
                font_id: theme::font(s.px(13.0), Weight::Bold),
                color: theme::MUTED,
                extra_letter_spacing: s.px(13.0 * 0.08),
                ..Default::default()
            },
        ));
        let title = painter.layout(
            hero.item.title.clone(),
            theme::font(s.px(34.0), Weight::ExtraBold),
            theme::INK,
            inner.width(),
        );
        let left = remaining_line(hero.position, hero.duration);
        let left = painter.layout_no_wrap(
            left.unwrap_or_default(),
            theme::font(s.px(16.0), Weight::Bold),
            theme::MUTED,
        );
        let button_h = s.px(58.0);
        let gap = s.px(10.0);
        let bar_h = s.px(14.0);
        let block = kicker.size().y
            + gap * 0.5
            + title.size().y.min(s.px(76.0))
            + gap
            + left.size().y.max(bar_h)
            + gap
            + s.px(6.0)
            + button_h;
        let mut y = inner.center().y - block / 2.0;

        painter.galley(pos2(inner.left(), y), kicker.clone(), theme::MUTED);
        y += kicker.size().y + gap * 0.5;
        painter
            .with_clip_rect(Rect::from_min_size(
                pos2(inner.left(), y),
                vec2(inner.width(), s.px(76.0)),
            ))
            .galley(pos2(inner.left(), y), title.clone(), theme::INK);
        y += title.size().y.min(s.px(76.0)) + gap;

        let line_h = left.size().y.max(bar_h);
        let bar = Rect::from_min_size(
            pos2(inner.left(), y + (line_h - bar_h) / 2.0),
            vec2(s.px(260.0), bar_h),
        );
        theme::paint_progress_bar(
            &painter,
            bar,
            fraction.unwrap_or(0.0),
            s.px(theme::PILL_STROKE),
            false,
        );
        painter.galley(
            pos2(bar.right() + s.px(14.0), y + (line_h - left.size().y) / 2.0),
            left,
            theme::MUTED,
        );
        y += line_h + gap + s.px(6.0);

        // Continue: the yellow pill, focused first.
        let label = painter.layout_no_wrap(
            "Continue".to_string(),
            theme::font(s.px(24.0), Weight::ExtraBold),
            theme::INK,
        );
        let from = painter.layout_no_wrap(
            format!("from {}", format_time(hero.position)),
            theme::font(s.px(14.0), Weight::Bold),
            theme::MUTED,
        );
        let glyph = s.px(20.0);
        let width = s.px(16.0)
            + glyph
            + s.px(12.0)
            + label.size().x
            + s.px(12.0)
            + from.size().x
            + s.px(26.0);
        let button = Rect::from_min_size(pos2(inner.left(), y), vec2(width, button_h));
        let focused = self.focus == Focus::Continue;
        let (button, shadow) = if focused {
            (button.translate(vec2(0.0, -s.px(2.0))), s.vec(6.0, 7.0))
        } else {
            (button, s.vec(4.0, 5.0))
        };
        let pill = CornerRadius::same((button_h / 2.0).min(255.0) as u8);
        painter.rect_filled(button.translate(shadow), pill, theme::INK);
        painter.rect_filled(button, pill, theme::YELLOW);
        painter.rect_stroke(
            button,
            pill,
            Stroke::new(s.px(theme::OUTLINE), theme::INK),
            StrokeKind::Inside,
        );
        let mut x = button.left() + s.px(16.0);
        theme::paint_glyph(
            &painter,
            Rect::from_center_size(
                pos2(x + glyph / 2.0, button.center().y),
                egui::Vec2::splat(glyph),
            ),
            Glyph::Play,
            theme::INK,
        );
        x += glyph + s.px(12.0);
        let label_w = label.size().x;
        painter.galley(
            pos2(x, button.center().y - label.size().y / 2.0),
            label,
            theme::INK,
        );
        x += label_w + s.px(12.0);
        painter.galley(
            pos2(x, button.center().y - from.size().y / 2.0),
            from,
            theme::MUTED,
        );

        (button, thumb)
    }

    /// The library's name, and how many videos it holds.
    fn draw_header(
        &self,
        painter: &egui::Painter,
        layout: &Layout,
        offset: f32,
        title: &str,
        count: usize,
        s: Scale,
    ) {
        let origin = layout.header_origin(offset);
        let name = painter.layout_no_wrap(
            title.to_string(),
            theme::font(s.px(HEADER_NAME_SIZE), Weight::ExtraBold),
            theme::INK,
        );
        let line = layout.header_h;
        let name_w = name.size().x;
        painter.galley(
            pos2(origin.x, origin.y + (line - name.size().y) / 2.0),
            name,
            theme::INK,
        );
        let count = painter.layout_no_wrap(
            count_line(count),
            theme::font(s.px(HEADER_COUNT_SIZE), Weight::Bold),
            theme::MUTED,
        );
        // Sat on the name's baseline rather than centred on it, as the mockup
        // sets it.
        let baseline = origin.y + (line + s.px(HEADER_NAME_SIZE) * 0.62) / 2.0;
        painter.galley(
            pos2(
                origin.x + name_w + s.px(12.0),
                baseline - count.size().y * 0.72,
            ),
            count,
            theme::MUTED,
        );
    }

    /// The fade and the yellow chevron chip at one edge of an overflowing
    /// library. Returns whether the chip was tapped.
    fn overflow_cue(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        layout: &Layout,
        left: bool,
        s: Scale,
    ) -> bool {
        let field = layout.field;
        let band = layout.library;
        let fade_w = s.px(FADE_W);
        let fade = if left {
            Rect::from_min_max(
                pos2(field.left(), band.top()),
                pos2(field.left() + fade_w, band.bottom()),
            )
        } else {
            Rect::from_min_max(
                pos2(field.right() - fade_w, band.top()),
                pos2(field.right(), band.bottom()),
            )
        };
        let clear = Color32::from_rgba_unmultiplied(
            theme::ENAMEL.r(),
            theme::ENAMEL.g(),
            theme::ENAMEL.b(),
            0,
        );
        let solid = Color32::from_rgba_unmultiplied(
            theme::ENAMEL.r(),
            theme::ENAMEL.g(),
            theme::ENAMEL.b(),
            242,
        );
        let (from, to) = if left { (solid, clear) } else { (clear, solid) };
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(fade.left_top(), from);
        mesh.colored_vertex(fade.left_bottom(), from);
        mesh.colored_vertex(fade.right_top(), to);
        mesh.colored_vertex(fade.right_bottom(), to);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 2, 3);
        painter.add(mesh);

        let size = s.px(MORE_SIZE);
        let center = if left {
            pos2(
                field.left() + s.px(MORE_INSET) + size / 2.0,
                band.center().y,
            )
        } else {
            pos2(
                field.right() - s.px(MORE_INSET) - size / 2.0,
                band.center().y,
            )
        };
        let chip = Rect::from_center_size(center, egui::Vec2::splat(size));
        let r = size / 2.0;
        painter.circle_filled(center + s.vec(3.0, 3.0), r, theme::INK);
        painter.circle_filled(center, r, theme::YELLOW);
        painter.circle_stroke(
            center,
            r - s.px(theme::OUTLINE) / 2.0,
            Stroke::new(s.px(theme::OUTLINE), theme::INK),
        );
        theme::paint_glyph(
            painter,
            Rect::from_center_size(center, egui::Vec2::splat(size * 0.36)),
            if left {
                Glyph::ChevronLeft
            } else {
                Glyph::ChevronRight
            },
            theme::INK,
        );
        ui.interact(chip, ui.id().with(("more", left)), Sense::click())
            .clicked()
    }
}

/// Where everything goes, in points, for one frame.
struct Layout {
    s: Scale,
    field: Rect,
    /// The "keep watching" row, when there is one.
    hero: Option<Rect>,
    /// The band the library compartment occupies, unscrolled and as wide as
    /// the field.
    library: Rect,
    /// The compartment's width: its content's, or the field's less its
    /// margins, whichever is wider.
    compartment_w: f32,
    header_h: f32,
    grid_top: f32,
    rows: usize,
    thumb_w: f32,
    tile: egui::Vec2,
}

impl Layout {
    fn new(field: Rect, s: Scale, tiles: usize, hero: bool) -> Self {
        let outline = s.px(theme::OUTLINE);
        let top = field.top() + s.px(theme::FIELD_Y);
        let left = field.left() + s.px(theme::FIELD_X);
        let right = field.right() - s.px(theme::FIELD_X);
        let hero_rect = hero.then(|| {
            let h = s.px(HERO_THUMB_W * 9.0 / 16.0 + 2.0 * (HERO_ART_PAD + theme::OUTLINE));
            Rect::from_min_max(pos2(left, top), pos2(right, top + h))
        });
        let library_top = hero_rect.map_or(top, |r| r.bottom() + s.px(HERO_GAP));
        let library = Rect::from_min_max(
            pos2(left, library_top),
            pos2(right, field.bottom() - s.px(theme::FIELD_BOTTOM)),
        );

        let thumb_w = s.px(if hero { THUMB_W_BELOW_HERO } else { THUMB_W });
        let title_h = s.px(TITLE_SIZE * TITLE_LINE_HEIGHT) * TITLE_LINES as f32;
        let tile = vec2(
            thumb_w + s.px(2.0 * TILE_PAD),
            s.px(TILE_PAD) + thumb_w * 9.0 / 16.0 + s.px(TILE_PAD) + title_h + s.px(TILE_PAD),
        );
        let header_h = s.px(HEADER_NAME_SIZE);
        let grid_top = library.top() + outline + s.px(PAD_TOP) + header_h + s.px(HEADER_GAP);
        let grid_h = library.bottom() - outline - s.px(PAD_BOTTOM) - grid_top;
        let fit = (((grid_h + s.px(ROW_GAP)) / (tile.y + s.px(ROW_GAP))).floor() as usize).max(1);
        let rows = if hero || tiles < TWO_ROWS_FROM {
            1
        } else {
            fit
        };

        let columns = tiles.div_ceil(rows).max(1) as f32;
        let content_w = columns * tile.x + (columns - 1.0) * s.px(COLUMN_GAP);
        let compartment_w =
            (content_w + 2.0 * (s.px(theme::COMPARTMENT_PAD) + outline)).max(library.width());

        Self {
            s,
            field,
            hero: hero_rect,
            library,
            compartment_w,
            header_h,
            grid_top,
            rows,
            thumb_w,
            tile,
        }
    }

    /// How far the library can scroll: until its right edge sits a field
    /// margin in from the field's.
    fn max_scroll(&self) -> f32 {
        (self.library.left() + self.compartment_w + self.s.px(theme::FIELD_X) - self.field.right())
            .max(0.0)
    }

    fn compartment(&self, offset: f32) -> Rect {
        Rect::from_min_size(
            pos2(self.library.left() - offset, self.library.top()),
            vec2(self.compartment_w, self.library.height()),
        )
    }

    /// The top-left of the header line.
    fn header_origin(&self, offset: f32) -> egui::Pos2 {
        let s = self.s;
        pos2(
            self.library.left() - offset + s.px(theme::OUTLINE + theme::COMPARTMENT_PAD + 2.0),
            self.library.top() + s.px(theme::OUTLINE + PAD_TOP),
        )
    }

    /// The `i`th thumbnail's cell, selection edge to selection edge: rows fill
    /// down each column before the next column starts.
    fn tile_rect(&self, i: usize, offset: f32) -> Rect {
        let s = self.s;
        let (column, row) = (i / self.rows, i % self.rows);
        let x = self.library.left() - offset
            + s.px(theme::OUTLINE + theme::COMPARTMENT_PAD)
            + column as f32 * (self.tile.x + s.px(COLUMN_GAP));
        let y = self.grid_top + row as f32 * (self.tile.y + s.px(ROW_GAP));
        Rect::from_min_size(pos2(x, y), self.tile)
    }
}

/// One step of the D-pad through a column-filled grid.
fn step(focus: Focus, nav: Nav, grid: Grid) -> Focus {
    let Grid { tiles, rows, hero } = grid;
    let rows = rows.max(1);
    match focus {
        Focus::Continue => match nav {
            Nav::Down if tiles > 0 => Focus::Tile(0),
            _ => Focus::Continue,
        },
        Focus::Tile(i) => {
            let row = i % rows;
            match nav {
                Nav::Left => Focus::Tile(i.checked_sub(rows).unwrap_or(i)),
                Nav::Right => {
                    if i + rows < tiles {
                        Focus::Tile(i + rows)
                    } else if (tiles.saturating_sub(1)) / rows > i / rows {
                        // The last column is short of this row: land on its
                        // last thumbnail rather than refusing to move.
                        Focus::Tile(tiles - 1)
                    } else {
                        Focus::Tile(i)
                    }
                }
                Nav::Up if row > 0 => Focus::Tile(i - 1),
                Nav::Up if hero => Focus::Continue,
                Nav::Down if row + 1 < rows && i + 1 < tiles => Focus::Tile(i + 1),
                _ => Focus::Tile(i),
            }
        }
    }
}

/// Keep the focus on something that is there: Continue only with a hero row,
/// a thumbnail only within the thumbnails.
fn clamp_focus(focus: Focus, grid: Grid) -> Focus {
    match focus {
        Focus::Continue if grid.hero => Focus::Continue,
        Focus::Continue => Focus::Tile(0),
        Focus::Tile(_) if grid.tiles == 0 && grid.hero => Focus::Continue,
        Focus::Tile(i) => Focus::Tile(i.min(grid.tiles.saturating_sub(1))),
    }
}

/// Whether the pointer moved this frame. Hover moves the focus only then, so a
/// pointer resting over a thumbnail does not fight the D-pad.
fn pointer_moved(ui: &egui::Ui) -> bool {
    ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
}

/// One thumbnail cell: the selection fill when focused, the thumbnail, the
/// title under it.
#[allow(clippy::too_many_arguments)]
fn paint_tile(
    ui: &egui::Ui,
    painter: &egui::Painter,
    cell: Rect,
    thumb_w: f32,
    item: &Item,
    poster: Option<Vec<u8>>,
    progress: Option<f32>,
    focused: bool,
    playable: bool,
    s: Scale,
) {
    if focused {
        theme::paint_selection(painter, cell, s);
    }
    let thumb = Rect::from_min_size(
        cell.min + s.vec(TILE_PAD, TILE_PAD),
        vec2(thumb_w, thumb_w * 9.0 / 16.0),
    );
    paint_thumb(
        ui,
        painter,
        thumb,
        s.px(THUMB_RADIUS),
        item,
        poster,
        progress,
        s,
    );

    let font = theme::font(s.px(TITLE_SIZE), Weight::ExtraBold);
    let mut job = egui::text::LayoutJob::single_section(
        item.title.clone(),
        egui::TextFormat {
            font_id: font,
            color: theme::INK,
            line_height: Some(s.px(TITLE_SIZE * TITLE_LINE_HEIGHT)),
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: thumb_w,
        max_rows: TITLE_LINES,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    job.halign = Align::Center;
    let galley = painter.layout_job(job);
    let top = thumb.bottom() + s.px(TILE_PAD);
    let pos = pos2(
        cell.center().x - galley.rect.center().x,
        top - galley.rect.top(),
    );
    painter.galley(pos, galley, theme::INK);

    if !playable {
        // Not playable here: at the design's 50% for anything locked.
        painter.rect_filled(
            cell,
            s.px(theme::SELECTION_RADIUS),
            theme::COMPARTMENT.gamma_multiply(0.5),
        );
    }
}

/// A 16:9 thumbnail: the poster cropped to fill it (or the placeholder on
/// putty), the duration chip, how much has been watched, and the 4px ink
/// border.
#[allow(clippy::too_many_arguments)]
fn paint_thumb(
    ui: &egui::Ui,
    painter: &egui::Painter,
    thumb: Rect,
    radius: f32,
    item: &Item,
    poster: Option<Vec<u8>>,
    progress: Option<f32>,
    s: Scale,
) {
    let border = s.px(theme::OUTLINE);
    let inner = thumb.shrink(border);
    let inner_radius = CornerRadius::same((radius - border).max(0.0) as u8);
    painter.rect_filled(thumb, radius, theme::PUTTY);

    let image =
        poster.map(|bytes| egui::Image::from_bytes(format!("bytes://poster-{}", item.id), bytes));
    let loaded = image.and_then(|image| {
        let size = image.load_for_size(ui.ctx(), inner.size()).ok()?.size()?;
        (size.x > 0.0 && size.y > 0.0).then_some((image, size))
    });
    match loaded {
        Some((image, size)) => {
            image
                .uv(cover_uv(inner.size(), size))
                .corner_radius(inner_radius)
                .paint_at(ui, inner);
        }
        None => {
            let art = Rect::from_center_size(
                inner.center(),
                egui::Vec2::splat(s.px(PLACEHOLDER_SIZE).min(inner.height() * 0.8)),
            );
            egui::Image::from_bytes("bytes://lunchbox-placeholder-video.png", PLACEHOLDER_PNG)
                .paint_at(ui, art);
        }
    }

    let partial = progress.filter(|p| *p > 0.0 && *p < 1.0);
    if let Some(fraction) = partial {
        let strip = Rect::from_min_max(
            pos2(inner.left(), inner.bottom() - s.px(STRIP_H)),
            inner.max,
        );
        let bottom = CornerRadius {
            nw: 0,
            ne: 0,
            sw: inner_radius.sw,
            se: inner_radius.se,
        };
        painter.rect_filled(strip, bottom, theme::PUTTY);
        let fill = Rect::from_min_size(strip.min, vec2(strip.width() * fraction, strip.height()));
        painter
            .with_clip_rect(fill.intersect(painter.clip_rect()))
            .rect_filled(strip, bottom, theme::YELLOW);
        let rule = s.px(STRIP_RULE);
        painter.rect_filled(
            Rect::from_min_max(
                pos2(strip.left(), strip.top() - rule),
                pos2(strip.right(), strip.top()),
            ),
            0.0,
            theme::INK,
        );
    }

    if let Some(chip) = item.duration_seconds.map(duration_chip) {
        let galley = painter.layout_no_wrap(
            chip,
            theme::font(s.px(CHIP_SIZE), Weight::Bold),
            theme::CREAM,
        );
        let lift = if partial.is_some() {
            s.px(STRIP_H + STRIP_RULE)
        } else {
            0.0
        };
        let pad = s.vec(7.0, 1.0);
        let min = pos2(
            inner.left() + s.px(CHIP_INSET) - border,
            inner.bottom() - s.px(CHIP_INSET) + border - lift - galley.size().y - 2.0 * pad.y,
        );
        let chip_rect = Rect::from_min_size(min, galley.size() + 2.0 * pad);
        painter.rect_filled(chip_rect, s.px(6.0), theme::INK);
        painter.galley(chip_rect.min + pad, galley, theme::CREAM);
    }

    painter.rect_stroke(
        thumb,
        radius,
        Stroke::new(border, theme::INK),
        StrokeKind::Inside,
    );
}

/// The part of an image of `source` size that covers a slot of `slot` size,
/// centred — CSS's `object-fit: cover`, as texture coordinates.
fn cover_uv(slot: egui::Vec2, source: egui::Vec2) -> Rect {
    let slot_aspect = slot.x / slot.y;
    let source_aspect = source.x / source.y;
    if source_aspect > slot_aspect {
        let w = slot_aspect / source_aspect;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = source_aspect / slot_aspect;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    }
}

/// How much of an item has been watched, 0–1, from where it was left off and
/// its length; `None` without a usable length. For a caller's
/// [`Library::progress`].
pub fn watched_fraction(position: f64, duration: Option<f64>) -> Option<f32> {
    let duration = duration.filter(|d| d.is_finite() && *d > 0.0)?;
    Some((position / duration).clamp(0.0, 1.0) as f32)
}

/// "10 min": the chip on a thumbnail. Whole minutes, rounded down, and never
/// "0 min" for something short.
fn duration_chip(seconds: u64) -> String {
    format!("{} min", (seconds / 60).max(1))
}

/// "10 videos", beside the library's name.
fn count_line(count: usize) -> String {
    match count {
        1 => "1 video".to_string(),
        n => format!("{n} videos"),
    }
}

/// "6 min left", under the title of the item offered to continue; nothing
/// when its length is not known.
fn remaining_line(position: f64, duration: Option<f64>) -> Option<String> {
    let left = duration.filter(|d| d.is_finite() && *d > 0.0)? - position;
    Some(format!(
        "{} min left",
        ((left.max(0.0) / 60.0) as u64).max(1)
    ))
}

/// The library's sideways scroll: followed by the focus, dragged by a finger
/// with a flick at the end, and moved by a wheel.
///
/// Reads absolute pointer positions rather than egui's frame-to-frame drag
/// delta, which on a touchscreen carries the jump from the previous touch's
/// release to the new one's start and would throw the row back to the start
/// on the next press.
#[derive(Default)]
struct Scroll {
    offset: f32,
    /// Where a D-pad move wants the row to be; the row eases toward it.
    target: f32,
    /// The pointer's x and the offset when the drag began.
    drag: Option<(f32, f32)>,
    /// Points per second, positive toward the end of the row, while a flick
    /// decays.
    velocity: f32,
    last_sample: Option<(f64, f32)>,
}

impl Scroll {
    /// Bring `tile` (unscrolled) fully into view, clear of the fade.
    fn follow(&mut self, tile: Rect, layout: &Layout, max: f32) {
        let s = layout.s;
        let lead = layout.library.left() + s.px(theme::OUTLINE + theme::COMPARTMENT_PAD);
        let trail = layout.field.right() - s.px(FADE_W);
        let mut target = self.target;
        if tile.right() - target > trail {
            target = tile.right() - trail;
        }
        if tile.left() - target < lead {
            target = tile.left() - lead;
        }
        self.target = target.clamp(0.0, max);
        self.velocity = 0.0;
    }

    fn jump_to(&mut self, target: f32) {
        self.target = target;
        self.velocity = 0.0;
    }

    fn update(
        &mut self,
        drag: &egui::Response,
        wheel: f32,
        dt: f32,
        now: f64,
        max: f32,
        ctx: &egui::Context,
    ) -> f32 {
        if drag.dragged()
            && let Some(pos) = drag.interact_pointer_pos()
        {
            let (start_x, start_offset) = *self.drag.get_or_insert_with(|| {
                self.velocity = 0.0;
                self.last_sample = None;
                (pos.x, self.offset)
            });
            if let Some((t, x)) = self.last_sample {
                let elapsed = (now - t) as f32;
                if elapsed > 0.0 {
                    self.velocity = 0.6 * self.velocity + 0.4 * (x - pos.x) / elapsed;
                }
            }
            self.last_sample = Some((now, pos.x));
            self.offset = (start_offset + start_x - pos.x).clamp(0.0, max);
            self.target = self.offset;
            return self.offset;
        }
        self.drag = None;
        self.last_sample = None;

        if wheel != 0.0 {
            self.target = (self.target - wheel).clamp(0.0, max);
            self.offset = self.target;
            self.velocity = 0.0;
        }

        // A flick, with egui's own scroll-area friction.
        if self.velocity.abs() > 20.0 {
            let friction = 1000.0 * dt;
            self.velocity = if friction > self.velocity.abs() {
                0.0
            } else {
                self.velocity - friction * self.velocity.signum()
            };
            self.target = (self.target + self.velocity * dt).clamp(0.0, max);
            self.offset = self.target;
            ctx.request_repaint();
            return self.offset;
        }
        self.velocity = 0.0;

        // Ease toward the target, quickly: a D-pad press should feel like it
        // moved the row, not like it started an animation.
        self.target = self.target.clamp(0.0, max);
        let gap = self.target - self.offset;
        if gap.abs() < 0.5 {
            self.offset = self.target;
        } else {
            self.offset += gap * (dt * 14.0).min(1.0);
            ctx.request_repaint();
        }
        self.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(tiles: usize, rows: usize, hero: bool) -> Grid {
        Grid { tiles, rows, hero }
    }

    #[test]
    fn left_and_right_cross_columns_and_stop_at_the_ends() {
        let g = grid(10, 2, false);
        assert_eq!(step(Focus::Tile(0), Nav::Right, g), Focus::Tile(2));
        assert_eq!(step(Focus::Tile(3), Nav::Left, g), Focus::Tile(1));
        assert_eq!(step(Focus::Tile(1), Nav::Left, g), Focus::Tile(1));
        assert_eq!(step(Focus::Tile(8), Nav::Right, g), Focus::Tile(8));
    }

    #[test]
    fn right_into_a_short_last_column_lands_on_its_last_tile() {
        // Nine in two rows: the fifth column holds only index 8.
        let g = grid(9, 2, false);
        assert_eq!(step(Focus::Tile(7), Nav::Right, g), Focus::Tile(8));
        assert_eq!(step(Focus::Tile(6), Nav::Right, g), Focus::Tile(8));
        assert_eq!(step(Focus::Tile(8), Nav::Right, g), Focus::Tile(8));
    }

    #[test]
    fn up_and_down_stay_in_the_column_without_wrapping() {
        let g = grid(10, 2, false);
        assert_eq!(step(Focus::Tile(4), Nav::Down, g), Focus::Tile(5));
        assert_eq!(step(Focus::Tile(5), Nav::Down, g), Focus::Tile(5));
        assert_eq!(step(Focus::Tile(5), Nav::Up, g), Focus::Tile(4));
        assert_eq!(step(Focus::Tile(4), Nav::Up, g), Focus::Tile(4));
        // A short last column has nothing below its top tile.
        let g = grid(9, 2, false);
        assert_eq!(step(Focus::Tile(8), Nav::Down, g), Focus::Tile(8));
    }

    #[test]
    fn continue_sits_above_the_row() {
        let g = grid(4, 1, true);
        assert_eq!(step(Focus::Tile(2), Nav::Up, g), Focus::Continue);
        assert_eq!(step(Focus::Continue, Nav::Down, g), Focus::Tile(0));
        assert_eq!(step(Focus::Continue, Nav::Right, g), Focus::Continue);
        // Without a hero row, up from the top row goes nowhere.
        assert_eq!(
            step(Focus::Tile(2), Nav::Up, grid(4, 1, false)),
            Focus::Tile(2)
        );
    }

    #[test]
    fn focus_lands_on_something_that_is_there() {
        assert_eq!(
            clamp_focus(Focus::Continue, grid(4, 1, false)),
            Focus::Tile(0)
        );
        assert_eq!(
            clamp_focus(Focus::Tile(9), grid(4, 1, false)),
            Focus::Tile(3)
        );
        assert_eq!(
            clamp_focus(Focus::Tile(0), grid(0, 1, true)),
            Focus::Continue
        );
    }

    #[test]
    fn a_small_library_is_one_row_and_a_large_one_two() {
        let field = Rect::from_min_size(pos2(0.0, 0.0), theme::DESIGN_SIZE);
        assert_eq!(Layout::new(field, Scale(1.0), 5, false).rows, 1);
        assert_eq!(Layout::new(field, Scale(1.0), 6, false).rows, 2);
        assert_eq!(Layout::new(field, Scale(1.0), 10, true).rows, 1);
    }

    #[test]
    fn tiles_sit_where_the_mockup_puts_them() {
        // media-library.png: the compartment's left edge at 40, the first
        // thumbnail at 68 and the second column's at 436.
        let field = Rect::from_min_size(pos2(0.0, 0.0), theme::DESIGN_SIZE);
        let layout = Layout::new(field, Scale(1.0), 10, false);
        let first = layout.tile_rect(0, 0.0);
        assert_eq!(first.left() + TILE_PAD, 68.0);
        assert_eq!(layout.tile_rect(2, 0.0).left() + TILE_PAD, 436.0);
        assert_eq!(layout.tile_rect(1, 0.0).left(), first.left());
        assert!(layout.max_scroll() > 0.0, "ten videos overflow 1280px");
    }

    #[test]
    fn cover_crops_the_long_axis_and_centres() {
        let uv = cover_uv(vec2(16.0, 9.0), vec2(4.0, 3.0));
        assert_eq!(uv.left(), 0.0);
        assert_eq!(uv.right(), 1.0);
        assert!((uv.height() - 0.75).abs() < 1e-4);
        assert!((uv.center().y - 0.5).abs() < 1e-4);
        let uv = cover_uv(vec2(16.0, 9.0), vec2(32.0, 9.0));
        assert!((uv.width() - 0.5).abs() < 1e-4);
    }

    #[test]
    fn the_words_read_naturally() {
        assert_eq!(duration_chip(634), "10 min");
        assert_eq!(duration_chip(20), "1 min");
        assert_eq!(count_line(1), "1 video");
        assert_eq!(count_line(10), "10 videos");
        assert_eq!(
            remaining_line(252.0, Some(634.0)).as_deref(),
            Some("6 min left")
        );
        assert_eq!(remaining_line(252.0, None), None);
    }

    #[test]
    fn watched_fraction_needs_a_length() {
        assert_eq!(watched_fraction(300.0, Some(600.0)), Some(0.5));
        assert_eq!(watched_fraction(300.0, None), None);
        assert_eq!(watched_fraction(300.0, Some(0.0)), None);
    }
}
