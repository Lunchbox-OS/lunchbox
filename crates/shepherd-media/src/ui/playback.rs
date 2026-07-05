//! Playback view: composites mpv's render output into the eframe surface
//! and draws a touch- and controller-friendly control overlay.
//!
//! Lifecycle:
//!
//! - `PlaybackView::new` is called once, inside `eframe::App::new`'s
//!   creation closure, after `session.bind_gl` has been wired up.
//! - On every `update()` while the session is in `Playing` or
//!   `Stopping`, the host calls `draw`, which:
//!   1. (Re)allocates the offscreen FBO + texture if the viewport
//!      changed size, registering the texture with egui_glow on the
//!      first draw.
//!   2. Asks mpv to render the current frame into that FBO.
//!   3. Paints the texture full-screen and overlays the controls.
//! - Input (touch/click, keyboard, gamepad) is fed into `handle_input`
//!   before `draw` each frame.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use shepherd_media_core::{Session, SessionInput};

use shepherd_media_ui::theme;
use shepherd_media_ui::video::{self, VideoCompositor};

/// Delta (seconds) applied by the ±10s buttons and the LB/RB / dpad-left/right
/// gamepad bindings.
const SEEK_DELTA_SECONDS: f64 = 10.0;

/// How long the control overlay stays visible after the last input event.
const CONTROLS_VISIBLE_FOR: Duration = Duration::from_secs(3);

/// Height (logical pixels) of the bottom control panel, sized for thumb taps.
const CONTROL_BAR_HEIGHT: f32 = 160.0;

/// Minimum hit-box size for a touch-friendly button.
const TOUCH_TARGET: f32 = 72.0;

pub struct PlaybackView {
    /// Off-screen GL target mpv renders into, shared with the Android app.
    compositor: VideoCompositor,
    last_input_at: Instant,
    /// Title of the item that's currently playing — captured when entering
    /// Playing state so the overlay header keeps showing the right name even
    /// after the session machine transitions back to Browsing.
    item_title: String,
    /// Latched into `true` by the mpv update callback running on a background
    /// thread; cleared once we've issued a render this frame.
    needs_render: Arc<std::sync::atomic::AtomicBool>,
}

impl PlaybackView {
    pub fn new(gl: Arc<glow::Context>, needs_render: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            compositor: VideoCompositor::new(gl),
            last_input_at: Instant::now(),
            item_title: String::new(),
            needs_render,
        }
    }

    pub fn note_item_started(&mut self, title: &str) {
        self.item_title = title.to_string();
        self.last_input_at = Instant::now();
    }

    /// Called every frame while playback is active, before `draw`.
    pub fn handle_input(
        &mut self,
        ctx: &egui::Context,
        session: &mut Session,
        gamepad_events: &[gilrs::EventType],
    ) {
        let mut any_input = false;

        ctx.input(|input| {
            // Pointer activity (touch or mouse) summons the controls.
            if input.pointer.any_pressed() || input.pointer.any_down() {
                any_input = true;
            }

            if input.key_pressed(egui::Key::Space) || input.key_pressed(egui::Key::K) {
                toggle_pause(session);
                any_input = true;
            }
            if input.key_pressed(egui::Key::Escape) || input.key_pressed(egui::Key::Backspace) {
                session.handle_input(SessionInput::StopPlayback);
                any_input = true;
            }
            if input.key_pressed(egui::Key::ArrowLeft) || input.key_pressed(egui::Key::J) {
                let _ = session.seek_relative(-SEEK_DELTA_SECONDS);
                any_input = true;
            }
            if input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::L) {
                let _ = session.seek_relative(SEEK_DELTA_SECONDS);
                any_input = true;
            }
            // Volume is intentionally not bound here — the global HUD's
            // volume keys (and gamepad bindings) handle that everywhere.
        });

        for ev in gamepad_events {
            use gilrs::{Button, EventType};
            if let EventType::ButtonPressed(btn, _) = ev {
                any_input = true;
                match btn {
                    Button::South => toggle_pause(session),
                    Button::East | Button::Start | Button::Select => {
                        session.handle_input(SessionInput::StopPlayback);
                    }
                    Button::DPadLeft | Button::LeftTrigger | Button::LeftTrigger2 => {
                        let _ = session.seek_relative(-SEEK_DELTA_SECONDS);
                    }
                    Button::DPadRight | Button::RightTrigger | Button::RightTrigger2 => {
                        let _ = session.seek_relative(SEEK_DELTA_SECONDS);
                    }
                    // Volume bindings live on the global HUD, not here.
                    _ => any_input = false,
                }
            }
        }

        if any_input {
            self.last_input_at = Instant::now();
        }
    }

    /// Render mpv's current frame into our FBO and paint the egui scene
    /// (video texture + control overlay).
    pub fn draw(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame, session: &mut Session) {
        let ctx = ui.ctx().clone();
        let screen_pixels = ctx.content_rect().size() * ctx.pixels_per_point();
        let target_size = [
            screen_pixels.x.max(1.0) as u32,
            screen_pixels.y.max(1.0) as u32,
        ];

        // Pull a frame from mpv if it has one, regardless of which thread
        // signaled. We always render at least the previous frame so the
        // scene stays painted on resize.
        let _new_frame = self
            .needs_render
            .swap(false, std::sync::atomic::Ordering::Relaxed);

        // The shared compositor sizes the off-screen target and has mpv render
        // this frame into it; we get back the egui texture to paint.
        let texture_id = self.compositor.composite(
            target_size,
            |texture| frame.register_native_glow_texture(texture),
            |fbo, w, h| {
                if let Err(e) = session.render(fbo, w, h) {
                    tracing::warn!("mpv render failed: {e}");
                }
            },
        );

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::BLACK))
            .show_inside(ui, |ui| {
                let rect = ui.max_rect();
                video::paint_frame(ui.painter(), rect, texture_id);

                // Auto-hide the overlay after CONTROLS_VISIBLE_FOR of idle.
                let controls_visible =
                    self.last_input_at.elapsed() < CONTROLS_VISIBLE_FOR || session.is_paused();
                if controls_visible {
                    self.draw_overlay(ui, rect, session);
                }
            });

        // Keep redrawing while the overlay is visible (so the elapsed
        // time updates) and at a slower cadence otherwise.
        let next = if self.last_input_at.elapsed() < CONTROLS_VISIBLE_FOR {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        };
        ctx.request_repaint_after(next);
    }

    fn draw_overlay(&self, ui: &mut egui::Ui, rect: egui::Rect, session: &mut Session) {
        // Header strip — title + back affordance.
        let header_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 96.0));
        ui.painter()
            .rect_filled(header_rect, 0.0, egui::Color32::from_black_alpha(180));
        ui.painter().text(
            header_rect.left_center() + egui::vec2(32.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &self.item_title,
            egui::FontId::proportional(28.0),
            theme::TEXT,
        );

        let mut header_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(header_rect.shrink(16.0))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if button(&mut header_ui, "‹ Back", TOUCH_TARGET).clicked() {
            session.handle_input(SessionInput::StopPlayback);
        }

        // Bottom control bar.
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.max.y - CONTROL_BAR_HEIGHT),
            egui::vec2(rect.width(), CONTROL_BAR_HEIGHT),
        );
        ui.painter()
            .rect_filled(bar_rect, 0.0, egui::Color32::from_black_alpha(180));

        let position = session.position().unwrap_or(0.0);
        let duration = session.duration().unwrap_or(0.0);

        // Scrubber row: elapsed / slider / total.
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
            theme::FOCUS_BORDER,
            theme::FOCUS_BORDER,
        ) {
            let _ = session.seek_absolute(new_pos);
        }
        scrubber_ui.add_space(12.0);
        scrubber_ui.label(
            egui::RichText::new(video::format_time(duration))
                .monospace()
                .size(20.0),
        );

        // Button row: -10s / play-pause / +10s, centered. Volume is
        // intentionally not here — the global HUD owns volume control
        // so duplicating it on the playback overlay would just confuse.
        let buttons_rect = egui::Rect::from_min_size(
            bar_rect.min + egui::vec2(32.0, 72.0),
            egui::vec2(bar_rect.width() - 64.0, TOUCH_TARGET),
        );
        let mut buttons_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(buttons_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        // Spacer that centers the button cluster within the row.
        let cluster_width = TOUCH_TARGET * 2.0 * 3.0 + 16.0 * 2.0;
        let lead = ((buttons_rect.width() - cluster_width) / 2.0).max(0.0);
        buttons_ui.add_space(lead);
        if button(&mut buttons_ui, "« 10s", TOUCH_TARGET).clicked() {
            let _ = session.seek_relative(-SEEK_DELTA_SECONDS);
        }
        buttons_ui.add_space(16.0);
        // Glyphs deliberately drawn from the Geometric Shapes block —
        // bundled NotoEmoji covers them while it does NOT reliably
        // render the Dingbat-block "❚❚" we used previously.
        let play_label = if session.is_paused() {
            "▶  Play"
        } else {
            "▮▮  Pause"
        };
        if button(&mut buttons_ui, play_label, TOUCH_TARGET).clicked() {
            toggle_pause(session);
        }
        buttons_ui.add_space(16.0);
        if button(&mut buttons_ui, "10s »", TOUCH_TARGET).clicked() {
            let _ = session.seek_relative(SEEK_DELTA_SECONDS);
        }
    }
}

fn toggle_pause(session: &mut Session) {
    let new_paused = !session.is_paused();
    let _ = session.set_paused(new_paused);
}

fn button(ui: &mut egui::Ui, label: &str, height: f32) -> egui::Response {
    let text = egui::RichText::new(label).size(22.0).strong();
    ui.add_sized(
        egui::vec2(height * 2.0, height),
        egui::Button::new(text)
            .fill(theme::TILE_FOCUSED)
            .corner_radius(12.0),
    )
}
