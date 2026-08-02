//! egui-based UI: poster grid in `Browsing` state, embedded mpv player
//! with a touch- and controller-friendly overlay in `Playing` state.

mod playback;

use shepherd_media_ui::{grid, prompt, theme};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use shepherd_media_app::ResumeTracker;
use shepherd_media_core::{
    ClassifiedUri, Item, Session, SessionInput, SessionState, resolve_source,
};
use shepherd_util::gamepad_nav::{NavDir, StickNav};

use crate::posters::{self, PosterCache};
use crate::video_cache::VideoCache;

/// How long the "continue watching" offer waits for its item to show up in the
/// grid before lapsing.
///
/// With `--connectivity-check` the grid starts pessimistically empty of remote
/// items and fills in once the first probe lands (a few seconds). Waiting covers
/// that; lapsing afterwards keeps a card from appearing over a grid the viewer
/// has already started using.
const OFFER_WINDOW: Duration = Duration::from_secs(10);

/// The "continue watching" card in this binary's browse theme.
const PROMPT_THEME: prompt::PromptTheme = prompt::PromptTheme {
    // One step darker than the tiles so the card reads as its own surface and
    // its buttons stay distinguishable from it.
    panel: theme::BG,
    text: theme::TEXT,
    dim_text: theme::TEXT_DIM,
    button: theme::TILE,
    button_focused: theme::TILE_FOCUSED,
    focus_border: theme::FOCUS_BORDER,
};

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
) -> Result<ExitCause, eframe::Error> {
    let posters = posters::prefetch(session.library());

    // With `--resume` on, browse mode opens offering the item watched most
    // recently — provided it is still in the library.
    let resume_offer = match (&start_mode, &resume) {
        (StartMode::Browsing, Some(tracker)) => tracker
            .last_item_in(session.library().items.iter().map(|i| i.id.as_str()))
            .map(|id| id.to_string()),
        _ => None,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_fullscreen(true)
            .with_decorations(false)
            .with_title("shepherd-media"),
        ..Default::default()
    };

    let signaled = Arc::new(AtomicBool::new(false));
    let signaled_clone = signaled.clone();

    eframe::run_native(
        "shepherd-media",
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
                focused: 0,
                columns: 4,
                grid_scroll: grid::ScrollState::default(),
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
                prompt: prompt::ResumePrompt::new(),
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
/// [`shepherd_media_core::NativeDisplay`]).
fn native_display(cc: &eframe::CreationContext<'_>) -> Option<shepherd_media_core::NativeDisplay> {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

    match cc.display_handle().ok()?.as_raw() {
        RawDisplayHandle::Wayland(h) => Some(shepherd_media_core::NativeDisplay::Wayland(
            h.display.as_ptr(),
        )),
        RawDisplayHandle::Xlib(h) => {
            Some(shepherd_media_core::NativeDisplay::X11(h.display?.as_ptr()))
        }
        _ => None,
    }
}

struct App {
    session: Session,
    posters: PosterCache,
    gilrs: Option<gilrs::Gilrs>,
    /// Index into the *visible* item list for the current frame.
    focused: usize,
    columns: usize,
    grid_scroll: grid::ScrollState,
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
    /// Analog left-stick navigation state for the browse grid.
    stick_nav: StickNav,
    /// Saved playback positions, when `--resume` was passed. `None` turns the
    /// whole feature off: nothing is recorded and nothing is offered.
    resume: Option<ResumeTracker>,
    /// The item the "continue watching" card is offering, until the viewer
    /// answers it. `None` once answered (or when there was nothing to offer).
    resume_offer: Option<String>,
    /// How long to wait for that item to appear in the grid before letting the
    /// offer lapse — see [`OFFER_WINDOW`].
    resume_offer_until: Instant,
    /// Focus state of that card.
    prompt: prompt::ResumePrompt,
}

impl App {
    /// Build the list of items to display for the current frame.
    fn visible_items(&self) -> Vec<Item> {
        let info = shepherd_media_core::PlatformInfo::current();
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
                        .map(|c| c.cached_path(&item.id).is_some())
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
            // shepherdd ends the activity mid-film routinely (a time limit, a
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
        if !visible.is_empty() {
            self.focused = self.focused.min(visible.len() - 1);
        }

        // The "continue watching" card is modal: while it is up the grid still
        // paints (as its backdrop) but takes no input. It only goes up once its
        // item is actually listed (see `OFFER_WINDOW`).
        let offering = match self.resume_offer.as_deref() {
            Some(id) if visible.iter().any(|item| item.id == id) => true,
            Some(_) if Instant::now() < self.resume_offer_until => false,
            Some(_) => {
                self.resume_offer = None;
                false
            }
            None => false,
        };
        if !offering {
            self.handle_browse_input(ctx, &visible, &gamepad_events);
        }
        let title = self.session.library().title.clone();
        let posters = &self.posters;
        let selected = grid::draw(
            ui,
            &mut self.grid_scroll,
            &title,
            &visible,
            &mut self.focused,
            &mut self.columns,
            &|id| posters.get(id).cloned(),
        );
        if offering {
            self.draw_resume_prompt(ui, &visible, &gamepad_events);
        } else if let Some(id) = selected {
            self.start_item(&id);
        }

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl App {
    fn handle_browse_input(
        &mut self,
        ctx: &egui::Context,
        visible: &[Item],
        gamepad_events: &[gilrs::EventType],
    ) {
        let n = visible.len();
        if n == 0 {
            return;
        }
        let cols = self.columns.max(1);

        ctx.input(|input| {
            if input.key_pressed(egui::Key::ArrowRight) {
                self.move_focus(1, n);
            }
            if input.key_pressed(egui::Key::ArrowLeft) {
                self.move_focus_back(1);
            }
            if input.key_pressed(egui::Key::ArrowDown) {
                self.move_focus(cols, n);
            }
            if input.key_pressed(egui::Key::ArrowUp) {
                self.move_focus_back(cols);
            }
            if input.key_pressed(egui::Key::Enter) {
                self.activate(visible);
            }
            if input.key_pressed(egui::Key::Escape) {
                self.session.handle_input(SessionInput::ExitSession);
            }
        });

        for ev in gamepad_events {
            use gilrs::{Button, EventType};
            if let EventType::ButtonPressed(btn, _) = ev {
                match btn {
                    Button::DPadLeft => self.move_focus_back(1),
                    Button::DPadRight => self.move_focus(1, n),
                    Button::DPadUp => self.move_focus_back(cols),
                    Button::DPadDown => self.move_focus(cols, n),
                    Button::South => self.activate(visible),
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
                match dir {
                    NavDir::Up => self.move_focus_back(cols),
                    NavDir::Down => self.move_focus(cols, n),
                    NavDir::Left => self.move_focus_back(1),
                    NavDir::Right => self.move_focus(1, n),
                }
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

    fn move_focus(&mut self, step: usize, len: usize) {
        self.focused = (self.focused + step).min(len.saturating_sub(1));
    }

    fn move_focus_back(&mut self, step: usize) {
        self.focused = self.focused.saturating_sub(step);
    }

    fn activate(&mut self, visible: &[Item]) {
        let Some(item) = visible.get(self.focused) else {
            return;
        };
        let info = shepherd_media_core::PlatformInfo::current();
        if resolve_source(item, &info).is_none() {
            return;
        }
        let id = item.id.clone();
        self.start_item(&id);
    }

    /// Start `item_id`, from its saved position when `--resume` is on. Every
    /// path into playback goes through here so resuming isn't tied to one of
    /// them (tap, Enter, gamepad, or the "continue watching" card).
    fn start_item(&mut self, item_id: &str) {
        let start = self
            .resume
            .as_ref()
            .and_then(|tracker| tracker.start_position(item_id));
        self.session.set_start_position(start);
        self.session
            .handle_input(SessionInput::SelectItem(item_id.to_string()));
    }

    /// Draw the "continue watching" card over the grid and act on the answer.
    fn draw_resume_prompt(
        &mut self,
        ui: &mut egui::Ui,
        visible: &[Item],
        gamepad_events: &[gilrs::EventType],
    ) {
        let Some(item_id) = self.resume_offer.clone() else {
            return;
        };
        // The caller only draws while the item is listed; this is belt and
        // braces so the borrow below can't fail.
        let Some(item) = visible.iter().find(|i| i.id == item_id) else {
            return;
        };

        // Gamepad: the card's own handling covers pointer and keyboard (which
        // is what a remote's D-pad arrives as), so only the pad maps here.
        let mut action = prompt::PromptAction::None;
        for ev in gamepad_events {
            use gilrs::{Button, EventType};
            if let EventType::ButtonPressed(btn, _) = ev {
                match btn {
                    Button::DPadLeft => self.prompt.move_focus(-1),
                    Button::DPadRight => self.prompt.move_focus(1),
                    Button::South => action = self.prompt.focused_action(),
                    Button::East => action = prompt::PromptAction::Dismiss,
                    _ => {}
                }
            }
        }

        let position = self
            .resume
            .as_ref()
            .and_then(|tracker| tracker.start_position(&item_id));
        let duration = item.duration_seconds.map(|d| d as f64);
        let drawn = self.prompt.draw(
            ui,
            ui.max_rect(),
            &item.title,
            position,
            duration,
            &PROMPT_THEME,
        );
        if action == prompt::PromptAction::None {
            action = drawn;
        }

        match action {
            prompt::PromptAction::None => {}
            prompt::PromptAction::Resume => {
                self.resume_offer = None;
                self.start_item(&item_id);
            }
            prompt::PromptAction::Dismiss => {
                self.resume_offer = None;
                // Leave the grid focus on the item that was offered: it is
                // still the most likely thing the viewer wants.
                if let Some(idx) = visible.iter().position(|i| i.id == item_id) {
                    self.focused = idx;
                }
            }
        }
    }
}
