//! Shared video-playback UI building blocks.
//!
//! Both front-ends draw a transport overlay over the video, and the time
//! formatting, touch scrubber and key→intent mapping are identical, so they
//! live here. Each binary keeps its own overlay layout, theme, input model, and
//! Session- vs `PlayerHandle`-driven playback on top.
//!
//! How the video gets on screen is *not* shared any more:
//!
//! - the Linux binary composites it, and uses [`VideoCompositor`] — an
//!   off-screen FBO-backed texture that mpv renders into and egui paints;
//! - the Android app does not, because mpv decodes straight into a
//!   `SurfaceView` behind the window (`vo=mediacodec_embed`) and egui only
//!   paints the overlay over transparency. See issue #115: compositing through
//!   egui limited it to `hwdec=mediacodec-copy`, which reads every decoded
//!   frame back into system RAM.
//!
//! So [`VideoCompositor`] and [`paint_frame`] have a single caller today. They
//! stay here because the overlay they sit beside is shared, and because a
//! platform without a Surface-style output would need them again.

use std::sync::Arc;
use std::time::Duration;

use egui::{Color32, TextureId};
use glow::HasContext;
use lunchbox_media_core::Transport;

/// Seconds applied by the ±10s buttons and the seek keys/gamepad bindings.
pub const SEEK_DELTA_SECONDS: f64 = 10.0;
/// How long the control overlay stays visible after the last input event.
pub const CONTROLS_VISIBLE_FOR: Duration = Duration::from_secs(3);

/// How long the "skipped a sponsor" notice stays on screen (issue #159).
///
/// Long enough for a viewer to read why the video jumped, short enough that it
/// is gone before it becomes part of the picture.
pub const SKIP_NOTICE_FOR: Duration = Duration::from_secs(3);
/// Height (logical px) of the bottom control bar, sized for thumb taps.
const CONTROL_BAR_HEIGHT: f32 = 160.0;
/// Minimum hit-box size for a touch-friendly button.
const TOUCH_TARGET: f32 = 72.0;

/// An off-screen GL render target for video: the player renders into an
/// FBO-backed texture, which is then painted into the egui scene. The FBO +
/// texture are reallocated whenever the surface size changes.
pub struct VideoCompositor {
    gl: Arc<glow::Context>,
    surface: Option<GlSurface>,
}

struct GlSurface {
    fbo: glow::Framebuffer,
    texture: glow::Texture,
    size: [u32; 2],
    texture_id: TextureId,
}

impl VideoCompositor {
    pub fn new(gl: Arc<glow::Context>) -> Self {
        Self { gl, surface: None }
    }

    /// Size the off-screen target to `size_px` (reallocating and re-registering
    /// via `register` on a change), have `render` draw the current frame into it,
    /// and return the egui texture to paint. `render` receives the GL framebuffer
    /// name and the target width/height in pixels. `register` wraps the host's
    /// `eframe::Frame::register_native_glow_texture` and runs only on (re)alloc,
    /// which keeps this crate free of an `eframe` dependency.
    ///
    /// Returns `None` only before the very first allocation.
    pub fn composite(
        &mut self,
        size_px: [u32; 2],
        register: impl FnOnce(glow::Texture) -> TextureId,
        render: impl FnOnce(i32, i32, i32),
    ) -> Option<TextureId> {
        self.ensure_surface(size_px, register);
        let surface = self.surface.as_ref()?;
        // Bind our FBO so the player draws into the texture, not the back buffer
        // that egui is about to paint into.
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(surface.fbo));
        }
        render(
            framebuffer_to_gl(surface.fbo),
            surface.size[0] as i32,
            surface.size[1] as i32,
        );
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        Some(surface.texture_id)
    }

    fn ensure_surface(
        &mut self,
        target_size: [u32; 2],
        register: impl FnOnce(glow::Texture) -> TextureId,
    ) {
        let needs_alloc = match &self.surface {
            Some(s) => s.size != target_size,
            None => true,
        };
        if !needs_alloc {
            return;
        }

        // Free the old surface, if any.
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

        self.surface = Some(GlSurface {
            fbo,
            texture,
            size: target_size,
            texture_id: register(texture),
        });
    }
}

/// Paint the compositor's video texture to fill `rect`.
pub fn paint_frame(painter: &egui::Painter, rect: egui::Rect, texture: Option<TextureId>) {
    if let Some(texture) = texture {
        painter.image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// Format `seconds` as `m:ss` (or `h:mm:ss` past an hour); `--:--` for a
/// non-finite or negative value.
pub fn format_time(seconds: f64) -> String {
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

/// Touch-friendly horizontal slider used for the playback scrubber.
///
/// Returns `Some(new_value)` if the user is actively dragging this frame; the
/// caller pushes that value to the source of truth (mpv) immediately. The widget
/// reads the pointer position directly rather than relying on `egui::Slider`'s
/// drag-delta logic, which is unreliable on touchscreens, and tracks the finger
/// straight away so the knob doesn't snap back to the stale value between frames.
/// `fill` colors the filled track and `knob` the handle.
pub fn touch_slider(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    current: f64,
    range: (f64, f64),
    fill: Color32,
    knob: Color32,
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
    painter.rect_filled(track, 6.0, Color32::from_white_alpha(40));

    if range.1 > range.0 {
        let filled =
            egui::Rect::from_min_size(track.min, egui::vec2(track.width() * frac, track.height()));
        painter.rect_filled(filled, 6.0, fill);

        let knob_x = track.min.x + track.width() * frac;
        painter.circle_filled(egui::pos2(knob_x, track.center().y), 14.0, knob);
    }

    new_value
}

/// glow exposes `Framebuffer` as an opaque newtype; mpv wants the raw GL integer
/// name.
fn framebuffer_to_gl(fb: glow::Framebuffer) -> i32 {
    fb.0.get() as i32
}

/// Colors the transport overlay draws with — each front-end supplies its theme.
pub struct OverlayTheme {
    /// Title text.
    pub text: Color32,
    /// Filled portion of the scrubber track.
    pub slider_fill: Color32,
    /// Scrubber knob.
    pub slider_knob: Color32,
    /// Button fill; `None` uses egui's default button styling.
    pub button_fill: Option<Color32>,
}

/// What the user asked for via the overlay this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    /// No exit requested.
    None,
    /// The back button was tapped — the caller should leave/stop playback.
    Leave,
}

/// Draw the transport overlay (title + back, scrubber, ±10s / play-pause) over
/// `rect`, driving `transport`, and return whether the user asked to leave.
/// Shared by both front-ends; only the colors (`theme`) and the leave-handling
/// differ between them.
pub fn transport_overlay<T: Transport + ?Sized>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    transport: &mut T,
    title: &str,
    theme: &OverlayTheme,
) -> OverlayAction {
    let mut action = OverlayAction::None;

    // Header strip — title + back affordance.
    let header_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 96.0));
    ui.painter()
        .rect_filled(header_rect, 0.0, Color32::from_black_alpha(180));
    ui.painter().text(
        header_rect.left_center() + egui::vec2(32.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(28.0),
        theme.text,
    );

    let mut header_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(header_rect.shrink(16.0))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    if button(&mut header_ui, "‹ Back", theme.button_fill).clicked() {
        action = OverlayAction::Leave;
    }

    // Bottom control bar.
    let bar_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.max.y - CONTROL_BAR_HEIGHT),
        egui::vec2(rect.width(), CONTROL_BAR_HEIGHT),
    );
    ui.painter()
        .rect_filled(bar_rect, 0.0, Color32::from_black_alpha(180));

    let position = transport.position().unwrap_or(0.0);
    let duration = transport.duration().unwrap_or(0.0);

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
        theme.slider_fill,
        theme.slider_knob,
    ) {
        let _ = transport.seek_absolute(new_pos);
    }
    scrubber_ui.add_space(12.0);
    scrubber_ui.label(
        egui::RichText::new(format_time(duration))
            .monospace()
            .size(20.0),
    );

    // Button row: -10s / play-pause / +10s, centered. Volume is intentionally
    // absent — the global HUD owns volume, so duplicating it here would confuse.
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
    if button(&mut buttons_ui, "« 10s", theme.button_fill).clicked() {
        let _ = transport.seek_relative(-SEEK_DELTA_SECONDS);
    }
    buttons_ui.add_space(16.0);
    // Glyphs deliberately drawn from the Geometric Shapes block — bundled
    // NotoEmoji covers them, unlike the Dingbat-block "❚❚".
    let play_label = if transport.is_paused() {
        "▶  Play"
    } else {
        "▮▮  Pause"
    };
    if button(&mut buttons_ui, play_label, theme.button_fill).clicked() {
        toggle_pause(transport);
    }
    buttons_ui.add_space(16.0);
    if button(&mut buttons_ui, "10s »", theme.button_fill).clicked() {
        let _ = transport.seek_relative(SEEK_DELTA_SECONDS);
    }

    action
}

/// Paint the notice that a segment was just skipped.
///
/// Deliberately inert: no animation, no button, nothing to dismiss. It sits
/// above where the control bar would be so it does not collide with the
/// transport when both are up, and it is drawn whether or not the controls are
/// visible — the jump it explains happens with the overlay hidden.
pub fn skip_notice(painter: &egui::Painter, rect: egui::Rect, text: &str, theme: &OverlayTheme) {
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(22.0),
        theme.text,
    );
    let anchor = egui::pos2(
        rect.min.x + 32.0,
        rect.max.y - CONTROL_BAR_HEIGHT - 32.0 - galley.size().y,
    );
    let background =
        egui::Rect::from_min_size(anchor, galley.size()).expand2(egui::vec2(16.0, 10.0));
    painter.rect_filled(background, 8.0, Color32::from_black_alpha(180));
    painter.galley(anchor, galley, theme.text);
}

/// Toggle play/pause on any transport.
pub fn toggle_pause<T: Transport + ?Sized>(transport: &mut T) {
    let paused = transport.is_paused();
    let _ = transport.set_paused(!paused);
}

fn button(ui: &mut egui::Ui, label: &str, fill: Option<Color32>) -> egui::Response {
    let text = egui::RichText::new(label).size(22.0).strong();
    let mut button = egui::Button::new(text).corner_radius(12.0);
    if let Some(fill) = fill {
        button = button.fill(fill);
    }
    ui.add_sized(egui::vec2(TOUCH_TARGET * 2.0, TOUCH_TARGET), button)
}

/// A transport action derived from a key press.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportIntent {
    TogglePause,
    /// Seek by the carried delta (seconds; negative to rewind).
    Seek(f64),
    /// Leave playback (back / stop).
    Leave,
}

/// The keys the transport responds to, so a caller can poll each with
/// `Input::key_pressed` and dispatch via [`key_intent`].
pub const TRANSPORT_KEYS: &[egui::Key] = &[
    egui::Key::Space,
    egui::Key::K,
    egui::Key::Enter,
    egui::Key::ArrowLeft,
    egui::Key::J,
    egui::Key::ArrowRight,
    egui::Key::L,
    egui::Key::Escape,
    egui::Key::Backspace,
    egui::Key::BrowserBack,
];

/// Map a transport key to its intent. `seek_delta` is the ±seconds step.
/// `Enter` (D-pad center) toggles pause and `BrowserBack` (the Android remote
/// BACK) leaves; both are harmless on desktop, where they simply weren't bound.
pub fn key_intent(key: egui::Key, seek_delta: f64) -> Option<TransportIntent> {
    use egui::Key;
    match key {
        Key::Space | Key::K | Key::Enter => Some(TransportIntent::TogglePause),
        Key::ArrowLeft | Key::J => Some(TransportIntent::Seek(-seek_delta)),
        Key::ArrowRight | Key::L => Some(TransportIntent::Seek(seek_delta)),
        Key::Escape | Key::Backspace | Key::BrowserBack => Some(TransportIntent::Leave),
        _ => None,
    }
}
