//! Poster grid rendering.

use eframe::egui;
use shepherd_media_core::{Item, Session, resolve_source};

use crate::platform;
use crate::posters::PosterCache;
use crate::ui::theme;

/// Draw the poster grid for `items` (already filtered to the currently visible
/// subset).  `focused` is an index into `items`.
pub fn draw(
    ctx: &egui::Context,
    session: &mut Session,
    items: &[Item],
    focused: &mut usize,
    columns: &mut usize,
    posters: &PosterCache,
) {
    let mut to_select: Option<String> = None;
    let library_title = session.library().title.clone();

    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(theme::BG).inner_margin(48.0))
        .show(ctx, |ui| {
            let available = ui.available_width();
            let target_tile_width = 220.0;
            *columns = ((available / (target_tile_width + 16.0)).floor() as usize).max(1);

            ui.heading(library_title);
            ui.add_space(16.0);

            let info = platform::current();

            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("poster-grid")
                    .num_columns(*columns)
                    .spacing(egui::vec2(16.0, 24.0))
                    .show(ui, |ui| {
                        for (idx, item) in items.iter().enumerate() {
                            let playable = resolve_source(item, &info).is_some();
                            let is_focused = idx == *focused;
                            let bytes = posters.get(&item.id).cloned();
                            let response = draw_tile(
                                ui,
                                &item.id,
                                &item.title,
                                bytes.as_deref(),
                                playable,
                                is_focused,
                            );
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
        });

    if let Some(id) = to_select {
        session.handle_input(shepherd_media_core::SessionInput::SelectItem(id));
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
        painter.rect_stroke(rect, 12.0, egui::Stroke::new(4.0, theme::FOCUS_BORDER));
    }

    // Image slot: top portion of the tile
    let image_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(12.0, 12.0),
        egui::vec2(rect.width() - 24.0, rect.height() - 60.0),
    );
    if let Some(bytes) = poster {
        let uri = format!("bytes://poster-{id}");
        egui::Image::from_bytes(uri, bytes.to_vec())
            .maintain_aspect_ratio(true)
            .fit_to_exact_size(image_rect.size())
            .paint_at(ui, image_rect);
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

fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(3)
        .collect::<String>()
        .to_uppercase()
}
