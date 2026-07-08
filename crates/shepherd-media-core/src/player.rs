//! Playback backend abstraction.
//!
//! The `PlayerHandle` trait is the seam that lets the same session state
//! machine drive Linux libmpv and a future Android JNI implementation.
//! Keep libmpv-specific types out of every other module — they belong here
//! and nowhere else.

use std::ffi::{CStr, c_void};

use thiserror::Error;

use crate::library::Source;

/// Function pointer lookup for OpenGL symbols, supplied by the host UI layer
/// (e.g. eframe/glutin) and forwarded to libmpv's render backend.
///
/// Type alias form is `&'static`-defaulted by the language; the trait method
/// in `PlayerHandle::bind_gl` takes `&dyn Fn(&CStr) -> *const c_void`
/// inline so callers can pass non-`'static` borrows. This alias exists only
/// for documentation in signatures that already supply a lifetime.
pub type GetProcAddress<'a> = dyn Fn(&CStr) -> *const c_void + 'a;

/// Operations on a media player. Calls are non-blocking with respect to
/// playback: `play` returns once mpv has accepted the command, not when
/// playback ends.
///
/// Transport-control methods and embedded-render hooks have default no-op
/// implementations so adapters (e.g. caching wrappers) can opt in to
/// delegating only the methods they care about.
pub trait PlayerHandle: Send {
    fn play(&mut self, source: &Source) -> Result<(), PlayerError>;
    fn stop(&mut self) -> Result<(), PlayerError>;
    fn is_playing(&self) -> bool;
    fn poll_event(&mut self) -> Option<PlayerEvent>;

    // -----------------------------------------------------------------
    // Transport controls. The default impls cover backends that don't
    // surface playback control to the UI.
    // -----------------------------------------------------------------

    fn set_paused(&mut self, _paused: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn is_paused(&self) -> bool {
        false
    }

    fn seek_relative(&mut self, _delta_seconds: f64) -> Result<(), PlayerError> {
        Ok(())
    }

    fn seek_absolute(&mut self, _seconds: f64) -> Result<(), PlayerError> {
        Ok(())
    }

    fn position(&self) -> Option<f64> {
        None
    }

    fn duration(&self) -> Option<f64> {
        None
    }

    fn set_volume(&mut self, _percent: f64) -> Result<(), PlayerError> {
        Ok(())
    }

    fn volume(&self) -> Option<f64> {
        None
    }

    /// Attach an external audio track to the *next* [`play`](Self::play) call,
    /// or clear it with `None`. Needed for sources whose video and audio are
    /// separate streams (e.g. a YouTube DASH video-only URL paired with an
    /// audio-only URL), where the platform resolves both and the player must
    /// mux them at playback. Applies once and is consumed by the next `play`.
    fn set_external_audio(&mut self, _url: Option<String>) {}

    // -----------------------------------------------------------------
    // Embedded rendering hooks. The host calls `bind_gl` once after its
    // OpenGL context is current, registers a redraw callback so it
    // knows when mpv has a new frame ready, and then drives `render`
    // each draw cycle.
    // -----------------------------------------------------------------

    /// Bind mpv's render context to the host's OpenGL context. Must be
    /// called from the GL thread, exactly once, before `render`.
    fn bind_gl(
        &mut self,
        _get_proc_address: &dyn Fn(&CStr) -> *const c_void,
    ) -> Result<(), PlayerError> {
        Ok(())
    }

    /// Render the current frame into `fbo` (use 0 for the default
    /// framebuffer). The host is expected to bind `fbo` itself before
    /// calling; mpv re-binds the framebuffer it is told about.
    fn render(&self, _fbo: i32, _width: i32, _height: i32) -> Result<(), PlayerError> {
        Ok(())
    }

    /// Register a callback that fires (potentially from a background
    /// thread) when mpv has a new frame ready. The host should use this
    /// to wake the UI thread so it can call `render`. Must be called
    /// after `bind_gl`.
    fn set_redraw_callback(&mut self, _cb: Box<dyn Fn() + Send + Sync + 'static>) {}
}

/// The playback-transport controls a UI overlay needs — the common subset of
/// [`PlayerHandle`] and [`Session`](crate::Session), which expose these methods
/// with identical signatures. Lets a shared overlay drive either one: the
/// Android app passes its `dyn PlayerHandle`, the Linux binary its `Session`.
pub trait Transport {
    fn is_paused(&self) -> bool;
    fn set_paused(&mut self, paused: bool) -> Result<(), PlayerError>;
    fn seek_relative(&mut self, delta_seconds: f64) -> Result<(), PlayerError>;
    fn seek_absolute(&mut self, seconds: f64) -> Result<(), PlayerError>;
    fn position(&self) -> Option<f64>;
    fn duration(&self) -> Option<f64>;
}

/// Every player is transport-controllable (`?Sized` so `dyn PlayerHandle`
/// qualifies). `Session` gets its own impl in `session.rs`.
impl<T: PlayerHandle + ?Sized> Transport for T {
    fn is_paused(&self) -> bool {
        PlayerHandle::is_paused(self)
    }
    fn set_paused(&mut self, paused: bool) -> Result<(), PlayerError> {
        PlayerHandle::set_paused(self, paused)
    }
    fn seek_relative(&mut self, delta_seconds: f64) -> Result<(), PlayerError> {
        PlayerHandle::seek_relative(self, delta_seconds)
    }
    fn seek_absolute(&mut self, seconds: f64) -> Result<(), PlayerError> {
        PlayerHandle::seek_absolute(self, seconds)
    }
    fn position(&self) -> Option<f64> {
        PlayerHandle::position(self)
    }
    fn duration(&self) -> Option<f64> {
        PlayerHandle::duration(self)
    }
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Started,
    EndOfFile,
    Error(String),
    Closed,
}

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("player backend error: {0}")]
    Backend(String),

    #[error("invalid source for backend: {0}")]
    InvalidSource(String),
}

/// Bounded retry policy for restarting playback after a transient
/// [`PlayerEvent::Error`]. A flaky stream (e.g. a network connection dropping
/// right after the file opens) usually ends the file with an error that clears
/// on a restart, so both front-ends recover a couple of times before giving up.
/// This holds only the *policy* (how many restarts remain); each front-end still
/// performs the restart and reports it in its own way — the Android app drives
/// its own event loop while the Linux binary goes through [`Session`](crate::Session).
#[derive(Debug, Clone)]
pub struct RetryBudget {
    used: u8,
    max: u8,
}

impl RetryBudget {
    /// Default number of restarts allowed before surfacing the error.
    pub const DEFAULT_MAX: u8 = 2;

    /// A fresh budget with [`RetryBudget::DEFAULT_MAX`] restarts available.
    pub fn new() -> Self {
        Self {
            used: 0,
            max: Self::DEFAULT_MAX,
        }
    }

    /// Refill the budget — call when a new item starts playing.
    pub fn reset(&mut self) {
        self.used = 0;
    }

    /// Consume one restart if any remain, returning whether the caller should
    /// retry (`true`) or give up and surface the error (`false`).
    pub fn try_retry(&mut self) -> bool {
        if self.used < self.max {
            self.used += 1;
            true
        } else {
            false
        }
    }
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod retry_budget_tests {
    use super::RetryBudget;

    #[test]
    fn allows_default_max_retries_then_gives_up() {
        let mut budget = RetryBudget::new();
        for _ in 0..RetryBudget::DEFAULT_MAX {
            assert!(
                budget.try_retry(),
                "restarts within the budget should retry"
            );
        }
        assert!(
            !budget.try_retry(),
            "once the budget is spent the caller should give up"
        );
    }

    #[test]
    fn reset_refills_the_budget() {
        let mut budget = RetryBudget::new();
        while budget.try_retry() {}
        assert!(!budget.try_retry());
        budget.reset();
        assert!(
            budget.try_retry(),
            "reset should make retries available again"
        );
    }
}

#[cfg(feature = "libmpv")]
pub use libmpv_backend::LibmpvPlayer;

#[cfg(feature = "libmpv")]
mod libmpv_backend {
    use std::ffi::{CStr, c_void};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use libmpv2::Mpv;
    use libmpv2::events::{Event, PropertyData};
    use libmpv2::render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType};

    use super::{PlayerError, PlayerEvent, PlayerHandle};
    use crate::library::{ClassifiedUri, Source};

    /// The libmpv2 init-params struct stores this as opaque context
    /// alongside our trampoline so mpv can ask "what's the address of
    /// glFoo?" during render context creation.
    type ProcAddrFn = dyn Fn(&CStr) -> *const c_void + 'static;

    // Threshold mpv log levels at which we surface a player Error event back
    // to the session. mpv numbers log levels with FATAL=10, ERROR=20, WARN=30,
    // INFO=40 and so on — lower is more severe.
    const MPV_LOG_LEVEL_ERROR: u32 = 20;

    /// Newtype holding the optional RenderContext so we can manually opt
    /// into Send. libmpv documents the render context as safe to move
    /// across threads; only `mpv_render_context_render` requires the GL
    /// thread, which the host enforces by only calling `render` from the
    /// UI thread.
    struct RenderCtxHolder(Option<RenderContext>);

    // SAFETY: see RenderCtxHolder comment. The raw pointer the
    // RenderContext wraps is not actually thread-local; libmpv only
    // restricts which thread calls `render`.
    unsafe impl Send for RenderCtxHolder {}

    /// libmpv-backed `PlayerHandle` that renders into a host OpenGL
    /// framebuffer rather than mpv's own window. The session constructs
    /// this up front; the UI layer calls `bind_gl` once a GL context is
    /// current and then drives `render` on each draw cycle.
    pub struct LibmpvPlayer {
        mpv: Mpv,
        playing: AtomicBool,
        // Created lazily by `bind_gl` and consulted by every later
        // `render` / `set_redraw_callback` call.
        render_ctx: Mutex<RenderCtxHolder>,
        // An external audio URL to attach to the next `play`, consumed there.
        // Used for separate video/audio streams (e.g. YouTube DASH).
        external_audio: Option<String>,
    }

    impl LibmpvPlayer {
        /// `fast_render` applies mpv's `fast` profile (bilinear scaling, no
        /// dither/deband). Weak GPUs — e.g. the Amlogic Mali in a Fire TV Stick —
        /// otherwise can't upscale to a 1080p output surface within a frame and
        /// present at a fraction of the display rate; the desktop binary leaves it
        /// off for full quality.
        pub fn new(ytdl_format: &str, fast_render: bool) -> Result<Self, PlayerError> {
            let mpv = Mpv::with_initializer(|init| {
                // `vo=libmpv` disables mpv's own windowing — the host UI
                // owns the surface and composites mpv's output via
                // RenderContext.
                init.set_property("vo", "libmpv")?;
                if fast_render {
                    // Best-effort: keep default quality if the profile is missing.
                    let _ = init.set_property("profile", "fast");
                }
                init.set_property("osc", "no")?;
                init.set_property("input-default-bindings", "no")?;
                init.set_property("input-vo-keyboard", "no")?;
                init.set_property("keep-open", "no")?;
                init.set_property("ytdl", "yes")?;
                init.set_property("ytdl-format", ytdl_format)?;
                // Hardware-accelerated decode where available; fall back
                // to software automatically.
                init.set_property("hwdec", "auto-safe")?;
                // Optional verbose mpv log to a file, for on-device debugging.
                if let Ok(path) = std::env::var("SHEPHERD_MPV_LOG") {
                    let _ = init.set_property("msg-level", "all=v");
                    let _ = init.set_property("log-file", path.as_str());
                }
                Ok(())
            })
            .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;

            // Observe `idle-active` so we can detect mpv returning to idle
            // (stop issued from the UI) as a Closed event.
            mpv.observe_property("idle-active", libmpv2::Format::Flag, 0)
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;

            Ok(Self {
                mpv,
                playing: AtomicBool::new(false),
                render_ctx: Mutex::new(RenderCtxHolder(None)),
                external_audio: None,
            })
        }

        fn uri_for_source(source: &Source) -> Result<String, PlayerError> {
            match &source.uri {
                ClassifiedUri::Local(path) => {
                    path.to_str().map(|s| s.to_string()).ok_or_else(|| {
                        PlayerError::InvalidSource(format!("non-UTF-8 path: {}", path.display()))
                    })
                }
                ClassifiedUri::DirectHttp(url)
                | ClassifiedUri::YouTube(url)
                | ClassifiedUri::Unknown(url) => Ok(url.to_string()),
            }
        }
    }

    /// libmpv's get_proc_address callback wraps a host-supplied
    /// `dyn Fn(&CStr) -> *const c_void`. The wrapper signature mpv
    /// expects takes a `&str`, so we re-CString here.
    fn proc_address_trampoline(getter: &&'static ProcAddrFn, name: &str) -> *mut c_void {
        let cname = match std::ffi::CString::new(name) {
            Ok(c) => c,
            Err(_) => return std::ptr::null_mut(),
        };
        (getter)(&cname) as *mut c_void
    }

    impl PlayerHandle for LibmpvPlayer {
        fn play(&mut self, source: &Source) -> Result<(), PlayerError> {
            let uri = Self::uri_for_source(source)?;
            // Discard events left over from a previous session before starting a
            // new one. `stop` makes mpv emit an `EndFile`, and the UI stops
            // draining events once it leaves the playback screen, so that event
            // (and the `idle-active` that follows) sit in the queue. Without this
            // the next `play` reads the stale `EndFile` as *this* file ending and
            // tears playback down immediately — and each teardown re-issues
            // `stop`, so playback stays stuck. (Reliably triggered by seeking and
            // then closing right away.)
            while self.mpv.wait_event(0.0).is_some() {}
            // An external audio track (separate video/audio streams) is attached
            // via the loadfile per-file options. The value is length-prefix
            // quoted (`%<len>%<str>`) so commas/colons in the URL don't get
            // parsed as option separators.
            match self.external_audio.take() {
                Some(audio) => {
                    let opts = format!("audio-file=%{}%{}", audio.len(), audio);
                    self.mpv
                        .command("loadfile", &[&uri, "replace", "0", &opts])
                        .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;
                }
                None => {
                    self.mpv
                        .command("loadfile", &[&uri, "replace"])
                        .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;
                }
            }
            // Reset pause state on every new playback.
            let _ = self.mpv.set_property("pause", false);
            self.playing.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn stop(&mut self) -> Result<(), PlayerError> {
            self.mpv
                .command("stop", &[])
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))
        }

        fn is_playing(&self) -> bool {
            self.playing.load(Ordering::SeqCst)
        }

        fn set_paused(&mut self, paused: bool) -> Result<(), PlayerError> {
            self.mpv
                .set_property("pause", paused)
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))
        }

        fn is_paused(&self) -> bool {
            self.mpv.get_property::<bool>("pause").unwrap_or(false)
        }

        fn seek_relative(&mut self, delta_seconds: f64) -> Result<(), PlayerError> {
            let arg = format!("{delta_seconds}");
            self.mpv
                .command("seek", &[&arg, "relative"])
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))
        }

        fn seek_absolute(&mut self, seconds: f64) -> Result<(), PlayerError> {
            let arg = format!("{seconds}");
            self.mpv
                .command("seek", &[&arg, "absolute"])
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))
        }

        fn position(&self) -> Option<f64> {
            self.mpv.get_property::<f64>("time-pos").ok()
        }

        fn duration(&self) -> Option<f64> {
            self.mpv.get_property::<f64>("duration").ok()
        }

        fn set_volume(&mut self, percent: f64) -> Result<(), PlayerError> {
            self.mpv
                .set_property("volume", percent.clamp(0.0, 100.0))
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))
        }

        fn volume(&self) -> Option<f64> {
            self.mpv.get_property::<f64>("volume").ok()
        }

        fn set_external_audio(&mut self, url: Option<String>) {
            self.external_audio = url;
        }

        fn bind_gl(
            &mut self,
            get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        ) -> Result<(), PlayerError> {
            // SAFETY: `OpenGLInitParams<C>` has no lifetime parameter, so the
            // compiler insists `C` be `'static`. In practice libmpv2 boxes the
            // params, hands them to mpv's `mpv_render_context_create`, and
            // frees the box before `RenderContext::new` returns (regardless of
            // success/failure). mpv itself resolves every GL function pointer
            // it needs inside that create call and does not retain the
            // get-proc-address callback. So the borrow is safely contained to
            // this stack frame even though the type system can't express that.
            let static_proc: &'static ProcAddrFn = unsafe { std::mem::transmute(get_proc_address) };

            let ctx = RenderContext::new(
                unsafe { self.mpv.ctx.as_mut() },
                vec![
                    RenderParam::ApiType(RenderParamApiType::OpenGl),
                    RenderParam::InitParams(OpenGLInitParams {
                        get_proc_address: proc_address_trampoline,
                        ctx: static_proc,
                    }),
                ],
            )
            .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))?;

            self.render_ctx.lock().unwrap().0 = Some(ctx);
            Ok(())
        }

        fn render(&self, fbo: i32, width: i32, height: i32) -> Result<(), PlayerError> {
            let guard = self.render_ctx.lock().unwrap();
            let Some(ctx) = guard.0.as_ref() else {
                return Err(PlayerError::Backend(
                    "render called before bind_gl".to_string(),
                ));
            };
            // `flip=false`: when rendering to the default GL framebuffer you
            // pass `true` to compensate for GL's Y-up vs video Y-down. We
            // render to an FBO that is then sampled by egui with UV (0,0) at
            // the top, which already swaps Y back; passing `true` would
            // double-flip and leave the video upside-down (the user reports
            // it as "rotated 180 and mirrored", which is what an upside-down
            // image with text inside looks like in everyday terms).
            ctx.render::<&'static ProcAddrFn>(fbo, width, height, false)
                .map_err(|e: libmpv2::Error| PlayerError::Backend(e.to_string()))
        }

        fn set_redraw_callback(&mut self, cb: Box<dyn Fn() + Send + Sync + 'static>) {
            if let Some(ctx) = self.render_ctx.lock().unwrap().0.as_mut() {
                ctx.set_update_callback(cb);
            }
        }

        fn poll_event(&mut self) -> Option<PlayerEvent> {
            let event = self.mpv.wait_event(0.0)?;
            match event {
                Ok(ev) => match ev {
                    Event::StartFile => Some(PlayerEvent::Started),
                    Event::EndFile(_) => {
                        self.playing.store(false, Ordering::SeqCst);
                        Some(PlayerEvent::EndOfFile)
                    }
                    Event::Shutdown => {
                        self.playing.store(false, Ordering::SeqCst);
                        Some(PlayerEvent::Closed)
                    }
                    Event::PropertyChange { name, change, .. } => match (name, change) {
                        ("idle-active", PropertyData::Flag(true)) => {
                            // Reaching idle without an explicit EOF means the
                            // UI issued a stop. Treat as Closed so the
                            // session machine returns to Browsing.
                            if self.playing.swap(false, Ordering::SeqCst) {
                                Some(PlayerEvent::Closed)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    },
                    Event::LogMessage {
                        log_level, text, ..
                    } if log_level <= MPV_LOG_LEVEL_ERROR => {
                        Some(PlayerEvent::Error(text.to_owned()))
                    }
                    _ => None,
                },
                Err(e) => Some(PlayerEvent::Error(e.to_string())),
            }
        }
    }
}
