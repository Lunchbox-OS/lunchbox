//! Shared video-playback UI building blocks.
//!
//! Both front-ends composite an mpv/`PlayerHandle` frame into the eframe surface
//! and draw a transport overlay. The GL compositing — an off-screen FBO-backed
//! texture the player renders into, exposed to egui as a texture — is identical,
//! as are the time formatting and the touch scrubber, so they live here. Each
//! binary keeps its own overlay layout, theme, input model, and Session- vs
//! `PlayerHandle`-driven playback on top.

use std::sync::Arc;

use egui::{Color32, TextureId};
use glow::HasContext;

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
