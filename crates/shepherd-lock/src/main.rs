//! `shepherd-lock` — the screen lock for administrator mode (issue #154).
//!
//! A caregiver who has entered administrator mode has a general-purpose desktop
//! in front of them, on a device whose whole design assumes the person at the
//! keyboard is a child. Walking away is a *supported* way to use the mode —
//! waiting for a Steam download on a slow connection is the motivating case —
//! so the mode cannot simply close itself. This is what makes that safe: the
//! work keeps running, and the screen cannot be touched until an administrator
//! unlocks it from the companion or web app.
//!
//! # Why this is not GTK, unlike every other shepherd surface
//!
//! It has to be a *real* session lock — `ext-session-lock-v1` — rather than a
//! `gtk4-layer-shell` overlay like the HUD and the pairing display.
//!
//! The deciding property is what happens when this process dies. Under the lock
//! protocol the compositor keeps the session locked and paints a blank screen;
//! there is no way back to the desktop. A layer surface, by contrast, is just a
//! window: anything that can reach the compositor can close it, and #148
//! recorded that happening for real —
//!
//! > an activity that issued `[app_id=org.shepherd.hud] kill` removed the HUD
//! > for the rest of the session
//!
//! The same command against a layer-shell "lock" would unlock the device. For a
//! control whose entire job is keeping a determined child out, failing *closed*
//! is the requirement, and only the session-lock protocol offers it.
//!
//! The cost is that there is no GTK binding for the protocol in this
//! generation, so this draws with cairo into a shared-memory buffer rather than
//! packing widgets. The content is two lines of text, which is why that trade is
//! affordable here and would not be for the HUD.
//!
//! # Unlocking
//!
//! shepherdd spawns this process and unlocks by sending **SIGTERM**, which is
//! handled: the lock is released with `unlock_and_destroy` and the process
//! exits. `SIGKILL` deliberately is not — it leaves the session locked with no
//! client to release it, recoverable only by restarting the compositor. That is
//! the safe direction, and it is why shepherdd must always ask politely.

use anyhow::{Context, Result};
use clap::Parser;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    output::{OutputHandler, OutputState},
    reexports::calloop::EventLoop,
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
    shm::{Shm, ShmHandler, raw::RawPool},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{error, info, warn};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm, wl_surface},
};

/// Set from the SIGTERM handler; the event loop polls it. A handler may do
/// almost nothing safely, and storing to an atomic is one of the things it may.
static UNLOCK_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigterm(_: i32) {
    UNLOCK_REQUESTED.store(true, Ordering::SeqCst);
}

#[derive(Parser, Debug)]
#[command(name = "shepherd-lock")]
#[command(about = "Session lock for shepherd's administrator mode", long_about = None)]
struct Args {
    /// Headline shown on the lock screen.
    #[arg(long, default_value = "Locked")]
    message: String,

    /// Second line, naming who can end this and how.
    #[arg(
        long,
        default_value = "Unlock from the Shepherd app on your phone or the management page"
    )]
    detail: String,

    #[arg(short, long, default_value = "info")]
    log_level: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&args.log_level)),
        )
        .init();

    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        nix::sys::signal::signal(
            nix::sys::signal::Signal::SIGTERM,
            nix::sys::signal::SigHandler::Handler(on_sigterm),
        )
        .context("install SIGTERM handler")?;
    }

    let conn = Connection::connect_to_env().context("connect to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn).context("initialize the registry")?;
    let qh: QueueHandle<App> = event_queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().context("create the event loop")?;

    let mut app = App {
        conn: conn.clone(),
        compositor: CompositorState::bind(&globals, &qh).context("bind wl_compositor")?,
        output_state: OutputState::new(&globals, &qh),
        registry_state: RegistryState::new(&globals),
        shm: Shm::bind(&globals, &qh).context("bind wl_shm")?,
        lock_state: SessionLockState::new(&globals, &qh),
        lock: None,
        surfaces: Vec::new(),
        message: args.message,
        detail: args.detail,
        exit: false,
        refused: false,
    };

    // Take the lock before creating any surface: until this succeeds there is
    // nothing to draw on, and a compositor without the protocol must fail here
    // rather than after the caller believes the screen is covered.
    let lock = app
        .lock_state
        .lock(&qh)
        .map_err(|_| anyhow::anyhow!("this compositor does not support ext-session-lock-v1"))?;
    app.lock = Some(lock);

    // One surface per output, or the compositor will not consider the session
    // locked — an unlocked output is an uncovered screen.
    for output in app.output_state.outputs() {
        let lock = app.lock.as_ref().expect("just set");
        let surface = app.compositor.create_surface(&qh);
        app.surfaces
            .push(lock.create_lock_surface(surface, &output, &qh));
    }

    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("insert the Wayland source: {e}"))?;

    while !app.exit {
        event_loop
            .dispatch(Duration::from_millis(50), &mut app)
            .context("dispatch the event loop")?;

        if UNLOCK_REQUESTED.swap(false, Ordering::SeqCst) {
            info!("SIGTERM received; releasing the lock");
            app.unlock();
        }
    }

    if app.refused {
        error!("the compositor refused the lock");
        std::process::exit(1);
    }
    Ok(())
}

struct App {
    conn: Connection,
    compositor: CompositorState,
    output_state: OutputState,
    registry_state: RegistryState,
    shm: Shm,
    lock_state: SessionLockState,
    lock: Option<SessionLock>,
    surfaces: Vec<SessionLockSurface>,
    message: String,
    detail: String,
    exit: bool,
    refused: bool,
}

impl App {
    /// Release the lock and stop.
    ///
    /// The roundtrip is load-bearing: without it the process can exit before
    /// the compositor has seen the destroy, and a lock client that dies with
    /// the lock still held leaves the session locked for good.
    fn unlock(&mut self) {
        if let Some(lock) = self.lock.take() {
            lock.unlock();
            if let Err(e) = self.conn.roundtrip() {
                warn!(error = %e, "roundtrip after unlocking failed; the screen may stay locked");
            }
            info!("Unlocked");
        }
        self.exit = true;
    }

    /// Paint the lock screen into a shared-memory buffer.
    ///
    /// cairo renders into its own image surface and the result is copied,
    /// rather than drawing directly into the pool: cairo wants to own its
    /// backing store, and at 1280x720 the copy is under 4 MB and happens only
    /// when the compositor asks for a configure.
    ///
    /// Both formats are premultiplied little-endian BGRA — cairo's `ARgb32` and
    /// wl_shm's `Argb8888` — so the copy needs no conversion.
    fn draw(&self, width: u32, height: u32) -> Option<Vec<u8>> {
        let surface =
            cairo::ImageSurface::create(cairo::Format::ARgb32, width as i32, height as i32).ok()?;
        {
            let cr = cairo::Context::new(&surface).ok()?;
            // The launcher's own background, so the lock reads as part of the
            // device rather than as a crash.
            cr.set_source_rgb(0.078, 0.086, 0.157);
            let _ = cr.paint();

            let cx = width as f64 / 2.0;
            let cy = height as f64 / 2.0;

            cr.select_font_face(
                "sans-serif",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Bold,
            );
            cr.set_font_size(48.0);
            cr.set_source_rgb(0.93, 0.94, 0.96);
            if let Ok(ext) = cr.text_extents(&self.message) {
                cr.move_to(cx - ext.width() / 2.0 - ext.x_bearing(), cy - 16.0);
                let _ = cr.show_text(&self.message);
            }

            cr.select_font_face(
                "sans-serif",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Normal,
            );
            cr.set_font_size(22.0);
            cr.set_source_rgb(0.62, 0.65, 0.72);
            if let Ok(ext) = cr.text_extents(&self.detail) {
                cr.move_to(cx - ext.width() / 2.0 - ext.x_bearing(), cy + 32.0);
                let _ = cr.show_text(&self.detail);
            }
        }
        surface.flush();
        let data = surface.take_data().ok()?;
        Some(data.to_vec())
    }
}

impl SessionLockHandler for App {
    fn locked(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        info!("Session locked");
    }

    fn finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        // The compositor will not honour the lock — most often because another
        // client already holds one. Exiting non-zero tells shepherdd the screen
        // is *not* covered, which must never be mistaken for success.
        self.refused = true;
        self.lock = None;
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        lock_surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let (width, height) = configure.new_size;
        if width == 0 || height == 0 {
            return;
        }
        let stride = width as i32 * 4;
        let Some(pixels) = self.draw(width, height) else {
            warn!("could not render the lock screen; painting it blank");
            return;
        };

        let Ok(mut pool) = RawPool::new(pixels.len(), &self.shm) else {
            warn!("could not allocate a shared-memory pool for the lock screen");
            return;
        };
        pool.mmap()[..pixels.len()].copy_from_slice(&pixels);
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
            (),
            qh,
        );
        lock_surface.wl_surface().attach(Some(&buffer), 0, 0);
        lock_surface.wl_surface().commit();
        buffer.destroy();
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }
    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _o: wl_output::WlOutput) {}
    fn update_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _o: wl_output::WlOutput) {}
    fn output_destroyed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _o: wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

smithay_client_toolkit::delegate_compositor!(App);
smithay_client_toolkit::delegate_output!(App);
smithay_client_toolkit::delegate_shm!(App);
smithay_client_toolkit::delegate_registry!(App);
smithay_client_toolkit::delegate_session_lock!(App);
// The buffer is destroyed the moment it is attached, so nothing needs its
// release event.
wayland_client::delegate_noop!(App: ignore wl_buffer::WlBuffer);
