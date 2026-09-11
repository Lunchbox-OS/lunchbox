//! Main GTK4 application for the launcher

use gtk4::glib;
use gtk4::prelude::*;
use shepherd_util::EntryId;
use shepherd_util::gamepad_nav::{NavDir, StickNav};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::client::{CommandClient, ServiceClient};
use crate::grid::LauncherGrid;
use crate::state::{LauncherState, SharedState};

/// CSS styling for the launcher
const LAUNCHER_CSS: &str = r#"
.admin-picker { padding: 32px 48px; }
.admin-search { font-size: 20px; padding: 10px 14px; }
window {
    background-color: #1a1a2e;
}

.launcher-grid {
    padding: 48px;
}

.launcher-tile {
    background: #16213e;
    background-color: #16213e;
    border-radius: 16px;
    padding: 16px;
    min-width: 140px;
    min-height: 140px;
    border: 2px solid transparent;
    transition: all 200ms ease;
    color: #e0e0e0;
    box-shadow: none;
}

.launcher-tile:hover {
    background: #1f3460;
    background-color: #1f3460;
    border-color: #4a90d9;
}

.launcher-tile:focus,
.launcher-tile:focus-visible {
    background: #1f3460;
    background-color: #1f3460;
    border-color: #ffd166;
}

.launcher-tile:active {
    background: #0f3460;
    background-color: #0f3460;
}

.launcher-tile:disabled {
    opacity: 0.4;
}

.tile-label {
    color: #e0e0e0;
    font-size: 14px;
    font-weight: 500;
}

.launcher-tile image {
    -gtk-icon-style: regular;
    color: #e0e0e0;
}

.status-label {
    color: #888888;
    font-size: 18px;
}

.error-label {
    color: #ff6b6b;
    font-size: 16px;
}

.launching-spinner {
    min-width: 64px;
    min-height: 64px;
}

.session-active-box {
    padding: 48px;
}

.session-label {
    color: #ffffff;
    font-size: 24px;
    font-weight: 600;
}

.session-sublabel {
    color: #888888;
    font-size: 16px;
}
"#;

pub struct LauncherApp {
    socket_path: PathBuf,
}

impl LauncherApp {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub fn run(&self) -> i32 {
        let app = gtk4::Application::builder()
            .application_id("org.shepherd.launcher")
            .build();

        let socket_path = self.socket_path.clone();

        app.connect_activate(move |app| {
            Self::build_ui(app, socket_path.clone());
        });

        app.run().into()
    }

    fn build_ui(app: &gtk4::Application, socket_path: PathBuf) {
        // Load CSS
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(LAUNCHER_CSS);
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("Could not get default display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // Create main window
        let window = gtk4::ApplicationWindow::builder()
            .application(app)
            .title("Shepherd Launcher")
            .default_width(1280)
            .default_height(720)
            .build();

        // Make fullscreen
        window.fullscreen();

        // Create main stack for different views
        let stack = gtk4::Stack::new();
        stack.set_transition_type(gtk4::StackTransitionType::Crossfade);
        stack.set_transition_duration(300);

        // Create views
        let grid = LauncherGrid::new();
        let loading_view = Self::create_loading_view();
        let error_view = Self::create_error_view();
        let session_view = Self::create_session_view();
        let disconnected_view = Self::create_disconnected_view();

        stack.add_named(&grid, Some("grid"));
        stack.add_named(&loading_view, Some("loading"));
        stack.add_named(&error_view.0, Some("error"));
        stack.add_named(&session_view.0, Some("session"));
        stack.add_named(&disconnected_view.0, Some("disconnected"));
        // Administrator mode's app picker (issue #154): the same grid the child
        // sees, over the system's `.desktop` files, with a search bar. Its own
        // grid instance rather than the child's, so the two launch paths cannot
        // be confused for one another — this one starts arbitrary programs.
        let admin_grid = LauncherGrid::new();
        let admin_search = gtk4::SearchEntry::builder()
            .placeholder_text("Search applications")
            .hexpand(true)
            .build();
        admin_search.add_css_class("admin-search");
        let admin_view = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
        admin_view.add_css_class("admin-picker");
        admin_view.append(&admin_search);
        admin_view.append(&admin_grid);
        stack.add_named(&admin_view, Some("admin"));

        window.set_child(Some(&stack));

        // Create shared state
        let state = SharedState::new();
        let state_receiver = state.subscribe();

        // Create tokio runtime for async operations
        let runtime = Arc::new(Runtime::new().expect("Failed to create tokio runtime"));

        // Create command channel
        let (_command_tx, command_rx) = mpsc::unbounded_channel();

        // Create command client for sending commands
        let command_client = Arc::new(CommandClient::new(&socket_path));
        Self::setup_keyboard_input(&window, &grid);
        Self::setup_admin_picker(
            &admin_grid,
            &admin_search,
            command_client.clone(),
            runtime.clone(),
            state.clone(),
        );
        Self::setup_gamepad_input(
            &window,
            &grid,
            command_client.clone(),
            runtime.clone(),
            state.clone(),
        );

        // Connect grid launch callback
        let cmd_client = command_client.clone();
        let state_clone = state.clone();
        let rt = runtime.clone();
        grid.connect_launch(move |entry_id| {
            // Only act while the grid is what the child is actually looking
            // at. Every input path funnels through here — keyboard, pointer
            // and the evdev-polled gamepad — so this is the one place that
            // covers them all.
            //
            // Without it, a press aimed at a running activity reaches the grid
            // behind it and starts something unintended. That is what happened
            // on 2026-08-20 (issue #136): a close that took 5s to complete left
            // a stale grid under the child's thumb, and their second press
            // launched Bitwig Studio.
            let current = state_clone.get();
            if !matches!(current, LauncherState::Idle { .. }) {
                debug!(
                    entry_id = %entry_id,
                    state = ?std::mem::discriminant(&current),
                    "Ignoring launch: the grid is not the active view"
                );
                return;
            }

            info!(entry_id = %entry_id, "Launch requested");
            state_clone.set(LauncherState::Launching {
                entry_id: entry_id.to_string(),
            });

            let client = cmd_client.clone();
            let state = state_clone.clone();
            let entry_id = entry_id.clone();
            rt.spawn(async move {
                match client.launch(&entry_id).await {
                    Ok(crate::client::LaunchOutcomeOwned::Approved {
                        session_id,
                        deadline,
                    }) => {
                        info!(session_id = %session_id, "Launch approved, setting SessionActive");
                        let now = shepherd_util::now();
                        let time_remaining = deadline.and_then(|d| {
                            if d > now {
                                (d - now).to_std().ok()
                            } else {
                                Some(std::time::Duration::ZERO)
                            }
                        });
                        // The wire form is a string; the launcher state
                        // uses a typed SessionId. Parse; if the server
                        // ever hands us something malformed we synthesise
                        // a fresh id and drive on rather than crashing
                        // the UI.
                        let session_id = uuid::Uuid::parse_str(&session_id)
                            .map(shepherd_util::SessionId::from_uuid)
                            .unwrap_or_else(|_| shepherd_util::SessionId::new());
                        state.set(LauncherState::SessionActive {
                            session_id,
                            entry_label: entry_id.to_string(),
                            time_remaining,
                        });
                    }
                    Ok(crate::client::LaunchOutcomeOwned::Denied { message }) => {
                        error!(message = %message, "Launch denied");
                        state.set(LauncherState::Error { message });
                    }
                    Err(e) => {
                        // Launch failed on server side (spawn error, entry
                        // not found, ...) — refresh state to recover.
                        error!(error = %e, "Launch failed on server");
                        match client.get_state().await {
                            Ok(snapshot) => {
                                if snapshot.current_session.is_some() {
                                    debug!("Session still active after spawn failure");
                                } else {
                                    state.set(LauncherState::Idle {
                                        entries: snapshot.entries,
                                    });
                                }
                            }
                            Err(re) => {
                                error!(error = %re, "Failed to get state after launch failure");
                                state.set(LauncherState::Error {
                                    message: format!("Launch failed: {}", e),
                                });
                            }
                        }
                    }
                }
            });
        });

        // Connect retry button
        let cmd_client = command_client.clone();
        let state_clone = state.clone();
        let rt = runtime.clone();
        disconnected_view.1.connect_clicked(move |_| {
            info!("Retry connection requested");
            state_clone.set(LauncherState::Connecting);

            let client = cmd_client.clone();
            let state = state_clone.clone();
            rt.spawn(async move {
                match client.get_state().await {
                    Ok(_) => {
                        // Will trigger state update
                    }
                    Err(e) => {
                        error!(error = %e, "Reconnect failed");
                        state.set(LauncherState::Disconnected);
                    }
                }
            });
        });

        // Start shepherdd client in background thread (separate from GTK main loop)
        // This ensures the tokio runtime is properly driven for event reception
        let state_for_client = state.clone();
        let socket_for_client = socket_path.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .expect("Failed to create tokio runtime for event loop");
            rt.block_on(async move {
                let client = ServiceClient::new(socket_for_client, state_for_client, command_rx);
                client.run().await;
            });
        });

        // Set up state change handler
        let stack_weak = stack.downgrade();
        let grid_weak = grid.downgrade();
        let window_weak = window.downgrade();
        let error_label = error_view.1.clone();
        let session_label = session_view.1.clone();
        let session_hint = session_view.2.clone();

        glib::spawn_future_local(async move {
            let mut receiver = state_receiver;

            loop {
                receiver.changed().await.ok();

                let state = receiver.borrow().clone();

                let Some(stack) = stack_weak.upgrade() else {
                    break;
                };

                let grid = grid_weak.upgrade();
                let window = window_weak.upgrade();

                match state {
                    LauncherState::Disconnected => {
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_name("disconnected");
                    }
                    LauncherState::Connecting => {
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_name("loading");
                    }
                    LauncherState::Idle { entries } => {
                        if let Some(grid) = grid {
                            grid.set_entries(entries);
                            grid.set_tiles_sensitive(true);
                            grid.grab_focus();
                        }
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_name("grid");
                    }
                    LauncherState::Launching { entry_id: _ } => {
                        if let Some(grid) = grid {
                            grid.set_tiles_sensitive(false);
                        }
                        stack.set_visible_child_name("loading");
                    }
                    LauncherState::Closing { entry_label } => {
                        // Same surface as the session view, so the grid stays
                        // out of reach while the activity is torn down.
                        session_label.set_text(&format!("Closing {}…", entry_label));
                        session_hint.set_text("Please wait while the activity closes");
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_name("session");
                    }
                    LauncherState::SessionActive {
                        session_id: _,
                        entry_label,
                        time_remaining: _,
                    } => {
                        session_label.set_text(&format!("Loading: {}", entry_label));
                        session_hint.set_text("Please wait while the application starts");
                        // Show the session view as a loading screen behind the game
                        // The game window will appear on top when it launches
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_name("session");
                    }
                    LauncherState::Error { message } => {
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        error_label.set_text(&message);
                        stack.set_visible_child_name("error");
                    }
                    LauncherState::AdminMode => {
                        // Non-interactive by construction: the grid is a
                        // different stack child, so nothing here can launch.
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_name("admin");
                    }
                    LauncherState::Suspending => {
                        // Static cover drawn before the screen freezes on
                        // suspend; replaced by fresh state on resume (issue #73).
                        // Switch with no transition so the cover appears
                        // instantly and fully opaque — a crossfade would leave
                        // the stale content showing through (and possibly be the
                        // frame that freezes) for the duration of the animation.
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_full("loading", gtk4::StackTransitionType::None);
                    }
                }
            }
        });

        window.present();
    }

    /// Populate administrator mode's picker and keep the search working.
    ///
    /// The catalogue is fetched once per entry into the mode rather than
    /// polled: `.desktop` files change when something is installed, which is
    /// itself something the caregiver does from here, and re-reading fifty
    /// files on a timer to catch that is the wrong trade. Leaving and
    /// re-entering the mode refreshes it.
    ///
    /// GTK widgets are not `Send`, so nothing here touches the grid from inside
    /// a tokio task: the fetch drops its result in a mutex and the GTK-side
    /// tick picks it up, which is the same shape the rest of this file uses for
    /// crossing that boundary.
    fn setup_admin_picker(
        grid: &LauncherGrid,
        search: &gtk4::SearchEntry,
        client: Arc<CommandClient>,
        runtime: Arc<Runtime>,
        state: SharedState,
    ) {
        /// The catalogue, once fetched. `None` while a fetch is outstanding.
        type Catalogue = Arc<std::sync::Mutex<Option<Vec<shepherd_api::DesktopApp>>>>;
        let fetched: Catalogue = Arc::new(std::sync::Mutex::new(None));
        // The GTK-side copy, so typing filters without a round trip.
        let apps: Rc<RefCell<Vec<shepherd_api::DesktopApp>>> = Rc::new(RefCell::new(Vec::new()));

        let client_for_launch = client.clone();
        let runtime_for_launch = runtime.clone();
        let state_for_launch = state.clone();
        grid.connect_launch(move |entry_id| {
            // This grid's "entry id" is a desktop file ID, which is what
            // `launch_desktop_app` takes. Guarded on the state as well as the
            // daemon's own gate, so a stale click cannot start something after
            // the mode has ended.
            if !matches!(state_for_launch.get(), LauncherState::AdminMode) {
                return;
            }
            let id = entry_id.to_string();
            let client = client_for_launch.clone();
            runtime_for_launch.spawn(async move {
                match client.launch_desktop_app(&id).await {
                    Ok(()) => info!(id = %id, "Launched from the administrator picker"),
                    Err(e) => error!(id = %id, error = %e, "Launch from the picker failed"),
                }
            });
        });

        let grid_for_search = grid.clone();
        let apps_for_search = apps.clone();
        search.connect_search_changed(move |entry| {
            let needle = entry.text().to_lowercase();
            let filtered: Vec<_> = apps_for_search
                .borrow()
                .iter()
                .filter(|a| {
                    needle.is_empty()
                        || a.name.to_lowercase().contains(&needle)
                        // The id catches what a display name would not — typing
                        // "kde" to find "Krita", whose id is org.kde.krita.
                        || a.id.to_lowercase().contains(&needle)
                })
                .map(Self::desktop_app_as_entry)
                .collect();
            grid_for_search.set_entries(filtered);
            grid_for_search.select_first();
        });

        let grid_for_tick = grid.clone();
        let search_for_tick = search.clone();
        let mut was_admin = false;
        glib::timeout_add_local(Duration::from_millis(300), move || {
            let is_admin = matches!(state.get(), LauncherState::AdminMode);
            if is_admin && !was_admin {
                search_for_tick.set_text("");
                grid_for_tick.set_entries(Vec::new());
                let client = client.clone();
                let slot = fetched.clone();
                runtime.spawn(async move {
                    match client.list_desktop_apps().await {
                        Ok(list) => *slot.lock().unwrap() = Some(list),
                        Err(e) => error!(error = %e, "Could not list applications for the picker"),
                    }
                });
            }
            was_admin = is_admin;

            // Apply a completed fetch on the GTK thread.
            if let Some(list) = fetched.lock().unwrap().take() {
                let views: Vec<_> = list.iter().map(Self::desktop_app_as_entry).collect();
                *apps.borrow_mut() = list;
                grid_for_tick.set_entries(views);
                grid_for_tick.select_first();
                search_for_tick.grab_focus();
            }
            glib::ControlFlow::Continue
        });
    }

    /// Present a `.desktop` application as a grid tile.
    ///
    /// The tile's "entry id" is the desktop file ID, which is what the picker's
    /// launch path takes. Everything policy-shaped is inert: these are not
    /// activities, have no limits, and are always launchable while the mode is
    /// on — the mode is the gate.
    fn desktop_app_as_entry(app: &shepherd_api::DesktopApp) -> shepherd_api::EntryView {
        shepherd_api::EntryView {
            entry_id: EntryId::new(&app.id),
            label: app.name.clone(),
            icon_ref: app.icon.clone(),
            kind_tag: shepherd_api::EntryKindTag::Process,
            enabled: true,
            group: None,
            reasons: Vec::new(),
            tokens: None,
            max_run_if_started_now: None,
        }
    }

    fn setup_keyboard_input(window: &gtk4::ApplicationWindow, grid: &LauncherGrid) {
        let key_controller = gtk4::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let grid_weak = grid.downgrade();
        key_controller.connect_key_pressed(move |_, key, _, _| {
            let Some(grid) = grid_weak.upgrade() else {
                return glib::Propagation::Proceed;
            };

            let handled = match key {
                gtk4::gdk::Key::Up | gtk4::gdk::Key::w | gtk4::gdk::Key::W => {
                    grid.move_selection(0, -1);
                    true
                }
                gtk4::gdk::Key::Down | gtk4::gdk::Key::s | gtk4::gdk::Key::S => {
                    grid.move_selection(0, 1);
                    true
                }
                gtk4::gdk::Key::Left | gtk4::gdk::Key::a | gtk4::gdk::Key::A => {
                    grid.move_selection(-1, 0);
                    true
                }
                gtk4::gdk::Key::Right | gtk4::gdk::Key::d | gtk4::gdk::Key::D => {
                    grid.move_selection(1, 0);
                    true
                }
                gtk4::gdk::Key::Return | gtk4::gdk::Key::KP_Enter | gtk4::gdk::Key::space => {
                    grid.launch_selected();
                    true
                }
                _ => false,
            };

            if handled {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key_controller);
    }

    fn setup_gamepad_input(
        _window: &gtk4::ApplicationWindow,
        grid: &LauncherGrid,
        command_client: Arc<CommandClient>,
        runtime: Arc<Runtime>,
        state: SharedState,
    ) {
        let mut gilrs = match gilrs::Gilrs::new() {
            Ok(gilrs) => gilrs,
            Err(e) => {
                warn!(error = %e, "Gamepad input unavailable");
                return;
            }
        };

        let grid_weak = grid.downgrade();
        let cmd_client = command_client.clone();
        let rt = runtime.clone();
        let state_clone = state.clone();
        let mut stick_nav = StickNav::default();

        glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(grid) = grid_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };

            // Gamepads are read straight from evdev via gilrs, so — unlike the
            // keyboard — these presses do not go through the compositor and
            // are not gated by which surface has focus. The grid must
            // therefore gate on its own state, or a press meant for a running
            // activity acts on the tile that happens to be selected behind it
            // (issue #136). Nav is harmless, but only act at all while we are
            // actually showing the grid.
            let showing_grid = matches!(state_clone.get(), LauncherState::Idle { .. });

            // Drain button events. Drop axis events on the floor — we poll
            // the stick directly below so we can drive auto-repeat from the
            // timer rather than depending on AxisChanged deltas.
            while let Some(event) = gilrs.next_event() {
                if let gilrs::EventType::ButtonPressed(button, _) = event.event {
                    if !showing_grid && button != gilrs::Button::Mode {
                        continue;
                    }
                    match button {
                        gilrs::Button::DPadUp => grid.move_selection(0, -1),
                        gilrs::Button::DPadDown => grid.move_selection(0, 1),
                        gilrs::Button::DPadLeft => grid.move_selection(-1, 0),
                        gilrs::Button::DPadRight => grid.move_selection(1, 0),
                        gilrs::Button::South | gilrs::Button::East | gilrs::Button::Start => {
                            grid.launch_selected();
                        }
                        gilrs::Button::Mode => {
                            Self::request_stop_current(
                                cmd_client.clone(),
                                rt.clone(),
                                state_clone.clone(),
                            );
                        }
                        _ => {}
                    }
                }
            }

            // Left stick: shared analog-nav logic with shepherd-media so the
            // two launcher UIs feel identical (deadzone crossing fires once,
            // then waits, then auto-repeats).
            if !showing_grid {
                return glib::ControlFlow::Continue;
            }
            if let Some((_id, gp)) = gilrs.gamepads().next() {
                let x = gp.value(gilrs::Axis::LeftStickX);
                let y = gp.value(gilrs::Axis::LeftStickY);
                if let Some(dir) = stick_nav.tick(x, y, Instant::now()) {
                    match dir {
                        NavDir::Up => grid.move_selection(0, -1),
                        NavDir::Down => grid.move_selection(0, 1),
                        NavDir::Left => grid.move_selection(-1, 0),
                        NavDir::Right => grid.move_selection(1, 0),
                    }
                }
            }

            glib::ControlFlow::Continue
        });
    }

    fn request_stop_current(
        command_client: Arc<CommandClient>,
        runtime: Arc<Runtime>,
        state: SharedState,
    ) {
        runtime.spawn(async move {
            match command_client.stop_current().await {
                Ok(()) => {
                    info!("stop_current acknowledged");
                }
                Err(e) => {
                    // "no active session" here is a benign race (the
                    // session already ended between the button press and
                    // the RPC hitting the server), not an error to surface.
                    let msg = e.to_string();
                    if msg.to_ascii_lowercase().contains("no active session") {
                        debug!("stop_current: no active session");
                    } else {
                        error!(error = %e, "stop_current failed");
                        state.set(LauncherState::Error {
                            message: format!("Failed to stop current activity: {}", e),
                        });
                    }
                }
            }
        });
    }

    fn create_loading_view() -> gtk4::Box {
        let container = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
        container.set_halign(gtk4::Align::Center);
        container.set_valign(gtk4::Align::Center);

        let spinner = gtk4::Spinner::new();
        spinner.set_spinning(true);
        spinner.add_css_class("launching-spinner");
        container.append(&spinner);

        let label = gtk4::Label::new(Some("Loading..."));
        label.add_css_class("status-label");
        container.append(&label);

        container
    }

    fn create_error_view() -> (gtk4::Box, gtk4::Label) {
        let container = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
        container.set_halign(gtk4::Align::Center);
        container.set_valign(gtk4::Align::Center);

        let icon = gtk4::Image::from_icon_name("dialog-error");
        icon.set_pixel_size(64);
        container.append(&icon);

        let label = gtk4::Label::new(Some("An error occurred"));
        label.add_css_class("error-label");
        label.set_wrap(true);
        label.set_max_width_chars(40);
        container.append(&label);

        (container, label)
    }

    /// The screen shown over a session — both while the activity is starting
    /// and while it is being closed. Returns the headline and the sublabel,
    /// because the two states need different hints.
    fn create_session_view() -> (gtk4::Box, gtk4::Label, gtk4::Label) {
        let container = gtk4::Box::new(gtk4::Orientation::Vertical, 24);
        container.set_halign(gtk4::Align::Center);
        container.set_valign(gtk4::Align::Center);
        container.add_css_class("session-active-box");

        let spinner = gtk4::Spinner::new();
        spinner.set_spinning(true);
        spinner.add_css_class("launching-spinner");
        container.append(&spinner);

        let label = gtk4::Label::new(Some("Loading..."));
        label.add_css_class("session-label");
        container.append(&label);

        let hint = gtk4::Label::new(Some("Please wait while the application starts"));
        hint.add_css_class("session-sublabel");
        container.append(&hint);

        (container, label, hint)
    }

    fn create_disconnected_view() -> (gtk4::Box, gtk4::Button) {
        let container = gtk4::Box::new(gtk4::Orientation::Vertical, 24);
        container.set_halign(gtk4::Align::Center);
        container.set_valign(gtk4::Align::Center);

        let icon = gtk4::Image::from_icon_name("network-offline");
        icon.set_pixel_size(64);
        container.append(&icon);

        let label = gtk4::Label::new(Some("System not ready"));
        label.add_css_class("status-label");
        container.append(&label);

        let retry_button = gtk4::Button::with_label("Retry");
        retry_button.add_css_class("launcher-tile");
        container.append(&retry_button);

        (container, retry_button)
    }
}
