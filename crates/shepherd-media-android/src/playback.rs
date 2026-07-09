//! Playback view: composites the player's GL output into the eframe surface and
//! draws the transport overlay.
//!
//! The GL compositor and the overlay are the shared `shepherd-media-ui::video`
//! code (also used by the Linux binary); this file is just the touch/D-pad
//! input handling and the Android theme colors on top. It drives any
//! `PlayerHandle`, so it composites real video from the libmpv backend on
//! Android and paints black behind the overlay with the `StubPlayer` on the
//! host (whose `render` is a no-op).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use shepherd_media_core::PlayerHandle;
use shepherd_media_ui::video::{self, VideoCompositor};

/// The shared transport overlay in the Android app's default-theme colors.
const OVERLAY_THEME: video::OverlayTheme = video::OverlayTheme {
    text: egui::Color32::WHITE,
    slider_fill: egui::Color32::LIGHT_BLUE,
    slider_knob: egui::Color32::WHITE,
    button_fill: None,
};

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

        // Keyboard / D-pad / remote transport, via the shared key→intent map.
        ctx.input(|input| {
            if input.pointer.any_pressed() || input.pointer.any_down() {
                any_input = true;
            }
            for &key in video::TRANSPORT_KEYS {
                if !input.key_pressed(key) {
                    continue;
                }
                match video::key_intent(key, video::SEEK_DELTA_SECONDS) {
                    Some(video::TransportIntent::TogglePause) => {
                        video::toggle_pause(player);
                        any_input = true;
                    }
                    Some(video::TransportIntent::Seek(delta)) => {
                        let _ = player.seek_relative(delta);
                        any_input = true;
                    }
                    Some(video::TransportIntent::Leave) => leave = true,
                    None => {}
                }
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

                let controls_visible = self.last_input_at.elapsed() < video::CONTROLS_VISIBLE_FOR
                    || player.is_paused();
                if controls_visible
                    && video::transport_overlay(ui, rect, player, title, &OVERLAY_THEME)
                        == video::OverlayAction::Leave
                {
                    leave = true;
                }
            });

        if any_input {
            self.last_input_at = Instant::now();
        }

        // mpv's render API is driven by the host: it re-presents the current
        // frame and advances when a new one is ready. While playing, repaint
        // every frame so video is composited at the display's refresh rate rather
        // than at mpv's update-callback cadence — the latter left the Fire TV
        // presenting ~15 fps (visibly choppy) even though decode kept up. When
        // paused, idle at a slow tick (still frequent enough to fade the overlay).
        if player.is_paused() {
            let next = if self.last_input_at.elapsed() < video::CONTROLS_VISIBLE_FOR {
                Duration::from_millis(33)
            } else {
                Duration::from_millis(250)
            };
            ctx.request_repaint_after(next);
        } else {
            ctx.request_repaint();
        }

        leave
    }
}
