//! Main GTK4 application for the launcher

use gtk4::glib;
use gtk4::prelude::*;
use lunchbox_util::EntryId;
use lunchbox_util::gamepad_nav::{NavDir, StickNav};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::client::{CommandClient, ServiceClient};
use crate::field::LauncherField;
use crate::state::{LauncherState, SharedState};
use crate::theme;

pub struct LauncherApp {
    socket_path: PathBuf,
}

impl LauncherApp {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub fn run(&self) -> i32 {
        let app = gtk4::Application::builder()
            .application_id("com.lunchboxos.launcher")
            .build();

        let socket_path = self.socket_path.clone();

        app.connect_activate(move |app| {
            Self::build_ui(app, socket_path.clone());
        });

        app.run().into()
    }

    fn build_ui(app: &gtk4::Application, socket_path: PathBuf) {
        // The stylesheet is written at the design size and multiplied for the
        // output it lands on, so it cannot be loaded once and forgotten: the
        // launcher is fullscreen on a screen whose size it does not know until
        // it is mapped. Loaded at 1.0 here and corrected in `track_scale`.
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(&theme::stylesheet(1.0));
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("Could not get default display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // Create main window
        let window = gtk4::ApplicationWindow::builder()
            .application(app)
            .title("Lunchbox Launcher")
            .default_width(1280)
            .default_height(720)
            .build();
        window.add_css_class("lb-launcher");

        // Make fullscreen
        window.fullscreen();

        // Create main stack for different views
        let stack = gtk4::Stack::new();
        stack.set_transition_type(gtk4::StackTransitionType::Crossfade);
        stack.set_transition_duration(300);

        // Create views
        let field = LauncherField::new();
        let loading_view = Self::create_loading_view();
        let error_view = Self::create_error_view();
        let session_view = Self::create_session_view();
        let disconnected_view = Self::create_disconnected_view();

        stack.add_named(&field, Some("field"));
        stack.add_named(&loading_view, Some("loading"));
        stack.add_named(&error_view.0, Some("error"));
        stack.add_named(&session_view.0, Some("session"));
        stack.add_named(&disconnected_view.0, Some("disconnected"));
        // Administrator mode's app picker (issue #154): the same grid the child
        // sees, over the system's `.desktop` files, with a search bar. Its own
        // grid instance rather than the child's, so the two launch paths cannot
        // be confused for one another — this one starts arbitrary programs.
        let admin_field = LauncherField::new();
        let admin_search = gtk4::SearchEntry::builder()
            .placeholder_text("Search applications")
            .hexpand(true)
            .build();
        admin_search.add_css_class("admin-search");
        let admin_view = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
        admin_view.add_css_class("admin-picker");
        admin_view.append(&admin_search);
        admin_view.append(&admin_field);
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
        Self::setup_keyboard_input(&window, &field, &admin_field, state.clone());
        Self::setup_admin_picker(
            &admin_field,
            &admin_search,
            command_client.clone(),
            runtime.clone(),
            state.clone(),
        );
        Self::setup_gamepad_input(
            &window,
            &field,
            command_client.clone(),
            runtime.clone(),
            state.clone(),
        );
        Self::track_scale(&window, &field, &provider);

        // Connect field launch callback
        let cmd_client = command_client.clone();
        let state_clone = state.clone();
        let rt = runtime.clone();
        field.connect_launch(move |entry_id| {
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
                        let now = lunchbox_util::now();
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
                            .map(lunchbox_util::SessionId::from_uuid)
                            .unwrap_or_else(|_| lunchbox_util::SessionId::new());
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
                                        groups: snapshot.groups,
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

        // Start lunchboxd client in background thread (separate from GTK main loop)
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
        let field_weak = field.downgrade();
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

                let field = field_weak.upgrade();
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
                    LauncherState::Idle { entries, groups } => {
                        if let Some(field) = field {
                            field.set_state(entries, groups);
                        }
                        if let Some(ref win) = window {
                            win.set_visible(true);
                        }
                        stack.set_visible_child_name("field");
                    }
                    LauncherState::Launching { entry_id: _ } => {
                        // Nothing to desensitise: every launch path checks the
                        // state before it acts, so the field being behind the
                        // loading view is already enough to make it inert.
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
        field: &LauncherField,
        search: &gtk4::SearchEntry,
        client: Arc<CommandClient>,
        runtime: Arc<Runtime>,
        state: SharedState,
    ) {
        /// The catalogue, once fetched. `None` while a fetch is outstanding.
        type Catalogue = Arc<std::sync::Mutex<Option<Vec<lunchbox_api::DesktopApp>>>>;
        let fetched: Catalogue = Arc::new(std::sync::Mutex::new(None));
        // The GTK-side copy, so typing filters without a round trip.
        let apps: Rc<RefCell<Vec<lunchbox_api::DesktopApp>>> = Rc::new(RefCell::new(Vec::new()));

        let client_for_launch = client.clone();
        let runtime_for_launch = runtime.clone();
        let state_for_launch = state.clone();
        field.connect_launch(move |entry_id| {
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

        let field_for_search = field.downgrade();
        let apps_for_search = apps.clone();
        search.connect_search_changed(move |entry| {
            let Some(field) = field_for_search.upgrade() else {
                return;
            };
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
            field.set_state(filtered, Self::picker_category());
        });

        let field_for_tick = field.downgrade();
        let search_for_tick = search.clone();
        let mut was_admin = false;
        glib::timeout_add_local(Duration::from_millis(300), move || {
            let is_admin = matches!(state.get(), LauncherState::AdminMode);
            if is_admin && !was_admin {
                search_for_tick.set_text("");
                if let Some(field) = field_for_tick.upgrade() {
                    field.set_state(Vec::new(), Self::picker_category());
                }
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
                if let Some(field) = field_for_tick.upgrade() {
                    field.set_state(views, Self::picker_category());
                }
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
    /// The one category the picker puts everything in.
    ///
    /// Administrator mode has no categories of its own — a `.desktop` file
    /// belongs to no group and carries no policy — so the picker borrows the
    /// child's field by handing it a single synthetic one. Everything the
    /// branding does then applies without being asked for: the sunk cream
    /// well, the selected cell, the item treatment, the scrolling and its
    /// fades. It is also the whole reason there is no second grid widget to
    /// keep in step with the first.
    fn picker_category() -> Vec<lunchbox_api::GroupView> {
        vec![lunchbox_api::GroupView {
            group_id: lunchbox_util::GroupId::new(Self::PICKER_GROUP),
            label: "Applications".to_string(),
            member_ids: Vec::new(),
            enabled: true,
            reasons: Vec::new(),
            used_today: Duration::ZERO,
            daily_quota: None,
            max_run_if_started_now: None,
            tokens: None,
            window_closes_at: None,
            earns_tokens: false,
        }]
    }

    /// The group id the picker's entries and its synthetic category share.
    const PICKER_GROUP: &'static str = "lunchbox:installed-applications";

    fn desktop_app_as_entry(app: &lunchbox_api::DesktopApp) -> lunchbox_api::EntryView {
        lunchbox_api::EntryView {
            entry_id: EntryId::new(&app.id),
            label: app.name.clone(),
            icon_ref: app.icon.clone(),
            kind_tag: lunchbox_api::EntryKindTag::Process,
            enabled: true,
            group: Some(lunchbox_util::GroupId::new(Self::PICKER_GROUP)),
            reasons: Vec::new(),
            tokens: None,
            // Not an activity, so nothing about it can earn or be earned.
            earns_tokens: false,
            max_run_if_started_now: None,
        }
    }

    fn setup_keyboard_input(
        window: &gtk4::ApplicationWindow,
        field: &LauncherField,
        admin_field: &LauncherField,
        state: SharedState,
    ) {
        let key_controller = gtk4::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let field_weak = field.downgrade();
        let admin_weak = admin_field.downgrade();
        key_controller.connect_key_pressed(move |_, key, _, _| {
            // Capture-phase, so this sees every key before anything else does.
            // Which field it steers depends on which one is on screen; in any
            // other state it steers nothing and lets the key through.
            let (field, in_picker) = match state.get() {
                LauncherState::Idle { .. } => (field_weak.upgrade(), false),
                LauncherState::AdminMode => (admin_weak.upgrade(), true),
                _ => return glib::Propagation::Proceed,
            };
            let Some(field) = field else {
                return glib::Propagation::Proceed;
            };

            // The picker has a search box, and a search box has to be able to
            // contain a space. The child's field has no text entry anywhere,
            // so space stays a second "launch this" there.
            if in_picker && key == gtk4::gdk::Key::space {
                return glib::Propagation::Proceed;
            }

            // WASD is a second D-pad for the child's field, where every key is
            // a button. In the picker those are letters someone is typing into
            // the search box, so only the arrows steer there.
            let wasd = !in_picker;
            let handled = match key {
                gtk4::gdk::Key::Up => {
                    field.move_selection(0, -1);
                    true
                }
                gtk4::gdk::Key::Down => {
                    field.move_selection(0, 1);
                    true
                }
                gtk4::gdk::Key::Left => {
                    field.move_selection(-1, 0);
                    true
                }
                gtk4::gdk::Key::Right => {
                    field.move_selection(1, 0);
                    true
                }
                gtk4::gdk::Key::w | gtk4::gdk::Key::W if wasd => {
                    field.move_selection(0, -1);
                    true
                }
                gtk4::gdk::Key::s | gtk4::gdk::Key::S if wasd => {
                    field.move_selection(0, 1);
                    true
                }
                gtk4::gdk::Key::a | gtk4::gdk::Key::A if wasd => {
                    field.move_selection(-1, 0);
                    true
                }
                gtk4::gdk::Key::d | gtk4::gdk::Key::D if wasd => {
                    field.move_selection(1, 0);
                    true
                }
                gtk4::gdk::Key::Return | gtk4::gdk::Key::KP_Enter | gtk4::gdk::Key::space => {
                    field.launch_selected();
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

    /// Keep the stylesheet and the field's geometry matched to the output.
    ///
    /// The window is fullscreen, so its real size arrives after it is mapped,
    /// and can change again if the device is docked to another display
    /// (issue #87). Both the CSS and the widget size requests are derived from
    /// that size, so both are redone whenever it changes — and only then, since
    /// reloading the stylesheet restyles every widget on the display.
    fn track_scale(
        window: &gtk4::ApplicationWindow,
        field: &LauncherField,
        provider: &gtk4::CssProvider,
    ) {
        let applied = Rc::new(std::cell::Cell::new(f64::NAN));
        let apply = {
            let field = field.downgrade();
            let provider = provider.clone();
            move |width: i32, height: i32| {
                let scale = theme::scale_for(width, height);
                if (scale - applied.get()).abs() < f64::EPSILON {
                    return;
                }
                applied.set(scale);
                debug!(width, height, scale, "Laying the field out for the output");
                provider.load_from_data(&theme::stylesheet(scale));
                if let Some(field) = field.upgrade() {
                    field.relayout();
                }
            }
        };

        let apply = Rc::new(apply);
        for property in ["default-width", "default-height"] {
            let apply = apply.clone();
            window.connect_notify_local(Some(property), move |win, _| {
                apply(win.width(), win.height());
            });
        }
        let apply_on_map = apply.clone();
        window.connect_map(move |win| {
            // The size is still the pre-fullscreen default at map time on some
            // compositors, so ask again once the frame has settled.
            let win = win.clone();
            let apply = apply_on_map.clone();
            glib::idle_add_local_once(move || apply(win.width(), win.height()));
        });
    }

    fn setup_gamepad_input(
        _window: &gtk4::ApplicationWindow,
        field: &LauncherField,
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

        let field_weak = field.downgrade();
        let cmd_client = command_client.clone();
        let rt = runtime.clone();
        let state_clone = state.clone();
        let mut stick_nav = StickNav::default();

        glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(field) = field_weak.upgrade() else {
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
                        gilrs::Button::DPadUp => field.move_selection(0, -1),
                        gilrs::Button::DPadDown => field.move_selection(0, 1),
                        gilrs::Button::DPadLeft => field.move_selection(-1, 0),
                        gilrs::Button::DPadRight => field.move_selection(1, 0),
                        gilrs::Button::South | gilrs::Button::East | gilrs::Button::Start => {
                            field.launch_selected();
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

            // Left stick: shared analog-nav logic with lunchbox-media so the
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
                        NavDir::Up => field.move_selection(0, -1),
                        NavDir::Down => field.move_selection(0, 1),
                        NavDir::Left => field.move_selection(-1, 0),
                        NavDir::Right => field.move_selection(1, 0),
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

    /// A card centred on the enamel. Every view that is not the field is one
    /// of these, so the launcher never shows bare text on a teal ground.
    fn create_card(spacing: i32) -> gtk4::Box {
        let card = gtk4::Box::new(gtk4::Orientation::Vertical, spacing);
        card.add_css_class("lb-card");
        card.set_halign(gtk4::Align::Center);
        card.set_valign(gtk4::Align::Center);
        card
    }

    fn create_loading_view() -> gtk4::Box {
        let container = Self::create_card(16);

        let spinner = gtk4::Spinner::new();
        spinner.set_spinning(true);
        spinner.add_css_class("lb-spinner");
        container.append(&spinner);

        let label = gtk4::Label::new(Some("Just a moment"));
        label.add_css_class("lb-message-title");
        container.append(&label);

        container
    }

    fn create_error_view() -> (gtk4::Box, gtk4::Label) {
        let container = Self::create_card(16);

        let title = gtk4::Label::new(Some("That didn't work"));
        title.add_css_class("lb-message-title");
        container.append(&title);

        let label = gtk4::Label::new(None);
        label.add_css_class("lb-message-body");
        label.set_wrap(true);
        label.set_max_width_chars(40);
        label.set_justify(gtk4::Justification::Center);
        container.append(&label);

        (container, label)
    }

    /// The screen shown over a session — both while the activity is starting
    /// and while it is being closed. Returns the headline and the sublabel,
    /// because the two states need different hints.
    fn create_session_view() -> (gtk4::Box, gtk4::Label, gtk4::Label) {
        let container = Self::create_card(16);

        let spinner = gtk4::Spinner::new();
        spinner.set_spinning(true);
        spinner.add_css_class("lb-spinner");
        container.append(&spinner);

        let label = gtk4::Label::new(Some("Loading..."));
        label.add_css_class("lb-message-title");
        container.append(&label);

        let hint = gtk4::Label::new(Some("Please wait while the application starts"));
        hint.add_css_class("lb-message-body");
        container.append(&hint);

        (container, label, hint)
    }

    fn create_disconnected_view() -> (gtk4::Box, gtk4::Button) {
        let container = Self::create_card(20);

        let label = gtk4::Label::new(Some("Not ready yet"));
        label.add_css_class("lb-message-title");
        container.append(&label);

        let hint = gtk4::Label::new(Some("Waiting for the Lunchbox service"));
        hint.add_css_class("lb-message-body");
        container.append(&hint);

        let retry_button = gtk4::Button::with_label("Try again");
        retry_button.add_css_class("lb-button");
        retry_button.set_halign(gtk4::Align::Center);
        container.append(&retry_button);

        (container, retry_button)
    }
}
