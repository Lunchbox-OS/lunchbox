//! Playback view: composites the player's GL output into the eframe surface and
//! draws a touch-friendly control overlay.
//!
//! This is adapted from the Linux binary's `ui/playback.rs`, trimmed to touch +
//! keyboard (no gamepad) and using egui's default theme. It is cross-platform:
//! it drives any `PlayerHandle`, so it composites real video from the libmpv
//! backend on Android and simply paints black behind the overlay with the
//! `StubPlayer` on the host (whose `render` is a no-op).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use shepherd_media_core::PlayerHandle;
use shepherd_media_ui::video::{self, VideoCompositor};

const SEEK_DELTA_SECONDS: f64 = 10.0;
const CONTROLS_VISIBLE_FOR: Duration = Duration::from_secs(3);
const CONTROL_BAR_HEIGHT: f32 = 160.0;
const TOUCH_TARGET: f32 = 72.0;

pub struct PlaybackView {
    /// Off-screen GL target the player renders into, shared with the Linux binary.
    compositor: VideoCompositor,
    last_input_at: Instant,
    needs_render: Arc<AtomicBool>,
}

impl PlaybackView {
    pub fn new(gl: Arc<glow::Context>, needs_render: Arc<AtomicBool>) -> Self {
        Self {
            compositor: VideoCompositor::new(gl),
            last_input_at: Instant::now(),
            needs_render,
        }
    }

    pub fn note_started(&mut self) {
        self.last_input_at = Instant::now();
    }

    /// Composite the current frame and draw the overlay. Returns `true` if the
    /// user asked to leave playback (back button / Esc).
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        frame: &mut eframe::Frame,
        player: &mut dyn PlayerHandle,
        title: &str,
    ) -> bool {
        let ctx = ui.ctx().clone();
        let mut leave = false;
        let mut any_input = false;

        // Keyboard transport.
        ctx.input(|input| {
            if input.pointer.any_pressed() || input.pointer.any_down() {
                any_input = true;
            }
            // Space/K, or the D-pad center (Enter), toggles play/pause.
            if input.key_pressed(egui::Key::Space)
                || input.key_pressed(egui::Key::K)
                || input.key_pressed(egui::Key::Enter)
            {
                toggle_pause(player);
                any_input = true;
            }
            // Remote BACK (Android delivers it as BrowserBack), Esc, or Backspace
            // leaves playback.
            if input.key_pressed(egui::Key::BrowserBack)
                || input.key_pressed(egui::Key::Escape)
                || input.key_pressed(egui::Key::Backspace)
            {
                leave = true;
            }
            if input.key_pressed(egui::Key::ArrowLeft) || input.key_pressed(egui::Key::J) {
                let _ = player.seek_relative(-SEEK_DELTA_SECONDS);
                any_input = true;
            }
            if input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::L) {
                let _ = player.seek_relative(SEEK_DELTA_SECONDS);
                any_input = true;
            }
        });

        let screen_pixels = ctx.content_rect().size() * ctx.pixels_per_point();
        let target_size = [
            screen_pixels.x.max(1.0) as u32,
            screen_pixels.y.max(1.0) as u32,
        ];

        let _new_frame = self.needs_render.swap(false, Ordering::Relaxed);

        // The shared compositor sizes the off-screen target and has the player
        // render this frame into it; we get back the egui texture to paint.
        let texture_id = self.compositor.composite(
            target_size,
            |texture| frame.register_native_glow_texture(texture),
            |fbo, w, h| {
                if let Err(e) = player.render(fbo, w, h) {
                    log::warn!("player render failed: {e}");
                }
            },
        );

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::BLACK))
            .show_inside(ui, |ui| {
                let rect = ui.max_rect();
                video::paint_frame(ui.painter(), rect, texture_id);

                let controls_visible =
                    self.last_input_at.elapsed() < CONTROLS_VISIBLE_FOR || player.is_paused();
                if controls_visible && self.draw_overlay(ui, rect, player, title) {
                    leave = true;
                }
            });

        if any_input {
            self.last_input_at = Instant::now();
        }

        let next = if self.last_input_at.elapsed() < CONTROLS_VISIBLE_FOR {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        };
        ctx.request_repaint_after(next);

        leave
    }

    /// Draw the overlay. Returns `true` if the back button was tapped.
    fn draw_overlay(
        &self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        player: &mut dyn PlayerHandle,
        title: &str,
    ) -> bool {
        let mut back = false;

        let header_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 96.0));
        ui.painter()
            .rect_filled(header_rect, 0.0, egui::Color32::from_black_alpha(180));
        ui.painter().text(
            header_rect.left_center() + egui::vec2(32.0, 0.0),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(28.0),
            egui::Color32::WHITE,
        );

        let mut header_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(header_rect.shrink(16.0))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if button(&mut header_ui, "‹ Back").clicked() {
            back = true;
        }

        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.max.y - CONTROL_BAR_HEIGHT),
            egui::vec2(rect.width(), CONTROL_BAR_HEIGHT),
        );
        ui.painter()
            .rect_filled(bar_rect, 0.0, egui::Color32::from_black_alpha(180));

        let position = player.position().unwrap_or(0.0);
        let duration = player.duration().unwrap_or(0.0);

        let scrubber_rect = egui::Rect::from_min_size(
            bar_rect.min + egui::vec2(32.0, 16.0),
            egui::vec2(bar_rect.width() - 64.0, 40.0),
        );
        let mut scrubber_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(scrubber_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        scrubber_ui.label(
            egui::RichText::new(video::format_time(position))
                .monospace()
                .size(20.0),
        );
        scrubber_ui.add_space(12.0);
        let slider_width = scrubber_rect.width() - 160.0;
        if let Some(new_pos) = video::touch_slider(
            &mut scrubber_ui,
            egui::vec2(slider_width, 40.0),
            position,
            (0.0, duration),
            egui::Color32::LIGHT_BLUE,
            egui::Color32::WHITE,
        ) {
            let _ = player.seek_absolute(new_pos);
        }
        scrubber_ui.add_space(12.0);
        scrubber_ui.label(
            egui::RichText::new(video::format_time(duration))
                .monospace()
                .size(20.0),
        );

        let buttons_rect = egui::Rect::from_min_size(
            bar_rect.min + egui::vec2(32.0, 72.0),
            egui::vec2(bar_rect.width() - 64.0, TOUCH_TARGET),
        );
        let mut buttons_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(buttons_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let cluster_width = TOUCH_TARGET * 2.0 * 3.0 + 16.0 * 2.0;
        let lead = ((buttons_rect.width() - cluster_width) / 2.0).max(0.0);
        buttons_ui.add_space(lead);
        if button(&mut buttons_ui, "« 10s").clicked() {
            let _ = player.seek_relative(-SEEK_DELTA_SECONDS);
        }
        buttons_ui.add_space(16.0);
        let play_label = if player.is_paused() {
            "▶  Play"
        } else {
            "▮▮  Pause"
        };
        if button(&mut buttons_ui, play_label).clicked() {
            toggle_pause(player);
        }
        buttons_ui.add_space(16.0);
        if button(&mut buttons_ui, "10s »").clicked() {
            let _ = player.seek_relative(SEEK_DELTA_SECONDS);
        }

        back
    }
}

fn toggle_pause(player: &mut dyn PlayerHandle) {
    let paused = player.is_paused();
    let _ = player.set_paused(!paused);
}

fn button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = egui::RichText::new(label).size(22.0).strong();
    ui.add_sized(
        egui::vec2(TOUCH_TARGET * 2.0, TOUCH_TARGET),
        egui::Button::new(text).corner_radius(12.0),
    )
}
