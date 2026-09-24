//! Shared video-playback UI building blocks.
//!
//! Both front-ends draw the same transport controls over the video — one
//! compartment floating at the bottom of the picture, in the Lunchbox
//! branding — and share the time formatting and the key→intent mapping, so
//! they live here. Each binary keeps its own input model and Session- vs
//! `PlayerHandle`-driven playback on top.
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

use egui::{Color32, Sense, TextureId};
use glow::HasContext;
use lunchbox_media_core::Transport;

use crate::theme::{self, Glyph, Scale, Weight};

/// Seconds applied by the ±10s buttons and the seek keys/gamepad bindings.
pub const SEEK_DELTA_SECONDS: f64 = 10.0;
/// How long the controls stay up after the last input event, while playing.
/// They never hide while paused.
pub const CONTROLS_VISIBLE_FOR: Duration = Duration::from_secs(4);

/// How long the "skipped a sponsor" notice stays on screen (issue #159).
///
/// Long enough for a viewer to read why the video jumped, short enough that it
/// is gone before it becomes part of the picture.
pub const SKIP_NOTICE_FOR: Duration = Duration::from_secs(3);

// The control bar's measurements (MEDIA.md §5), in design pixels.

/// Distance from the sides and the bottom of the picture.
const BAR_SIDE: f32 = 24.0;
const BAR_BOTTOM: f32 = 20.0;
const BAR_PAD: egui::Vec2 = egui::vec2(18.0, 12.0);
const BAR_GAP: f32 = 16.0;
const DISC: f32 = 44.0;
const MAIN_DISC: f32 = 60.0;
const META_W: f32 = 240.0;
const TRACK_H: f32 = 18.0;
const REMAINING_W: f32 = 46.0;
/// The controls are what a finger aims at on a phone, where the design's
/// field scales down furthest; below this they would be smaller than a
/// fingertip.
const MIN_SCALE: f32 = 0.75;

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

/// The seek track: a putty bar in an ink rim, yellow up to the position, with
/// an ink line where the yellow ends. Returns the position the viewer is
/// dragging it to, while they are.
///
/// Reads the pointer's absolute position rather than egui's drag delta, which
/// on a touchscreen carries the jump from the previous touch's release to the
/// new touch's start, and tracks the finger straight away so the fill does not
/// snap back to the stale position between frames.
fn seek_track(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    current: f64,
    range: (f64, f64),
    s: Scale,
) -> Option<f64> {
    // A taller hit area than the 18px track, for a thumb.
    let hit = rect.expand2(egui::vec2(0.0, s.px(14.0)));
    let response = ui.interact(hit, ui.id().with("seek-track"), Sense::click_and_drag());

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
    theme::paint_progress_bar(ui.painter(), rect, frac, s.px(theme::OUTLINE), true);
    new_value
}

/// glow exposes `Framebuffer` as an opaque newtype; mpv wants the raw GL integer
/// name.
fn framebuffer_to_gl(fb: glow::Framebuffer) -> i32 {
    fb.0.get() as i32
}

/// What the user asked for via the overlay this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    /// No exit requested.
    None,
    /// The back button was tapped — the caller should leave/stop playback.
    Leave,
}

/// Draw the transport controls over `rect` and drive `transport`: one
/// compartment floating at the bottom of the picture holding Back, the title
/// and where playback is, −10s, play/pause, +10s, the seek track and the time
/// left. Returns whether the viewer asked to leave.
///
/// Play/pause is drawn selected — yellow, lifted, with the ink offset shadow —
/// because it is what the remote's centre button does; the D-pad's left and
/// right seek, as the track would with the focus on it.
pub fn transport_overlay<T: Transport + ?Sized>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    transport: &mut T,
    title: &str,
) -> OverlayAction {
    let mut action = OverlayAction::None;
    let s = Scale(Scale::fit(rect.size()).0.max(MIN_SCALE));
    let bar = bar_rect(rect, s);
    let painter = ui.painter().clone();
    theme::paint_compartment(&painter, bar, s);

    let position = transport.position().unwrap_or(0.0);
    let duration = transport.duration().unwrap_or(0.0);

    let inner = bar.shrink2(s.vec(BAR_PAD.x + theme::OUTLINE, BAR_PAD.y + theme::OUTLINE));
    let mid = inner.center().y;
    let mut x = inner.left();
    let mut slot = |width: f32| {
        let r = egui::Rect::from_min_max(
            egui::pos2(x, mid - s.px(MAIN_DISC) / 2.0),
            egui::pos2(x + width, mid + s.px(MAIN_DISC) / 2.0),
        );
        x += width + s.px(BAR_GAP);
        r
    };
    let back = slot(s.px(DISC));
    let meta = slot(s.px(META_W));
    let rewind = slot(s.px(DISC));
    let play = slot(s.px(MAIN_DISC));
    let forward = slot(s.px(DISC));
    let remaining_w = s.px(REMAINING_W);
    let track = egui::Rect::from_min_max(
        egui::pos2(x, mid - s.px(TRACK_H) / 2.0),
        egui::pos2(
            inner.right() - remaining_w - s.px(BAR_GAP),
            mid + s.px(TRACK_H) / 2.0,
        ),
    );

    if disc_button(ui, &painter, back, s.px(DISC), Glyph::Back, false, s) {
        action = OverlayAction::Leave;
    }

    let title_galley = painter.layout(
        title.to_string(),
        theme::font(s.px(20.0), Weight::ExtraBold),
        theme::INK,
        f32::INFINITY,
    );
    let title_galley = elide_to(&painter, title, title_galley, meta.width(), s);
    let time = painter.layout_no_wrap(
        if duration > 0.0 {
            format!("{} of {}", format_time(position), format_time(duration))
        } else {
            format_time(position)
        },
        theme::font(s.px(13.0), Weight::Bold),
        theme::MUTED,
    );
    // Baloo 2's line box is tall for its letters; tuck the time up under the
    // title the way the mockup sets the two.
    let title_h = title_galley.size().y - s.px(5.0);
    let block = title_h + time.size().y;
    let top = mid - block / 2.0;
    painter.galley(egui::pos2(meta.left(), top), title_galley, theme::INK);
    painter.galley(egui::pos2(meta.left(), top + title_h), time, theme::MUTED);

    if disc_button(ui, &painter, rewind, s.px(DISC), Glyph::Rewind, false, s) {
        let _ = transport.seek_relative(-SEEK_DELTA_SECONDS);
    }
    let glyph = if transport.is_paused() {
        Glyph::Play
    } else {
        Glyph::Pause
    };
    if disc_button(ui, &painter, play, s.px(MAIN_DISC), glyph, true, s) {
        toggle_pause(transport);
    }
    if disc_button(ui, &painter, forward, s.px(DISC), Glyph::Forward, false, s) {
        let _ = transport.seek_relative(SEEK_DELTA_SECONDS);
    }

    if track.width() > 0.0
        && let Some(new_pos) = seek_track(ui, track, position, (0.0, duration), s)
    {
        let _ = transport.seek_absolute(new_pos);
    }

    let left = painter.layout_no_wrap(
        format_time((duration - position).max(0.0)),
        theme::font(s.px(14.0), Weight::Bold),
        theme::MUTED,
    );
    painter.galley(
        egui::pos2(inner.right() - left.size().x, mid - left.size().y / 2.0),
        left,
        theme::MUTED,
    );

    action
}

/// Where the control bar sits in `rect`: across the bottom, clear of the
/// edges, tall enough for the 60px play button.
fn bar_rect(rect: egui::Rect, s: Scale) -> egui::Rect {
    let height = s.px(MAIN_DISC + 2.0 * (BAR_PAD.y + theme::OUTLINE));
    egui::Rect::from_min_max(
        egui::pos2(
            rect.left() + s.px(BAR_SIDE),
            rect.bottom() - s.px(BAR_BOTTOM) - height,
        ),
        egui::pos2(
            rect.right() - s.px(BAR_SIDE),
            rect.bottom() - s.px(BAR_BOTTOM),
        ),
    )
}

/// A round button of `size` centred in `slot`. Returns whether it was tapped.
/// A pointer over it gives it the yellow of a selection, so a mouse sees what
/// it is about to press.
fn disc_button(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    slot: egui::Rect,
    size: f32,
    glyph: Glyph,
    selected: bool,
    s: Scale,
) -> bool {
    let rect = egui::Rect::from_center_size(slot.center(), egui::Vec2::splat(size));
    let response = ui.interact(
        rect,
        ui.id().with(("transport", glyph as u8)),
        Sense::click(),
    );
    let fill = if response.hovered() {
        theme::YELLOW
    } else {
        theme::COMPARTMENT
    };
    theme::paint_disc(painter, rect, glyph, fill, selected, s);
    response.clicked()
}

/// `galley` laid out on one line, cut to `width` with an ellipsis if it is
/// wider.
fn elide_to(
    painter: &egui::Painter,
    text: &str,
    galley: std::sync::Arc<egui::Galley>,
    width: f32,
    s: Scale,
) -> std::sync::Arc<egui::Galley> {
    if galley.size().x <= width {
        return galley;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_string(),
        theme::font(s.px(20.0), Weight::ExtraBold),
        theme::INK,
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// Paint the notice that a segment was just skipped.
///
/// Deliberately inert: no animation, no button, nothing to dismiss. It sits
/// above where the control bar would be so it does not collide with the
/// controls when both are up, and it is drawn whether or not they are visible —
/// the jump it explains happens with the controls hidden. An ink chip with
/// cream type, like a thumbnail's duration.
pub fn skip_notice(painter: &egui::Painter, rect: egui::Rect, text: &str) {
    let s = Scale(Scale::fit(rect.size()).0.max(MIN_SCALE));
    let bar = bar_rect(rect, s);
    let galley = painter.layout_no_wrap(
        text.to_string(),
        theme::font(s.px(18.0), Weight::Bold),
        theme::CREAM,
    );
    let pad = s.vec(14.0, 6.0);
    let size = galley.size() + 2.0 * pad;
    let chip = egui::Rect::from_min_size(
        egui::pos2(bar.left(), bar.top() - s.px(BAR_GAP) - size.y),
        size,
    );
    painter.rect_filled(chip, s.px(10.0), theme::INK);
    painter.galley(chip.min + pad, galley, theme::CREAM);
}

/// Toggle play/pause on any transport.
pub fn toggle_pause<T: Transport + ?Sized>(transport: &mut T) {
    let paused = transport.is_paused();
    let _ = transport.set_paused(!paused);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_sits_where_the_mockup_puts_it() {
        // media-player.png, on the 1280x664 field under the HUD: 24 from the
        // sides, 20 from the bottom, 92 tall.
        let field = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), theme::DESIGN_SIZE);
        let bar = bar_rect(field, Scale::fit(field.size()));
        assert_eq!(bar.left(), 24.0);
        assert_eq!(bar.right(), 1256.0);
        assert_eq!(bar.bottom(), 644.0);
        assert_eq!(bar.height(), 92.0);
    }

    #[test]
    fn times_read_as_clock_times() {
        assert_eq!(format_time(252.0), "4:12");
        assert_eq!(format_time(5088.0), "1:24:48");
        assert_eq!(format_time(f64::NAN), "--:--");
    }
}
