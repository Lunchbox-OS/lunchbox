//! Playback view: the transport overlay drawn over the video.
//!
//! Unlike the Linux binary, this front-end does **not** composite video into
//! the eframe surface. mpv decodes straight into the activity's video
//! `SurfaceView` (`vo=mediacodec_embed`, see `surface.rs`), which Android
//! composites behind this window — so all this view has to do is leave the
//! video area transparent and paint the controls on top.
//!
//! That is what the change is for. Compositing through egui meant the only
//! reachable hwdec was `mediacodec-copy`, which reads every decoded frame back
//! into system RAM to be re-uploaded as a texture; on a Fire TV that copy, and
//! the full-screen passes around it, capped 60fps content at ~20fps (#115).
//!
//! The overlay layout, theme and input handling are unchanged.

use std::time::{Duration, Instant};

use shepherd_media_core::PlayerHandle;
use shepherd_media_ui::video;

/// The shared transport overlay in the Android app's default-theme colors.
const OVERLAY_THEME: video::OverlayTheme = video::OverlayTheme {
    text: egui::Color32::WHITE,
    slider_fill: egui::Color32::LIGHT_BLUE,
    slider_knob: egui::Color32::WHITE,
    button_fill: None,
};

/// How often to repaint while the overlay is on screen, so its elapsed-time
/// readout ticks. The video is not ours to draw, so nothing else needs a frame.
const OVERLAY_TICK: Duration = Duration::from_millis(33);

/// Idle cadence once the controls have hidden: slow enough to cost nothing,
/// frequent enough to notice input and playback ending promptly.
const IDLE_TICK: Duration = Duration::from_millis(250);

pub struct PlaybackView {
    last_input_at: Instant,
}

impl PlaybackView {
    pub fn new() -> Self {
        Self {
            last_input_at: Instant::now(),
        }
    }

    pub fn note_started(&mut self) {
        self.last_input_at = Instant::now();
    }

    /// Draw the overlay. Returns `true` if the user asked to leave playback
    /// (back button / Esc).
    pub fn draw(&mut self, ui: &mut egui::Ui, player: &mut dyn PlayerHandle, title: &str) -> bool {
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

        let controls_visible =
            self.last_input_at.elapsed() < video::CONTROLS_VISIBLE_FOR || player.is_paused();

        // Transparent, not black: the video SurfaceView is *behind* this
        // window, so any opaque fill here hides it.
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
            .show_inside(ui, |ui| {
                let rect = ui.max_rect();
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

        // Nothing here is tied to the video's frame rate any more, so the old
        // unconditional `request_repaint()` — which drove a full render pass at
        // display rate whether or not there was a new frame — is gone.
        ctx.request_repaint_after(if controls_visible {
            OVERLAY_TICK
        } else {
            IDLE_TICK
        });

        leave
    }
}

impl Default for PlaybackView {
    fn default() -> Self {
        Self::new()
    }
}
