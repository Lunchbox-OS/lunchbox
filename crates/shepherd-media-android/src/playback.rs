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

use glow::HasContext;
use shepherd_media_core::PlayerHandle;

const SEEK_DELTA_SECONDS: f64 = 10.0;
const CONTROLS_VISIBLE_FOR: Duration = Duration::from_secs(3);
const CONTROL_BAR_HEIGHT: f32 = 160.0;
const TOUCH_TARGET: f32 = 72.0;

struct GlSurface {
    fbo: glow::Framebuffer,
    texture: glow::Texture,
    size: [u32; 2],
    texture_id: Option<egui::TextureId>,
}

pub struct PlaybackView {
    gl: Arc<glow::Context>,
    surface: Option<GlSurface>,
    last_input_at: Instant,
    needs_render: Arc<AtomicBool>,
}

impl PlaybackView {
    pub fn new(gl: Arc<glow::Context>, needs_render: Arc<AtomicBool>) -> Self {
        Self {
            gl,
            surface: None,
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
            if input.key_pressed(egui::Key::Space) || input.key_pressed(egui::Key::K) {
                toggle_pause(player);
                any_input = true;
            }
            if input.key_pressed(egui::Key::Escape) || input.key_pressed(egui::Key::Backspace) {
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
        self.ensure_surface(target_size, frame);

        let _new_frame = self.needs_render.swap(false, Ordering::Relaxed);

        if let Some(surface) = &self.surface {
            unsafe {
                self.gl
                    .bind_framebuffer(glow::FRAMEBUFFER, Some(surface.fbo));
            }
            if let Err(e) = player.render(
                framebuffer_to_gl(surface.fbo),
                surface.size[0] as i32,
                surface.size[1] as i32,
            ) {
                log::warn!("player render failed: {e}");
            }
            unsafe {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            }
        }

        let texture_id = self.surface.as_ref().and_then(|s| s.texture_id);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::BLACK))
            .show_inside(ui, |ui| {
                let rect = ui.max_rect();
                if let Some(tex_id) = texture_id {
                    ui.painter().image(
                        tex_id,
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }

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

    fn ensure_surface(&mut self, target_size: [u32; 2], frame: &mut eframe::Frame) {
        let needs_alloc = match &self.surface {
            Some(s) => s.size != target_size,
            None => true,
        };
        if !needs_alloc {
            return;
        }
        if let Some(old) = self.surface.take() {
            unsafe {
                self.gl.delete_framebuffer(old.fbo);
                self.gl.delete_texture(old.texture);
            }
        }

        let (fbo, texture) = unsafe {
            let texture = self
                .gl
                .create_texture()
                .expect("failed to create GL texture");
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                target_size[0] as i32,
                target_size[1] as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );

            let fbo = self
                .gl
                .create_framebuffer()
                .expect("failed to create GL framebuffer");
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.bind_texture(glow::TEXTURE_2D, None);
            (fbo, texture)
        };

        let texture_id = frame.register_native_glow_texture(texture);
        self.surface = Some(GlSurface {
            fbo,
            texture,
            size: target_size,
            texture_id: Some(texture_id),
        });
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
            egui::RichText::new(format_time(position))
                .monospace()
                .size(20.0),
        );
        scrubber_ui.add_space(12.0);
        let slider_width = scrubber_rect.width() - 160.0;
        if let Some(new_pos) = touch_slider(
            &mut scrubber_ui,
            egui::vec2(slider_width, 40.0),
            position,
            (0.0, duration),
        ) {
            let _ = player.seek_absolute(new_pos);
        }
        scrubber_ui.add_space(12.0);
        scrubber_ui.label(
            egui::RichText::new(format_time(duration))
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

fn touch_slider(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    current: f64,
    range: (f64, f64),
) -> Option<f64> {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let interacting = response.is_pointer_button_down_on() || response.dragged();
    let new_value = if interacting && range.1 > range.0 {
        let pos = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.latest_pos()));
        pos.map(|p| {
            let frac = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
            range.0 + frac * (range.1 - range.0)
        })
    } else {
        None
    };

    let display = new_value.unwrap_or(current);
    let frac = if range.1 > range.0 {
        ((display - range.0) / (range.1 - range.0)).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };

    let painter = ui.painter_at(rect);
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 12.0));
    painter.rect_filled(track, 6.0, egui::Color32::from_white_alpha(40));
    if range.1 > range.0 {
        let filled =
            egui::Rect::from_min_size(track.min, egui::vec2(track.width() * frac, track.height()));
        painter.rect_filled(filled, 6.0, egui::Color32::LIGHT_BLUE);
        let knob_x = track.min.x + track.width() * frac;
        painter.circle_filled(
            egui::pos2(knob_x, track.center().y),
            14.0,
            egui::Color32::WHITE,
        );
    }
    new_value
}

fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_string();
    }
    let total = seconds as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn framebuffer_to_gl(fb: glow::Framebuffer) -> i32 {
    fb.0.get() as i32
}
