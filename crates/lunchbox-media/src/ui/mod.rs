//! egui-based UI: the library screen in `Browsing` state, embedded mpv player
//! with a touch- and controller-friendly overlay in `Playing` state.

mod playback;

use lunchbox_media_ui::library::{self, Hero, LibraryView, Nav};
use lunchbox_media_ui::theme;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use lunchbox_media_app::ResumeTracker;
use lunchbox_media_core::{
    ClassifiedUri, Item, Session, SessionInput, SessionState, resolve_source,
};
use lunchbox_util::gamepad_nav::{NavDir, StickNav};

use crate::posters::{self, PosterCache};
use crate::skipping::SkipWatcher;
use lunchbox_media_cache::VideoCache;

/// How long the "keep watching" row waits for its item to show up in the
/// library before lapsing.
///
/// With `--connectivity-check` the grid starts pessimistically empty of remote
/// items and fills in once the first probe lands (a few seconds). Waiting covers
/// that; lapsing afterwards keeps the row from pushing the library down under a
/// viewer who has already started using it.
const OFFER_WINDOW: Duration = Duration::from_secs(10);

/// Why the UI loop returned.
#[derive(Debug, Clone, Copy)]
pub enum ExitCause {
    User,
    Signal,
}

/// Where the session starts when the UI is launched. `Browsing` is the
/// default (browse-mode activity); `Playing(item_id)` skips the grid for
/// direct-play activities.
pub enum StartMode {
    Browsing,
    Playing(String),
}

pub fn run(
    session: Session,
    term: Arc<AtomicBool>,
    online: Arc<AtomicBool>,
    cache: Option<Arc<VideoCache>>,
    start_mode: StartMode,
    resume: Option<ResumeTracker>,
    skipping: Option<SkipWatcher>,
) -> Result<ExitCause, eframe::Error> {
    let posters = posters::prefetch(session.library());

    // With `--resume` on, browse mode opens offering the item watched most
    // recently — provided it is still in the library, and was left partway
    // through: one watched to the end has nothing to continue.
    let resume_offer = match (&start_mode, &resume) {
        (StartMode::Browsing, Some(tracker)) => tracker
            .last_item_in(session.library().items.iter().map(|i| i.id.as_str()))
            .filter(|id| tracker.start_position(id).is_some())
            .map(|id| id.to_string()),
        _ => None,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_fullscreen(true)
            .with_decorations(false)
            .with_title("lunchbox-media"),
        ..Default::default()
    };

    let signaled = Arc::new(AtomicBool::new(false));
    let signaled_clone = signaled.clone();

    eframe::run_native(
        "lunchbox-media",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);

            let mut session = session;
            session.announce_ready();

            // Bind mpv's render context to the host GL context. This must
            // happen inside the eframe creation closure because that's
            // where `get_proc_address` is available.
            let native_display = native_display(cc);
            if native_display.is_none() {
                tracing::warn!(
                    "no native display handle available; mpv hardware decoding will fall back to a per-frame readback"
                );
            }
            if let Some(get_proc) = cc.get_proc_address.as_ref() {
                if let Err(e) = session.bind_gl(get_proc.as_ref(), native_display) {
                    tracing::error!("bind_gl failed: {e}");
                }
            } else {
                tracing::error!("eframe creation context did not expose get_proc_address");
            }

            // Mpv's wakeup fires on a background thread; flip an atomic
            // and ask egui to repaint so we render the new frame.
            let needs_render = Arc::new(AtomicBool::new(false));
            let needs_render_for_cb = needs_render.clone();
            let egui_ctx = cc.egui_ctx.clone();
            session.set_redraw_callback(Box::new(move || {
                needs_render_for_cb.store(true, Ordering::Relaxed);
                egui_ctx.request_repaint();
            }));

            let gl = cc
                .gl
                .as_ref()
                .expect("eframe must be configured with the glow backend")
                .clone();
            let playback = playback::PlaybackView::new(gl, needs_render);

            // For direct-play activities, dispatch the initial select
            // before the first frame so the UI opens in the playback
            // view rather than flashing the grid.
            if let StartMode::Playing(ref item_id) = start_mode {
                let start = resume.as_ref().and_then(|t| t.start_position(item_id));
                session.set_start_position(start);
                session.handle_input(SessionInput::SelectItem(item_id.clone()));
            }

            Ok(Box::new(App {
                session,
                posters,
                gilrs: gilrs::Gilrs::new().ok(),
                library: LibraryView::new(),
                term,
                signaled: signaled_clone,
                online,
                cache,
                playback,
                exit_after_playback: matches!(start_mode, StartMode::Playing(_)),
                playing_item: None,
                stick_nav: StickNav::default(),
                resume,
                resume_offer,
                resume_offer_until: Instant::now() + OFFER_WINDOW,
                skipping,
            }))
        }),
    )?;

    Ok(if signaled.load(Ordering::SeqCst) {
        ExitCause::Signal
    } else {
        ExitCause::User
    })
}

/// The windowing-system display handle behind the eframe window, which mpv
/// needs in order to bring up its VA-API interop (see
/// [`lunchbox_media_core::NativeDisplay`]).
fn native_display(cc: &eframe::CreationContext<'_>) -> Option<lunchbox_media_core::NativeDisplay> {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

    match cc.display_handle().ok()?.as_raw() {
        RawDisplayHandle::Wayland(h) => Some(lunchbox_media_core::NativeDisplay::Wayland(
            h.display.as_ptr(),
        )),
        RawDisplayHandle::Xlib(h) => {
            Some(lunchbox_media_core::NativeDisplay::X11(h.display?.as_ptr()))
        }
        _ => None,
    }
}

struct App {
    session: Session,
    posters: PosterCache,
    gilrs: Option<gilrs::Gilrs>,
    /// The library screen's focus and scroll.
    library: LibraryView,
    term: Arc<AtomicBool>,
    signaled: Arc<AtomicBool>,
    /// Latest connectivity status from the background check thread.
    /// `true` when online or when no connectivity check is configured.
    online: Arc<AtomicBool>,
    /// Video cache used to determine which remote items are available offline.
    cache: Option<Arc<VideoCache>>,
    playback: playback::PlaybackView,
    /// When `true` (direct-play mode), exit the app after playback ends
    /// rather than returning to the poster grid.
    exit_after_playback: bool,
    /// The item id currently being played, captured from the session state.
    /// Used to fire `PlaybackView::note_item_started` exactly once per
    /// playback rather than every frame (which would keep `last_input_at`
    /// fresh and prevent the HUD from ever auto-hiding).
    playing_item: Option<String>,
    /// Analog left-stick navigation state for the library screen.
    stick_nav: StickNav,
    /// Saved playback positions, when `--resume` was passed. `None` turns the
    /// whole feature off: nothing is recorded and nothing is offered.
    resume: Option<ResumeTracker>,
    /// The item the "keep watching" row offers, until anything is played.
    /// `None` from then on (or when there was nothing to offer).
    resume_offer: Option<String>,
    /// How long to wait for that item to appear in the library before letting
    /// the offer lapse — see [`OFFER_WINDOW`].
    resume_offer_until: Instant,
    /// SponsorBlock skipping, when a parent enabled it. `None` turns the whole
    /// feature off: nothing is looked up and nothing is skipped.
    skipping: Option<SkipWatcher>,
}

impl App {
    /// Build the list of items to display for the current frame.
    fn visible_items(&self) -> Vec<Item> {
        let info = lunchbox_media_core::PlatformInfo::current();
        let online = self.online.load(Ordering::Relaxed);
        self.session
            .library()
            .items
            .iter()
            .filter(|item| {
                let Some(source) = resolve_source(item, &info) else {
                    return false;
                };
                if online {
                    return true;
                }
                match &source.uri {
                    ClassifiedUri::Local(_) => true,
                    _ => self
                        .cache
                        .as_deref()
                        .map(|c| c.cached_path(source).is_some())
                        .unwrap_or(false),
                }
            })
            .cloned()
            .collect()
    }

    fn collect_gamepad_events(&mut self) -> Vec<gilrs::EventType> {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return Vec::new();
        };
        let mut ev = Vec::new();
        while let Some(event) = gilrs.next_event() {
            ev.push(event.event);
        }
        ev
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        if self.term.swap(false, Ordering::SeqCst) {
            // lunchboxd ends the activity mid-film routinely (a time limit, a
            // bedtime window closing), so this is a normal way to stop watching
            // — save the position before the player is torn down.
            if let Some(tracker) = self.resume.as_mut() {
                tracker.flush();
            }
            self.session.handle_input(SessionInput::SignalTerminate);
            self.signaled.store(true, Ordering::SeqCst);
        }

        self.session.tick();

        if self.session.is_exiting() {
            if let Some(tracker) = self.resume.as_mut() {
                tracker.flush();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        let gamepad_events = self.collect_gamepad_events();

        // Notice transitions into Playing so the playback overlay can
        // grab the current item's title for its header. Fire only on
        // transition (not every frame); otherwise the HUD's
        // last_input_at would be reset every frame and the controls
        // would never auto-hide.
        let now_playing = match self.session.state() {
            SessionState::Playing { item_id } => Some(item_id.clone()),
            _ => None,
        };
        if now_playing != self.playing_item {
            if let Some(ref id) = now_playing
                && let Some(item) = self.session.item_by_id(id)
            {
                self.playback.note_item_started(&item.title);
                // Start the segment lookup here rather than at play time: this
                // is the transition that knows *which* item, and the fetch runs
                // off-thread, so a cold lookup costs the opening seconds of the
                // video at worst.
                if let Some(watcher) = self.skipping.as_mut() {
                    watcher.note_item_started(item);
                }
            }
            if now_playing.is_none()
                && let Some(watcher) = self.skipping.as_mut()
            {
                watcher.note_stopped();
            }
            // Resume bookkeeping rides the same transition: a new item becomes
            // the one being tracked, and leaving `Playing` (EOF, stop, error)
            // writes out where it got to.
            if let Some(tracker) = self.resume.as_mut() {
                match now_playing {
                    Some(ref id) => tracker.note_started(id, Instant::now()),
                    None => tracker.finished(),
                }
            }
            self.playing_item = now_playing;
        }

        if matches!(
            self.session.state(),
            SessionState::Playing { .. } | SessionState::Stopping { .. }
        ) {
            // Before the resume bookkeeping, so a position saved this frame is
            // the one on the far side of a skip rather than inside it.
            if let Some(watcher) = self.skipping.as_mut()
                && let Some(skip) = watcher.poll(self.session.position(), self.session.duration())
            {
                match self.session.seek_absolute(skip.target) {
                    Ok(()) => self.playback.note_skipped(skip.category),
                    Err(e) => tracing::warn!("could not skip a SponsorBlock segment: {e}"),
                }
            }

            if let Some(tracker) = self.resume.as_mut() {
                tracker.progress(
                    self.session.position(),
                    self.session.duration(),
                    Instant::now(),
                );
                // Keep the session's restart point on the live position, so an
                // automatic retry after a transient stream error comes back
                // here rather than to the opening titles.
                if let Some(position) = tracker.live_position() {
                    self.session.set_start_position(Some(position));
                }
            }
            self.playback
                .handle_input(ctx, &mut self.session, &gamepad_events);
            self.playback.draw(ui, frame, &mut self.session);
            return;
        }

        // Browsing state: direct-play mode exits when the user lands
        // back here (after the single requested item finishes).
        if self.exit_after_playback {
            self.session.handle_input(SessionInput::ExitSession);
            return;
        }

        let visible = self.visible_items();

        // The "keep watching" row goes up once its item is actually listed
        // (see `OFFER_WINDOW`).
        let offering = match self.resume_offer.as_deref() {
            Some(id) if visible.iter().any(|item| item.id == id) => true,
            Some(_) if Instant::now() < self.resume_offer_until => false,
            Some(_) => {
                self.resume_offer = None;
                false
            }
            None => false,
        };
        self.handle_browse_input(ctx, &gamepad_events);

        let resume = self.resume.as_ref();
        let hero = offering
            .then(|| {
                let item = visible
                    .iter()
                    .find(|i| Some(i.id.as_str()) == self.resume_offer.as_deref())?;
                let saved = resume?.saved(&item.id)?;
                Some(Hero {
                    item,
                    position: saved.position_seconds,
                    duration: item
                        .duration_seconds
                        .map(|d| d as f64)
                        .or(saved.duration_seconds),
                })
            })
            .flatten();
        let title = self.session.library().title.clone();
        let posters = &self.posters;
        let chosen = self.library.draw(
            ui,
            &library::Library {
                title: &title,
                items: &visible,
                hero,
                poster: &|id| posters.get(id).cloned(),
                progress: &|item| {
                    let saved = resume?.saved(&item.id)?;
                    let duration = item
                        .duration_seconds
                        .map(|d| d as f64)
                        .or(saved.duration_seconds);
                    library::watched_fraction(saved.position_seconds, duration)
                },
            },
        );
        if let Some(id) = chosen {
            self.start_item(&id);
        }

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl App {
    /// Gamepad and Escape for the library screen. The arrow keys and Enter are
    /// the library view's own.
    fn handle_browse_input(&mut self, ctx: &egui::Context, gamepad_events: &[gilrs::EventType]) {
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.session.handle_input(SessionInput::ExitSession);
        }

        for ev in gamepad_events {
            use gilrs::{Button, EventType};
            if let EventType::ButtonPressed(btn, _) = ev {
                match btn {
                    Button::DPadLeft => self.library.navigate(Nav::Left),
                    Button::DPadRight => self.library.navigate(Nav::Right),
                    Button::DPadUp => self.library.navigate(Nav::Up),
                    Button::DPadDown => self.library.navigate(Nav::Down),
                    Button::South => self.library.activate(),
                    Button::East => self.session.handle_input(SessionInput::ExitSession),
                    _ => {}
                }
            }
        }

        // Left stick: same focus moves as the dpad, with auto-repeat while
        // held. Request a repaint while the stick is past the deadzone so
        // we keep ticking even when nothing else changes (the default
        // 100ms idle repaint would still work, but the faster cadence
        // matches the repeat interval).
        if let Some((sx, sy)) = self.read_left_stick() {
            let dir = self.stick_nav.tick(sx, sy, Instant::now());
            if self.stick_nav.is_active() {
                ctx.request_repaint_after(StickNav::REPEAT_INTERVAL);
            }
            if let Some(dir) = dir {
                self.library.navigate(match dir {
                    NavDir::Up => Nav::Up,
                    NavDir::Down => Nav::Down,
                    NavDir::Left => Nav::Left,
                    NavDir::Right => Nav::Right,
                });
            }
        }
    }

    /// Snapshot the connected gamepad's left stick. `None` if no gamepad is
    /// connected or gilrs isn't initialized.
    fn read_left_stick(&self) -> Option<(f32, f32)> {
        let gilrs = self.gilrs.as_ref()?;
        let (_id, gp) = gilrs.gamepads().next()?;
        Some((
            gp.value(gilrs::Axis::LeftStickX),
            gp.value(gilrs::Axis::LeftStickY),
        ))
    }

    /// Start `item_id`, from its saved position when `--resume` is on. Every
    /// path into playback goes through here so resuming isn't tied to one of
    /// them (tap, Enter, gamepad, or the "keep watching" row).
    ///
    /// Playing anything retires the "keep watching" row: it is how the
    /// library opens, not something to come back to.
    fn start_item(&mut self, item_id: &str) {
        self.resume_offer = None;
        let start = self
            .resume
            .as_ref()
            .and_then(|tracker| tracker.start_position(item_id));
        self.session.set_start_position(start);
        self.session
            .handle_input(SessionInput::SelectItem(item_id.to_string()));
    }
}
