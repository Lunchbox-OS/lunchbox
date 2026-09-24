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
//! The controls are the shared `lunchbox-media-ui` ones, the same as the
//! Linux binary's.

use std::time::{Duration, Instant};

use lunchbox_media_core::PlayerHandle;
use lunchbox_media_core::sponsorblock::Category;
use lunchbox_media_ui::video;

/// How often to repaint while the overlay is on screen, so its elapsed-time
/// readout ticks. The video is not ours to draw, so nothing else needs a frame.
const OVERLAY_TICK: Duration = Duration::from_millis(33);

/// Idle cadence once the controls have hidden: slow enough to cost nothing,
/// frequent enough to notice input and playback ending promptly.
const IDLE_TICK: Duration = Duration::from_millis(250);

pub struct PlaybackView {
    last_input_at: Instant,
    /// The most recent SponsorBlock skip and when it happened, so the viewer is
    /// told why the video jumped (issue #159). `None` once it has expired.
    skipped: Option<(Category, Instant)>,
}

impl PlaybackView {
    pub fn new() -> Self {
        Self {
            last_input_at: Instant::now(),
            skipped: None,
        }
    }

    pub fn note_started(&mut self) {
        self.last_input_at = Instant::now();
        self.skipped = None;
    }

    /// A SponsorBlock segment was just skipped; show why for a few seconds.
    ///
    /// Deliberately does *not* touch `last_input_at`: the skip is the player
    /// acting on its own, and summoning the whole transport overlay for it
    /// would put a control bar over the video nobody asked for.
    pub fn note_skipped(&mut self, category: Category) {
        self.skipped = Some((category, Instant::now()));
    }

    /// Draw the overlay. Returns `true` if the user asked to leave playback
    /// (back button / Esc).
    ///
    /// `video` is where the video sits inside `ui`, in points, or `None` before
    /// a file is open. Everything outside it is filled with black — see
    /// [`Self::paint_letterbox`].
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        player: &mut dyn PlayerHandle,
        title: &str,
        video: Option<crate::surface::VideoRect>,
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

        let controls_visible =
            self.last_input_at.elapsed() < video::CONTROLS_VISIBLE_FOR || player.is_paused();
        // Copied out of `self` so the closure below borrows neither.
        let skipped = self.skipped;
        let showing_notice = skipped.is_some_and(|(_, at)| at.elapsed() < video::SKIP_NOTICE_FOR);
        let mut expire_notice = false;

        // Transparent, not black: the video SurfaceView is *behind* this
        // window, so any opaque fill here hides it.
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
            .show_inside(ui, |ui| {
                let rect = ui.max_rect();
                Self::paint_letterbox(ui.painter(), rect, video);
                if controls_visible
                    && video::transport_overlay(ui, rect, player, title)
                        == video::OverlayAction::Leave
                {
                    leave = true;
                }

                match skipped {
                    Some((category, at)) if at.elapsed() < video::SKIP_NOTICE_FOR => {
                        video::skip_notice(
                            ui.painter(),
                            rect,
                            &format!("Skipped {}", category.label()),
                        );
                    }
                    Some(_) => expire_notice = true,
                    None => {}
                }
            });
        if expire_notice {
            self.skipped = None;
        }

        if any_input {
            self.last_input_at = Instant::now();
        }

        // Nothing here is tied to the video's frame rate any more, so the old
        // unconditional `request_repaint()` — which drove a full render pass at
        // display rate whether or not there was a new frame — is gone.
        ctx.request_repaint_after(if controls_visible || showing_notice {
            OVERLAY_TICK
        } else {
            IDLE_TICK
        });

        leave
    }
}

impl PlaybackView {
    /// Fill everything outside the video with black.
    ///
    /// The window is translucent — that is how the SurfaceView behind it shows
    /// through — so whatever this does not paint shows the home screen instead
    /// of a letterbox bar. Painting the *whole* panel is not an option either:
    /// an opaque fill over the video area would hide the video, and egui cannot
    /// punch a hole back through it. So the bars are painted as bars.
    ///
    /// With no video rectangle yet, nothing is painted: the surface is still
    /// filling the window, and black over all of it would hide the first frame.
    fn paint_letterbox(
        painter: &egui::Painter,
        rect: egui::Rect,
        video: Option<crate::surface::VideoRect>,
    ) {
        let Some(video) = video else { return };
        let black = egui::Color32::BLACK;
        let left = rect.min.x + video.x;
        let top = rect.min.y + video.y;
        let right = left + video.width;
        let bottom = top + video.height;

        for bar in [
            // Pillarbox, then letterbox: whichever pair is degenerate paints
            // nothing, so this covers both orientations without a branch.
            egui::Rect::from_min_max(rect.min, egui::pos2(left, rect.max.y)),
            egui::Rect::from_min_max(egui::pos2(right, rect.min.y), rect.max),
            egui::Rect::from_min_max(egui::pos2(left, rect.min.y), egui::pos2(right, top)),
            egui::Rect::from_min_max(egui::pos2(left, bottom), egui::pos2(right, rect.max.y)),
        ] {
            if bar.width() > 0.0 && bar.height() > 0.0 {
                painter.rect_filled(bar, 0.0, black);
            }
        }
    }
}

impl Default for PlaybackView {
    fn default() -> Self {
        Self::new()
    }
}
