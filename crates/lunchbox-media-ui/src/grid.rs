//! Poster grid rendering — the shared "view a library's contents" UI used by
//! both the Linux binary and the Android app, so they render an identical
//! poster grid. It is platform-agnostic: the caller supplies the items, the
//! focused index (moved by its own keyboard / D-pad / gamepad input), and a
//! poster-bytes accessor; `draw` returns the id of an item the user chose to
//! play, leaving playback to the caller.

use lunchbox_media_core::{Item, PlatformInfo, resolve_source};

use crate::theme;

/// Persistent state for the grid's custom drag-to-scroll. egui's built-in
/// `drag_to_scroll` is delta-based and resets to the top on the first press
/// after the user releases a stationary finger (because the press-frame's
/// `pointer.delta()` carries the jump from the previous touch's release
/// position to the new touch's start). The `touch_slider` in the playback view
/// hits the same delta bug and works around it by reading absolute pointer
/// positions; we do the same here.
#[derive(Default)]
pub struct ScrollState {
    /// Scroll offset to apply on the next frame. Updated by both the custom
    /// drag handler and the previous frame's ScrollArea output (so wheel,
    /// keyboard, and `scroll_to_me` still work).
    offset: f32,
    /// Captured at drag start so target_offset can be computed from absolute
    /// pointer position rather than frame-to-frame deltas.
    drag_start: Option<DragStart>,
    /// Velocity in points/second, positive = scrolling toward larger offset.
    /// Non-zero while kinetic flick is decaying.
    velocity: f32,
    /// Last pointer Y / timestamp during an active drag, used to derive
    /// release velocity without depending on egui's pointer.velocity().
    last_sample: Option<(f64, f32)>,
    /// Focused index from the previous frame. We only `scroll_to_me` the
    /// focused tile when the index changes (e.g., from a keyboard arrow or
    /// gamepad dpad press) so the user can drag-to-browse without the view
    /// snapping back to the focused tile each frame.
    last_focused: Option<usize>,
}

struct DragStart {
    pointer_y: f32,
    offset_y: f32,
}

/// Draw the poster grid for `items` (already filtered to the currently visible
/// subset). `focused` is an index into `items`; the caller moves it in response
/// to input. `get_poster` returns the (already-fetched) encoded poster bytes
/// for an item id, if available. Returns the id of an item the user activated
/// (clicked, and which is playable on this platform).
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &mut egui::Ui,
    scroll: &mut ScrollState,
    title: &str,
    items: &[Item],
    focused: &mut usize,
    columns: &mut usize,
    get_poster: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Option<String> {
    let mut to_select: Option<String> = None;

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::BG).inner_margin(48.0))
        .show_inside(ui, |ui| {
            let available = ui.available_width();
            let target_tile_width = 220.0;
            *columns = ((available / (target_tile_width + 16.0)).floor() as usize).max(1);

            ui.heading(title);
            ui.add_space(16.0);

            let info = PlatformInfo::current();

            // Drag-to-scroll covers the remaining vertical space. `Sense::drag`
            // (not click_and_drag) coexists with the tiles' `Sense::click`:
            // egui's hit-test reports click and drag hits independently, so
            // taps still register on the tile underneath.
            let scroll_rect = ui.available_rect_before_wrap();
            let drag_resp = ui.interact(
                scroll_rect,
                ui.id().with("grid-drag-to-scroll"),
                egui::Sense::drag(),
            );

            let (dt, now) = ui.input(|i| (i.stable_dt.min(0.1), i.time));
            let override_offset = update_scroll(scroll, &drag_resp, dt, now, ui.ctx());

            let mut area =
                egui::ScrollArea::vertical().scroll_source(egui::scroll_area::ScrollSource {
                    drag: false,
                    ..egui::scroll_area::ScrollSource::ALL
                });
            if let Some(offset) = override_offset {
                area = area.vertical_scroll_offset(offset);
            }
            let focus_changed = scroll.last_focused != Some(*focused);
            scroll.last_focused = Some(*focused);

            let output = area.show(ui, |ui| {
                egui::Grid::new("poster-grid")
                    .num_columns(*columns)
                    .spacing(egui::vec2(16.0, 24.0))
                    .show(ui, |ui| {
                        for (idx, item) in items.iter().enumerate() {
                            let playable = resolve_source(item, &info).is_some();
                            let is_focused = idx == *focused;
                            let bytes = get_poster(&item.id);
                            let response = draw_tile(
                                ui,
                                &item.id,
                                &item.title,
                                bytes.as_deref(),
                                playable,
                                is_focused,
                            );
                            if is_focused && focus_changed {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() && playable {
                                *focused = idx;
                                to_select = Some(item.id.clone());
                            }
                            if (idx + 1) % *columns == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });

            // Sync our tracked offset with whatever the ScrollArea ended up at
            // (it may have clamped our value, scrolled via wheel/keyboard,
            // or honored a scroll_to_me from a child widget).
            scroll.offset = output.state.offset.y;
        });

    to_select
}

/// Update `scroll` from this frame's drag input and return the offset to
/// force onto the ScrollArea (or `None` if we shouldn't override — meaning
/// scroll wheel / keyboard / arrow-key navigation get to drive scrolling).
fn update_scroll(
    scroll: &mut ScrollState,
    drag_resp: &egui::Response,
    dt: f32,
    now: f64,
    ctx: &egui::Context,
) -> Option<f32> {
    if drag_resp.dragged() {
        // A new touch — start tracking. Cancel any in-flight kinetic flick:
        // the user is taking over.
        let pos = drag_resp.interact_pointer_pos()?;
        let start = scroll.drag_start.get_or_insert_with(|| {
            scroll.velocity = 0.0;
            scroll.last_sample = None;
            DragStart {
                pointer_y: pos.y,
                offset_y: scroll.offset,
            }
        });

        scroll.offset = start.offset_y + (start.pointer_y - pos.y);

        // Track an exponentially-smoothed velocity to use for the flick on
        // release. We use the last sample rather than reading egui's
        // pointer.velocity() to keep this independent of the same input-
        // state bug that broke drag_to_scroll.
        if let Some((prev_t, prev_y)) = scroll.last_sample {
            let dt_sample = (now - prev_t) as f32;
            if dt_sample > 0.0 {
                let instant = (prev_y - pos.y) / dt_sample;
                scroll.velocity = 0.6 * scroll.velocity + 0.4 * instant;
            }
        }
        scroll.last_sample = Some((now, pos.y));
        Some(scroll.offset)
    } else {
        if scroll.drag_start.is_some() {
            scroll.drag_start = None;
            scroll.last_sample = None;
        }

        // Kinetic flick: matches egui's default scroll_area friction/stop
        // thresholds so the feel is the same as a normal egui app.
        let stop_speed = 20.0;
        let friction_coeff = 1000.0;
        let speed = scroll.velocity.abs();
        if speed < stop_speed {
            scroll.velocity = 0.0;
            return None;
        }
        let friction = friction_coeff * dt;
        if friction > speed {
            scroll.velocity = 0.0;
            return None;
        }
        scroll.velocity -= friction * scroll.velocity.signum();
        scroll.offset += scroll.velocity * dt;
        ctx.request_repaint();
        Some(scroll.offset)
    }
}

fn draw_tile(
    ui: &mut egui::Ui,
    id: &str,
    title: &str,
    poster: Option<&[u8]>,
    playable: bool,
    focused: bool,
) -> egui::Response {
    let desired = egui::vec2(220.0, 280.0);
    let (rect, response) = ui.allocate_at_least(desired, egui::Sense::click());

    let painter = ui.painter_at(rect);
    let bg = if focused {
        theme::TILE_FOCUSED
    } else {
        theme::TILE
    };
    painter.rect_filled(rect, 12.0, bg);

    if focused {
        painter.rect_stroke(
            rect,
            12.0,
            egui::Stroke::new(4.0_f32, theme::FOCUS_BORDER),
            egui::StrokeKind::Outside,
        );
    }

    // Image slot: top portion of the tile
    let image_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(12.0, 12.0),
        egui::vec2(rect.width() - 24.0, rect.height() - 60.0),
    );
    if let Some(bytes) = poster {
        let uri = format!("bytes://poster-{id}");
        let image = egui::Image::from_bytes(uri, bytes.to_vec());
        // `Image::paint_at` stretches the image to fill the given rect — it
        // ignores `maintain_aspect_ratio`. Query the loaded texture's source
        // size and shrink the paint rect to match its aspect, so a 16:9
        // YouTube thumbnail isn't squashed into a near-square slot.
        let source_size = image
            .load_for_size(ui.ctx(), image_rect.size())
            .ok()
            .and_then(|t| t.size());
        let paint_rect = match source_size {
            Some(s) if s.x > 0.0 && s.y > 0.0 => fit_centered(image_rect, s),
            _ => image_rect,
        };
        image.paint_at(ui, paint_rect);
    } else {
        painter.rect_filled(image_rect, 6.0, theme::BG);
        painter.text(
            image_rect.center(),
            egui::Align2::CENTER_CENTER,
            initials(title),
            egui::FontId::proportional(48.0),
            theme::TEXT_DIM,
        );
    }

    // Title slot
    let label_color = if playable {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    };
    painter.text(
        rect.left_bottom() + egui::vec2(12.0, -20.0),
        egui::Align2::LEFT_BOTTOM,
        title,
        egui::FontId::proportional(18.0),
        label_color,
    );

    if !playable {
        // Dim the entire tile by overlaying a translucent rectangle.
        painter.rect_filled(rect, 12.0, egui::Color32::from_black_alpha(120));
    }

    response
}

/// Largest centered subrect of `container` whose aspect ratio matches
/// `source_size`. Used to letterbox/pillarbox posters of varying aspects
/// inside the fixed-size tile image slot.
fn fit_centered(container: egui::Rect, source_size: egui::Vec2) -> egui::Rect {
    let scale = (container.width() / source_size.x).min(container.height() / source_size.y);
    egui::Rect::from_center_size(container.center(), source_size * scale)
}

fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(3)
        .collect::<String>()
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_takes_first_letter_of_up_to_three_words_uppercased() {
        assert_eq!(initials("The Matrix Reloaded"), "TMR");
        assert_eq!(initials("lower case title"), "LCT");
    }

    #[test]
    fn initials_caps_at_three_words_and_ignores_extra_whitespace() {
        assert_eq!(initials("a b c d e"), "ABC");
        assert_eq!(initials("  spaced   out  "), "SO");
    }

    #[test]
    fn initials_is_empty_for_a_blank_title() {
        assert_eq!(initials(""), "");
        assert_eq!(initials("   "), "");
    }

    #[test]
    fn fit_centered_pillarboxes_a_wide_source_without_squashing() {
        // Square slot, 16:9 (wide) source: the fit is limited by width, so it
        // spans the full width and is proportionally shorter — a 16:9 YouTube
        // thumbnail keeps its aspect instead of being squashed to a square.
        let slot = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let fit = fit_centered(slot, egui::vec2(16.0, 9.0));
        assert!((fit.width() - 200.0).abs() < 1e-3);
        assert!((fit.height() - 112.5).abs() < 1e-3); // 200 * 9/16
        // Centered in, and fully contained by, the slot.
        assert!((fit.center().x - slot.center().x).abs() < 1e-3);
        assert!((fit.center().y - slot.center().y).abs() < 1e-3);
        assert!(fit.min.y >= slot.min.y - 1e-3 && fit.max.y <= slot.max.y + 1e-3);
    }

    #[test]
    fn fit_centered_letterboxes_a_tall_source() {
        // Wide slot, portrait source: the fit is limited by height.
        let slot = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let fit = fit_centered(slot, egui::vec2(9.0, 16.0));
        assert!((fit.height() - 100.0).abs() < 1e-3);
        assert!((fit.width() - 56.25).abs() < 1e-3); // 100 * 9/16
        assert!(fit.min.x >= slot.min.x - 1e-3 && fit.max.x <= slot.max.x + 1e-3);
    }
}
