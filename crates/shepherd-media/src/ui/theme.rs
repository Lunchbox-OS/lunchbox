//! Visual styling for the browse UI. Dark background with a clear focus
//! outline so a single highlighted poster reads from across the room.

use eframe::egui;

pub const BG: egui::Color32 = egui::Color32::from_rgb(0x10, 0x12, 0x18);
pub const TILE: egui::Color32 = egui::Color32::from_rgb(0x1c, 0x20, 0x2c);
pub const TILE_FOCUSED: egui::Color32 = egui::Color32::from_rgb(0x2a, 0x33, 0x4a);
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(0xea, 0xea, 0xea);
pub const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(0x80, 0x80, 0x80);
pub const FOCUS_BORDER: egui::Color32 = egui::Color32::from_rgb(0xff, 0xd1, 0x66);

pub fn install(ctx: &egui::Context) {
    let mut style = (*ctx.global_style()).clone();
    style.visuals.dark_mode = true;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.window_fill = BG;
    style.visuals.panel_fill = BG;
    style.spacing.item_spacing = egui::vec2(16.0, 16.0);
    ctx.set_global_style(style);
}
