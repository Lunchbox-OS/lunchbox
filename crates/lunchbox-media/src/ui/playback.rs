//! Playback view: composites mpv's render output into the eframe surface
//! and draws a touch- and controller-friendly control overlay.
//!
//! Lifecycle:
//!
//! - `PlaybackView::new` is called once, inside `eframe::App::new`'s
//!   creation closure, after `session.bind_gl` has been wired up.
//! - On every `update()` while the session is in `Playing` or
//!   `Stopping`, the host calls `draw`, which uses the shared
//!   `lunchbox-media-ui::video` compositor to render mpv's current frame
//!   into an off-screen texture, paints it full-screen, and draws the
//!   shared transport overlay (in this binary's theme) on top.
//! - Input (touch/click, keyboard, gamepad) is fed into `handle_input`
//!   before `draw` each frame.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use lunchbox_media_core::sponsorblock::Category;
use lunchbox_media_core::{Session, SessionInput};

use lunchbox_media_ui::theme;
use lunchbox_media_ui::video::{self, VideoCompositor};

/// The shared transport overlay in the Linux binary's theme colors.
const OVERLAY_THEME: video::OverlayTheme = video::OverlayTheme {
    text: theme::TEXT,
    slider_fill: theme::FOCUS_BORDER,
    slider_knob: theme::FOCUS_BORDER,
    button_fill: Some(theme::TILE_FOCUSED),
};

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
    /// The most recent SponsorBlock skip, and when it happened, so the viewer
    /// gets told why the video jumped. `None` once the notice has expired.
    skipped: Option<(Category, Instant)>,
}

impl PlaybackView {
    pub fn new(gl: Arc<glow::Context>, needs_render: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            compositor: VideoCompositor::new(gl),
            last_input_at: Instant::now(),
            item_title: String::new(),
            needs_render,
            skipped: None,
        }
    }

    pub fn note_item_started(&mut self, title: &str) {
        self.item_title = title.to_string();
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

            // Keyboard transport, via the shared key→intent mapping.
            for &key in video::TRANSPORT_KEYS {
                if !input.key_pressed(key) {
                    continue;
                }
                any_input = true;
                match video::key_intent(key, video::SEEK_DELTA_SECONDS) {
                    Some(video::TransportIntent::TogglePause) => video::toggle_pause(session),
                    Some(video::TransportIntent::Seek(delta)) => {
                        let _ = session.seek_relative(delta);
                    }
                    Some(video::TransportIntent::Leave) => {
                        session.handle_input(SessionInput::StopPlayback);
                    }
                    None => {}
                }
            }
            // Volume is intentionally not bound here — the global HUD's
            // volume keys (and gamepad bindings) handle that everywhere.
        });

        for ev in gamepad_events {
            use gilrs::{Button, EventType};
            if let EventType::ButtonPressed(btn, _) = ev {
                any_input = true;
                match btn {
                    Button::South => video::toggle_pause(session),
                    Button::East | Button::Start | Button::Select => {
                        session.handle_input(SessionInput::StopPlayback);
                    }
                    Button::DPadLeft | Button::LeftTrigger | Button::LeftTrigger2 => {
                        let _ = session.seek_relative(-video::SEEK_DELTA_SECONDS);
                    }
                    Button::DPadRight | Button::RightTrigger | Button::RightTrigger2 => {
                        let _ = session.seek_relative(video::SEEK_DELTA_SECONDS);
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
                let controls_visible = self.last_input_at.elapsed() < video::CONTROLS_VISIBLE_FOR
                    || session.is_paused();
                if controls_visible
                    && video::transport_overlay(ui, rect, session, &self.item_title, &OVERLAY_THEME)
                        == video::OverlayAction::Leave
                {
                    session.handle_input(SessionInput::StopPlayback);
                }

                match self.skipped {
                    Some((category, at)) if at.elapsed() < video::SKIP_NOTICE_FOR => {
                        video::skip_notice(
                            ui.painter(),
                            rect,
                            &format!("Skipped {}", category.label()),
                            &OVERLAY_THEME,
                        );
                    }
                    Some(_) => self.skipped = None,
                    None => {}
                }
            });

        // Keep redrawing while the overlay is visible (so the elapsed
        // time updates) and at a slower cadence otherwise.
        let showing_notice = self
            .skipped
            .is_some_and(|(_, at)| at.elapsed() < video::SKIP_NOTICE_FOR);
        let next = if showing_notice || self.last_input_at.elapsed() < video::CONTROLS_VISIBLE_FOR {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        };
        ctx.request_repaint_after(next);
    }
}
