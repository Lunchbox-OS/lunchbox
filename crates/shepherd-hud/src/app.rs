//! HUD Application
//!
//! The main GTK4 application for the HUD overlay.
//! Uses gtk4-layer-shell to create an always-visible overlay.

use crate::battery::BatteryStatus;
use crate::state::{SessionState, SharedState};
use crate::time_display::TimeDisplay;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use shepherd_ipc::IpcClient;
use shepherd_util::default_socket_path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use tokio::runtime::Runtime;

/// Send a one-shot RPC to shepherdd on a background thread. The HUD's
/// GTK main loop must never block on IPC, so each action button spins
/// up a short-lived Tokio runtime, connects, calls, exits. Errors are
/// logged (there is no UI surface to report them to). `action` runs
/// against a freshly-connected client and returns any error the caller
/// wants to see in the log.
fn spawn_action<F, Fut>(socket_path: PathBuf, label: &'static str, action: F)
where
    F: FnOnce(IpcClient) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = shepherd_ipc::IpcResult<()>> + Send,
{
    std::thread::spawn(move || {
        let rt = Runtime::new().expect("Failed to create runtime");
        rt.block_on(async move {
            match IpcClient::connect(&socket_path).await {
                Ok(client) => {
                    if let Err(e) = action(client).await {
                        tracing::error!("Failed to send {}: {}", label, e);
                    }
                }
                Err(e) => tracing::error!("Failed to connect to shepherdd: {}", e),
            }
        });
    });
}

/// Ask shepherdd to end the current session gracefully (the "X" button).
fn request_stop_current(socket_path: PathBuf) {
    tracing::info!("Requesting end session");
    spawn_action(socket_path, "stop_current", |mut client| async move {
        client.stop_current(shepherd_api::StopMode::Graceful).await
    });
}

/// Pixel size for all symbolic icons in the HUD bar at scale 1.0. The
/// timer in `build_hud_content` multiplies this by the current HUD scale
/// factor so icons stay at their usual physical size when shepherdd drops
/// the compositor scale for an XWayland activity.
const BASE_ICON_PIXEL_SIZE: i32 = 20;

/// Logical-pixel width of the volume slider at scale 1.0, scaled the same
/// way as the icon size above so the slider grows with the rest of the HUD.
const BASE_VOLUME_SLIDER_WIDTH: i32 = 100;

/// Logical-pixel width of the brightness slider at scale 1.0. Matches the
/// volume slider so the two indicators line up.
const BASE_BRIGHTNESS_SLIDER_WIDTH: i32 = 100;

/// The HUD application
pub struct HudApp {
    app: gtk4::Application,
    socket_path: PathBuf,
    anchor: String,
    height: i32,
}

impl HudApp {
    pub fn new(socket_path: PathBuf, anchor: String, height: i32) -> Self {
        let app = gtk4::Application::builder()
            .application_id("org.shepherd.hud")
            .build();

        Self {
            app,
            socket_path,
            anchor,
            height,
        }
    }

    pub fn run(&self) -> i32 {
        let socket_path = self.socket_path.clone();
        let anchor = self.anchor.clone();
        let height = self.height;

        self.app.connect_activate(move |app| {
            let state = SharedState::new();
            let window = build_hud_window(app, &anchor, height, state.clone());

            // Start the IPC event listener
            let state_clone = state.clone();
            let socket_clone = socket_path.clone();
            std::thread::spawn(move || {
                if let Err(e) = run_event_loop(socket_clone, state_clone) {
                    tracing::error!("Event loop error: {}", e);
                }
            });

            // Subscribe to state changes
            let window_clone = window.clone();
            let state_clone = state.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let session_state = state_clone.session_state();
                let visible = session_state.is_visible();
                window_clone.set_visible(visible);
                glib::ControlFlow::Continue
            });

            window.present();
        });

        self.app.run().into()
    }
}

fn build_hud_window(
    app: &gtk4::Application,
    anchor: &str,
    height: i32,
    state: SharedState,
) -> gtk4::ApplicationWindow {
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .default_height(height)
        .decorated(false)
        .build();

    // CSS provider for the HUD's stylesheet. We install it once and rewrite
    // its contents whenever the UI scale factor changes (see HudScaleChanged
    // and `apply_scale` below) so font/padding sizes follow the factor
    // without needing to reload a fresh provider on the display.
    let css_provider = install_css_provider();

    // Initialize layer shell
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace("shepherd-hud");

    // Remove all margins from the layer-shell surface
    window.set_margin(Edge::Top, 0);
    window.set_margin(Edge::Bottom, 0);
    window.set_margin(Edge::Left, 0);
    window.set_margin(Edge::Right, 0);

    // Set anchors based on position
    match anchor {
        "bottom" => {
            window.set_anchor(Edge::Bottom, true);
            window.set_anchor(Edge::Left, true);
            window.set_anchor(Edge::Right, true);
        }
        _ => {
            // Default to top
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Left, true);
            window.set_anchor(Edge::Right, true);
        }
    }

    // Build the HUD content. apply_scale (below) is responsible for the
    // dynamic dimensions (default height, exclusive zone, font/padding) so
    // they stay in sync with the current UI scale factor.
    let content = build_hud_content(state.clone(), css_provider.clone(), window.clone(), height);
    window.set_child(Some(&content));

    // Populate the stylesheet and set initial dimensions at scale 1.0
    // before the window maps.
    apply_scale(&css_provider, &window, height, 1.0);

    window
}

fn build_hud_content(
    state: SharedState,
    css_provider: gtk4::CssProvider,
    window: gtk4::ApplicationWindow,
    base_height: i32,
) -> gtk4::Box {
    let container = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(16)
        .hexpand(true)
        .build();

    container.add_css_class("hud-bar");

    // Left section: App name and time
    let left_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .hexpand(true)
        .halign(gtk4::Align::Start)
        .build();

    let app_label = gtk4::Label::new(Some("No session"));
    app_label.add_css_class("app-name");
    left_box.append(&app_label);

    let time_display = TimeDisplay::new();
    left_box.append(&time_display);

    container.append(&left_box);

    // Center section: Warning banner (hidden by default)
    let warning_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::Center)
        .visible(false)
        .build();

    let warning_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
    warning_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    warning_box.append(&warning_icon);

    let warning_label = gtk4::Label::new(Some("Time running out!"));
    warning_label.add_css_class("warning-text");
    warning_box.append(&warning_label);

    warning_box.add_css_class("warning-banner");
    container.append(&warning_box);

    // Right section: System indicators and close button
    let right_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .build();

    // Wall clock display (shows mock time indicator in debug builds)
    let clock_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(4)
        .build();

    let clock_label = gtk4::Label::new(Some("--:--"));
    clock_label.add_css_class("clock-label");
    clock_box.append(&clock_label);
    let mut clock_format_full = false;

    // Add mock indicator if mock time is active (debug builds only)
    #[cfg(debug_assertions)]
    {
        if shepherd_util::is_mock_time_active() {
            let mock_indicator = gtk4::Label::new(Some("(MOCK)"));
            mock_indicator.add_css_class("mock-time-indicator");
            clock_box.append(&mock_indicator);
            clock_format_full = true;
        }
    }

    right_box.append(&clock_box);

    // Volume control with slider
    let volume_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(4)
        .build();
    volume_box.add_css_class("volume-control");

    // Mute button. Use an explicit child Image so its pixel size follows
    // the HUD scale factor (see `apply_scale`). `Button::set_icon_name`
    // would replace this child, so the timer below updates `volume_icon`
    // directly via `set_from_icon_name`.
    let volume_icon = gtk4::Image::from_icon_name("audio-volume-medium-symbolic");
    volume_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let volume_button = gtk4::Button::builder()
        .child(&volume_icon)
        .has_frame(false)
        .tooltip_text("Toggle mute")
        .build();
    volume_button.add_css_class("indicator-button");
    volume_button.connect_clicked(|_| {
        if let Err(e) = crate::volume::toggle_mute() {
            tracing::error!("Failed to toggle mute: {}", e);
        }
    });
    volume_box.append(&volume_button);

    // Volume slider. The `width_request` is rescaled by the timer below to
    // follow the HUD scale factor (see `BASE_VOLUME_SLIDER_WIDTH`).
    let volume_slider = gtk4::Scale::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .width_request(BASE_VOLUME_SLIDER_WIDTH)
        .draw_value(false)
        .build();
    volume_slider.set_range(0.0, 100.0);
    volume_slider.set_increments(5.0, 10.0);
    volume_slider.add_css_class("volume-slider");

    // Set initial value from shepherdd
    if let Some(info) = crate::volume::get_volume_status() {
        volume_slider.set_value(info.percent as f64);
    }

    // Handle slider value changes with debouncing
    // Create a channel for volume requests - the worker will debounce them
    let (volume_tx, volume_rx) = mpsc::channel::<u8>();

    // Spawn a dedicated volume worker thread that debounces requests
    std::thread::spawn(move || {
        const DEBOUNCE_MS: u64 = 50; // Wait 50ms for more changes before sending

        while let Ok(mut latest_percent) = volume_rx.recv() {
            // Drain any pending requests, keeping only the latest value
            // Use a short timeout to debounce rapid changes
            loop {
                match volume_rx.recv_timeout(std::time::Duration::from_millis(DEBOUNCE_MS)) {
                    Ok(percent) => {
                        latest_percent = percent; // Update to latest value
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // No more changes for DEBOUNCE_MS, send the request
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return; // Channel closed
                    }
                }
            }

            // Send only the final value
            if let Err(e) = crate::volume::set_volume(latest_percent) {
                tracing::error!("Failed to set volume: {}", e);
            }
        }
    });

    let slider_changing = std::rc::Rc::new(std::cell::Cell::new(false));
    let slider_changing_clone = slider_changing.clone();

    volume_slider.connect_change_value(move |slider, _, value| {
        slider_changing_clone.set(true);
        let percent = value.clamp(0.0, 100.0) as u8;

        // Send to debounce worker (non-blocking)
        let _ = volume_tx.send(percent);

        // Allow the slider to update immediately in UI
        slider.set_value(value);
        glib::Propagation::Stop
    });

    volume_box.append(&volume_slider);

    // Volume percentage label
    let volume_label = gtk4::Label::new(Some("--%"));
    volume_label.add_css_class("volume-label");
    volume_label.set_width_chars(4);
    volume_box.append(&volume_label);

    right_box.append(&volume_box);

    // Brightness control. Hidden when the host has no backlight (every
    // desktop machine, plus laptops missing `/sys/class/backlight/*`); on
    // hosts with one this is the laptop-style screen-dimmer slider.
    let brightness_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(4)
        .visible(false)
        .build();
    brightness_box.add_css_class("brightness-control");

    // The brightness icon doubles as the automatic-brightness toggle: pressing
    // it hands brightness over to the ambient-light loop (on hosts with a
    // sensor). It's a `ToggleButton` wrapping the icon `Image` — the same
    // shape as the volume mute button — so the scale timer can keep resizing
    // the icon via `set_pixel_size`. Automatic is the expected, default state,
    // so it renders plain; the icon lights up (in the brightness bar's own
    // colour) only when the user has taken *manual* control.
    let brightness_icon = gtk4::Image::from_icon_name("display-brightness-symbolic");
    brightness_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let brightness_button = gtk4::ToggleButton::builder()
        .child(&brightness_icon)
        .has_frame(false)
        .tooltip_text("Automatic brightness")
        .build();
    brightness_button.add_css_class("indicator-button");
    brightness_button.add_css_class("brightness-toggle");

    // Guards against the programmatic `set_active` in the update loop
    // re-triggering `toggled` and echoing a redundant RPC back to the daemon.
    let auto_updating = std::rc::Rc::new(std::cell::Cell::new(false));
    let auto_updating_clone = auto_updating.clone();
    brightness_button.connect_toggled(move |btn| {
        if auto_updating_clone.get() {
            return;
        }
        if let Err(e) = crate::brightness::set_auto_brightness(btn.is_active()) {
            tracing::error!("Failed to set auto brightness: {}", e);
        }
    });
    brightness_box.append(&brightness_button);

    let brightness_slider = gtk4::Scale::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .width_request(BASE_BRIGHTNESS_SLIDER_WIDTH)
        .draw_value(false)
        .build();
    brightness_slider.set_range(0.0, 100.0);
    brightness_slider.set_increments(5.0, 10.0);
    brightness_slider.add_css_class("brightness-slider");

    if let Some(info) = crate::brightness::get_brightness_status() {
        brightness_slider.set_value(info.percent as f64);
    }

    // Debounce brightness changes the same way as volume: the slider can
    // emit dozens of events per second while the user drags it, and the
    // sysfs/`brightnessctl` write is fast but not free.
    let (brightness_tx, brightness_rx_chan) = mpsc::channel::<u8>();
    std::thread::spawn(move || {
        const DEBOUNCE_MS: u64 = 50;

        while let Ok(mut latest_percent) = brightness_rx_chan.recv() {
            loop {
                match brightness_rx_chan.recv_timeout(std::time::Duration::from_millis(DEBOUNCE_MS))
                {
                    Ok(percent) => {
                        latest_percent = percent;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }

            if let Err(e) = crate::brightness::set_brightness(latest_percent) {
                tracing::error!("Failed to set brightness: {}", e);
            }
        }
    });

    let brightness_changing = std::rc::Rc::new(std::cell::Cell::new(false));
    let brightness_changing_clone = brightness_changing.clone();

    brightness_slider.connect_change_value(move |slider, _, value| {
        brightness_changing_clone.set(true);
        let percent = value.clamp(0.0, 100.0) as u8;

        let _ = brightness_tx.send(percent);
        slider.set_value(value);
        glib::Propagation::Stop
    });

    brightness_box.append(&brightness_slider);

    let brightness_label = gtk4::Label::new(Some("--%"));
    brightness_label.add_css_class("brightness-label");
    brightness_label.set_width_chars(4);
    brightness_box.append(&brightness_label);

    right_box.append(&brightness_box);

    // Display mode toggle (issue #87): mirror ⇄ external-only. Hidden unless an
    // external display is connected. Uses an explicit child Image so its pixel
    // size follows the HUD scale factor, like the other indicator buttons.
    let display_icon = gtk4::Image::from_icon_name("preferences-desktop-display-symbolic");
    display_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let display_button = gtk4::Button::builder()
        .child(&display_icon)
        .has_frame(false)
        .tooltip_text("Toggle external display mode")
        .visible(false)
        .build();
    display_button.add_css_class("indicator-button");
    let state_for_display = state.clone();
    display_button.connect_clicked(move |_| {
        if let Some(ds) = state_for_display.display_state() {
            let target = ds.mode.toggled();
            spawn_action(
                default_socket_path(),
                "set_display_mode",
                move |mut client| async move { client.set_display_mode(target).await.map(|_| ()) },
            );
        }
    });
    right_box.append(&display_button);

    // Network connectivity indicator. Shown only when at least one
    // connectivity check is configured. Icon reflects the worst status across
    // all configured checks; the tooltip lists every check and its result so
    // operators can see which target failed.
    let network_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(4)
        .visible(false)
        .build();
    network_box.add_css_class("network-indicator");

    let network_icon = gtk4::Image::from_icon_name("network-offline-symbolic");
    network_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    network_box.append(&network_icon);

    right_box.append(&network_box);

    // Battery indicator
    let battery_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(4)
        .build();

    let battery_icon = gtk4::Image::from_icon_name("battery-good-symbolic");
    battery_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    battery_box.append(&battery_icon);

    let battery_label = gtk4::Label::new(Some("--%"));
    battery_label.add_css_class("battery-label");
    battery_box.append(&battery_label);

    right_box.append(&battery_box);

    // Action button: shows as "End session" when a session is active, "Log out" otherwise.
    // Uses an explicit child Image for the same reason as `volume_button`.
    let action_icon = gtk4::Image::from_icon_name("system-log-out-symbolic");
    action_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let action_button = gtk4::Button::builder()
        .child(&action_icon)
        .has_frame(false)
        .tooltip_text("Log out")
        .build();
    action_button.add_css_class("close-button");

    // Confirmation popover for the "X" button. Ending an activity forcibly
    // loses its unsaved state, and the button is easy to hit by accident, so
    // activities that opt in (the default) get a "really end?" prompt before
    // the session is stopped (issue #78). The popover is parented to the
    // button, so on the layer-shell overlay it renders as a child popup above
    // the running activity. It is built once and re-shown on demand; its
    // message label is refreshed with the current activity name each time.
    let confirm_popover = gtk4::Popover::new();
    confirm_popover.set_parent(&action_button);
    confirm_popover.add_css_class("confirm-close-popover");
    // Drop the prompt straight down from the "X" button. The button sits at the
    // extreme right of the bar, so the default (horizontally centered) placement
    // would put half the popover past the right screen edge — and neither GTK
    // nor the compositor slides an oversized layer-shell popup back on-screen,
    // so it gets clipped (issue #97). `popup()` below additionally offsets it
    // left to keep it fully visible; Bottom gives it unlimited vertical room.
    confirm_popover.set_position(gtk4::PositionType::Bottom);
    // Autohide so the prompt dismisses itself when it loses focus (the user
    // taps the activity, presses Escape, etc.). Autohide relies on an input
    // grab that needs the layer surface to accept keyboard focus, so we switch
    // the HUD to on-demand keyboard interactivity only while the prompt is up
    // (see the popup/`closed` handlers below) and back to none otherwise, so
    // the always-present bar never steals keyboard focus from the activity.
    confirm_popover.set_autohide(true);
    let confirm_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .build();
    let confirm_label = gtk4::Label::new(Some("End this activity?"));
    confirm_label.add_css_class("confirm-close-message");
    confirm_label.set_wrap(true);
    confirm_label.set_max_width_chars(28);
    confirm_box.append(&confirm_label);
    let confirm_button_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .homogeneous(true)
        .build();
    let cancel_button = gtk4::Button::with_label("Cancel");
    let end_button = gtk4::Button::with_label("End activity");
    end_button.add_css_class("destructive-action");
    confirm_button_row.append(&cancel_button);
    confirm_button_row.append(&end_button);
    confirm_box.append(&confirm_button_row);
    confirm_popover.set_child(Some(&confirm_box));

    let popover_for_cancel = confirm_popover.clone();
    cancel_button.connect_clicked(move |_| {
        popover_for_cancel.popdown();
    });

    let popover_for_end = confirm_popover.clone();
    end_button.connect_clicked(move |_| {
        popover_for_end.popdown();
        request_stop_current(default_socket_path());
    });

    // Release the on-demand keyboard grab whenever the prompt goes away, no
    // matter how it was dismissed (Cancel, End, Escape, focus loss, or a
    // programmatic popdown when the activity ends by other means), so the HUD
    // returns to not competing for keyboard focus.
    let window_for_closed = window.clone();
    confirm_popover.connect_closed(move |_| {
        window_for_closed.set_keyboard_mode(KeyboardMode::None);
    });

    let state_for_action = state.clone();
    let popover_for_action = confirm_popover.clone();
    let confirm_box_for_offset = confirm_box.clone();
    let window_for_action = window.clone();
    action_button.connect_clicked(move |btn| {
        let session_state = state_for_action.session_state();
        let socket_path = default_socket_path();
        if session_state.session_id().is_some() {
            if session_state.confirm_on_close() {
                // Refresh the prompt with the activity's name, then ask. Take
                // keyboard focus so the autohide grab can dismiss on focus loss.
                if let Some(name) = session_state.entry_name() {
                    confirm_label.set_text(&format!("End {name}? Unsaved progress may be lost."));
                } else {
                    confirm_label.set_text("End this activity? Unsaved progress may be lost.");
                }
                window_for_action.set_keyboard_mode(KeyboardMode::OnDemand);
                // Right-align the popover to the button instead of letting it
                // center and spill off the right screen edge (issue #97). A
                // Bottom popover is centered on the button, so shifting its
                // center left by (popover_width - button_width)/2 lands its
                // right edge on the button's right edge — fully on-screen, and
                // independent of the compositor doing any slide-to-fit.
                //
                // Measure the content box (a plain widget) rather than the
                // popover itself: a GtkPopover is a native surface and reports a
                // near-zero preferred size before it is mapped. Add the popover
                // chrome (the `> contents` padding, which scales with the HUD
                // factor) so the whole surface, not just the content, clears the
                // edge.
                let (_, content_w, _, _) =
                    confirm_box_for_offset.measure(gtk4::Orientation::Horizontal, -1);
                let chrome = (2.0 * 14.0 * state_for_action.scale_factor()).round() as i32;
                let popover_w = content_w + chrome;
                let (_, button_w, _, _) = btn.measure(gtk4::Orientation::Horizontal, -1);
                popover_for_action.set_offset((button_w - popover_w) / 2, 0);
                popover_for_action.popup();
            } else {
                request_stop_current(socket_path);
            }
        } else {
            tracing::info!("Requesting logout");
            spawn_action(socket_path, "logout", |mut client| async move {
                client.logout().await
            });
        }
    });
    right_box.append(&action_button);

    container.append(&right_box);

    // Set up state updates
    let app_label_clone = app_label.clone();
    let time_display_clone = time_display.clone();
    let warning_box_clone = warning_box.clone();
    let warning_label_clone = warning_label.clone();
    let battery_box_clone = battery_box.clone();
    let battery_icon_clone = battery_icon.clone();
    let battery_label_clone = battery_label.clone();
    let volume_button_clone = volume_button.clone();
    let volume_icon_clone = volume_icon.clone();
    let volume_slider_clone = volume_slider.clone();
    let volume_label_clone = volume_label.clone();
    let slider_changing_for_update = slider_changing.clone();
    let brightness_box_clone = brightness_box.clone();
    let brightness_icon_clone = brightness_icon.clone();
    let brightness_slider_clone = brightness_slider.clone();
    let brightness_label_clone = brightness_label.clone();
    let brightness_changing_for_update = brightness_changing.clone();
    let brightness_button_clone = brightness_button.clone();
    let auto_updating_for_update = auto_updating.clone();
    let clock_label_clone = clock_label.clone();
    let action_button_clone = action_button.clone();
    let action_icon_clone = action_icon.clone();
    let confirm_popover_for_timer = confirm_popover.clone();
    let network_box_clone = network_box.clone();
    let network_icon_clone = network_icon.clone();
    let display_button_clone = display_button.clone();
    // Tracks the connector the HUD is currently anchored to, so we only
    // re-anchor the layer-shell surface when the active output actually changes.
    let anchored_connector = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let window_for_monitor = window.clone();
    // All icons we resize when the HUD scale factor changes.
    let scaled_icons: [gtk4::Image; 7] = [
        warning_icon.clone(),
        battery_icon.clone(),
        volume_icon.clone(),
        brightness_icon.clone(),
        action_icon.clone(),
        network_icon.clone(),
        display_icon.clone(),
    ];
    // Track the most-recently-applied scale factor so we only rebuild the
    // stylesheet when shepherdd sends a new HudScaleChanged value.
    let applied_scale = std::rc::Rc::new(std::cell::Cell::new(1.0_f64));
    let applied_scale_for_timer = applied_scale.clone();
    let css_provider_for_timer = css_provider.clone();
    let window_for_timer = window.clone();

    glib::timeout_add_local(Duration::from_millis(500), move || {
        // Re-apply scaling if shepherdd has changed it since the last tick.
        // The HUD bar height, exclusive zone, and stylesheet all derive from
        // this factor.
        let desired_scale = state.scale_factor();
        if (desired_scale - applied_scale_for_timer.get()).abs() > f64::EPSILON {
            apply_scale(
                &css_provider_for_timer,
                &window_for_timer,
                base_height,
                desired_scale,
            );
            let icon_size = (f64::from(BASE_ICON_PIXEL_SIZE) * desired_scale).round() as i32;
            for icon in &scaled_icons {
                icon.set_pixel_size(icon_size);
            }
            let slider_width = (f64::from(BASE_VOLUME_SLIDER_WIDTH) * desired_scale).round() as i32;
            volume_slider_clone.set_width_request(slider_width);
            let brightness_slider_width =
                (f64::from(BASE_BRIGHTNESS_SLIDER_WIDTH) * desired_scale).round() as i32;
            brightness_slider_clone.set_width_request(brightness_slider_width);
            applied_scale_for_timer.set(desired_scale);
        }

        // While suspending (or awaiting fresh state on resume) the time,
        // battery, and network indicators show placeholders instead of live
        // values, so the frame frozen across the suspend/resume gap is never a
        // stale status (issue #73).
        let suspended = state.is_suspended();

        // Update wall clock display
        if suspended {
            clock_label_clone.set_text("--:--");
        } else {
            let current_time = shepherd_util::now();
            if clock_format_full {
                clock_label_clone.set_text(&shepherd_util::format_datetime_full(&current_time));
            } else {
                clock_label_clone.set_text(&shepherd_util::format_clock_time(&current_time));
            }
        }

        // Update session state
        let session_state = state.session_state();
        let has_session = session_state.session_id().is_some();
        if has_session {
            action_icon_clone.set_icon_name(Some("window-close-symbolic"));
            action_button_clone.set_tooltip_text(Some("End session"));
        } else {
            action_icon_clone.set_icon_name(Some("system-log-out-symbolic"));
            action_button_clone.set_tooltip_text(Some("Log out"));
        }
        // If the activity has ended or started ending by any means other than
        // the prompt itself (time expiry, API stop, process exit), dismiss a
        // lingering close-confirmation popover — there is nothing left to
        // confirm (issue #78). Harmless no-op when it isn't showing.
        if !matches!(
            session_state,
            SessionState::Active { .. } | SessionState::Warning { .. }
        ) {
            confirm_popover_for_timer.popdown();
        }
        match &session_state {
            SessionState::NoSession => {
                app_label_clone.set_text("No session");
                time_display_clone.set_remaining(None);
                warning_box_clone.set_visible(false);
            }
            SessionState::Active {
                entry_name,
                started_at,
                time_limit_secs,
                ..
            } => {
                app_label_clone.set_text(entry_name);
                // Calculate remaining time based on elapsed time since session start
                let remaining = time_limit_secs.map(|limit| {
                    let elapsed = started_at.elapsed().as_secs();
                    limit.saturating_sub(elapsed)
                });
                time_display_clone.set_remaining(remaining);
                warning_box_clone.set_visible(false);
            }
            SessionState::Warning {
                entry_name,
                warning_issued_at,
                time_remaining_at_warning,
                message,
                severity,
                ..
            } => {
                app_label_clone.set_text(entry_name);
                // Calculate remaining time based on elapsed time since warning was issued
                let elapsed = warning_issued_at.elapsed().as_secs();
                let remaining = time_remaining_at_warning.saturating_sub(elapsed);
                time_display_clone.set_remaining(Some(remaining));
                // Use configuration-defined message if present, otherwise show time-based message
                let warning_text = message
                    .clone()
                    .unwrap_or_else(|| format!("Only {} seconds remaining!", remaining));
                warning_label_clone.set_text(&warning_text);

                // Apply severity-based CSS classes
                warning_box_clone.remove_css_class("warning-info");
                warning_box_clone.remove_css_class("warning-warn");
                warning_box_clone.remove_css_class("warning-critical");
                match severity {
                    shepherd_api::WarningSeverity::Info => {
                        warning_box_clone.add_css_class("warning-info");
                    }
                    shepherd_api::WarningSeverity::Warn => {
                        warning_box_clone.add_css_class("warning-warn");
                    }
                    shepherd_api::WarningSeverity::Critical => {
                        warning_box_clone.add_css_class("warning-critical");
                    }
                }

                warning_box_clone.set_visible(true);
            }
            SessionState::Ending { reason, .. } => {
                app_label_clone.set_text("Session ending...");
                warning_label_clone.set_text(reason);
                warning_box_clone.set_visible(true);
            }
        }

        // Update network connectivity indicator. The HUD aggregates every
        // configured check: if any one is offline we surface the offline
        // icon so the bar matches the user-visible behavior (entries that
        // require internet are hidden as soon as any check fails).
        let internet = state.internet_status();
        if suspended {
            // The cached connectivity can't be trusted across a suspend; show
            // a neutral "checking" placeholder until the post-resume re-check
            // delivers a fresh StateChanged.
            network_box_clone.set_visible(true);
            network_box_clone.remove_css_class("network-online");
            network_box_clone.remove_css_class("network-offline");
            network_icon_clone.set_icon_name(Some("content-loading-symbolic"));
            network_box_clone.set_tooltip_text(Some("Checking connectivity…"));
        } else if internet.is_empty() {
            network_box_clone.set_visible(false);
        } else {
            network_box_clone.set_visible(true);
            let any_offline = internet.iter().any(|s| !s.available);
            network_box_clone.remove_css_class("network-online");
            network_box_clone.remove_css_class("network-offline");
            if any_offline {
                network_icon_clone.set_icon_name(Some("network-offline-symbolic"));
                network_box_clone.add_css_class("network-offline");
            } else {
                network_icon_clone.set_icon_name(Some("network-transmit-receive-symbolic"));
                network_box_clone.add_css_class("network-online");
            }
            let mut tooltip = String::from("Internet connectivity checks:");
            for status in &internet {
                tooltip.push('\n');
                tooltip.push_str(if status.available { "✓ " } else { "✗ " });
                tooltip.push_str(&status.target);
            }
            network_box_clone.set_tooltip_text(Some(&tooltip));
        }

        // Update the display-mode toggle and follow the active output (#87).
        // The button only appears while an external display is connected; its
        // tooltip names the action the toggle performs from the current mode.
        if let Some(ds) = state.display_state() {
            use shepherd_api::DisplayMode;
            match (ds.has_secondary(), ds.mode) {
                (true, DisplayMode::Mirror) => {
                    display_button_clone.set_visible(true);
                    display_button_clone.set_tooltip_text(Some("Use external display only"));
                }
                (true, DisplayMode::ExternalOnly) => {
                    display_button_clone.set_visible(true);
                    display_button_clone.set_tooltip_text(Some("Mirror to external display"));
                }
                _ => display_button_clone.set_visible(false),
            }
            // The active output is the external in external-only mode, else the
            // primary. Re-anchor the layer-shell surface there so the HUD is
            // always visible on the screen the user is looking at.
            let active = match ds.mode {
                DisplayMode::ExternalOnly => ds.secondary.clone(),
                _ => ds.primary.clone(),
            };
            if let Some(name) = active
                && anchored_connector.borrow().as_deref() != Some(name.as_str())
                && let Some(monitor) = monitor_by_connector(&name)
            {
                // Force a clean unmap → remap onto the new output. When the
                // previously-anchored output is disabled (switching to
                // external-only), the compositor destroys the HUD's layer
                // surface but GTK still believes the window is visible — so
                // set_monitor/present alone won't recreate it and the HUD
                // vanishes. Hiding first resyncs GTK's mapped state, then
                // set_monitor + show builds a fresh surface on the live output.
                window_for_monitor.set_visible(false);
                window_for_monitor.set_monitor(&monitor);
                window_for_monitor.set_visible(true);
                *anchored_connector.borrow_mut() = Some(name);
            }
        }

        // Update battery
        if suspended {
            // Placeholder so a stale charge level isn't frozen on screen.
            battery_box_clone.set_visible(true);
            battery_icon_clone.set_icon_name(Some("battery-missing-symbolic"));
            battery_label_clone.set_text("--%");
        } else {
            let battery = BatteryStatus::read();
            let has_battery = battery.percent.is_some();
            battery_box_clone.set_visible(has_battery);
            if has_battery {
                battery_icon_clone.set_icon_name(Some(battery.icon_name()));
                if let Some(percent) = battery.percent {
                    battery_label_clone.set_text(&format!("{}%", percent));
                }
            }
        }

        // Update volume from cached state (updated via events, no polling needed)
        if let Some(volume) = state.volume_info() {
            volume_icon_clone.set_icon_name(Some(volume.icon_name()));
            volume_label_clone.set_text(&format!("{}%", volume.percent));

            // Only update slider if user is not actively dragging it
            if !slider_changing_for_update.get() {
                volume_slider_clone.set_value(volume.percent as f64);
            }
            // Reset the changing flag after a short delay
            slider_changing_for_update.set(false);

            // Disable slider when muted or when restrictions don't allow changes
            let slider_enabled = !volume.muted && volume.restrictions.allow_change;
            volume_slider_clone.set_sensitive(slider_enabled);
            volume_button_clone.set_sensitive(volume.restrictions.allow_mute);

            // Update slider range based on restrictions
            let min = volume.restrictions.min_volume.unwrap_or(0) as f64;
            let max = volume.restrictions.max_volume.unwrap_or(100) as f64;
            volume_slider_clone.set_range(min, max);
        } else {
            volume_label_clone.set_text("--%");
            volume_slider_clone.set_sensitive(false);
        }

        // Update brightness slider from cached state. Hidden entirely on
        // hosts that don't expose a backlight (`available=false`).
        if let Some(brightness) = state.brightness_info() {
            if brightness.available {
                brightness_box_clone.set_visible(true);
                brightness_icon_clone.set_icon_name(Some(brightness.icon_name()));
                brightness_label_clone.set_text(&format!("{}%", brightness.percent));

                if !brightness_changing_for_update.get() {
                    brightness_slider_clone.set_value(brightness.percent as f64);
                }
                brightness_changing_for_update.set(false);

                brightness_slider_clone.set_sensitive(brightness.restrictions.allow_change);

                let min = brightness.restrictions.min_brightness.unwrap_or(0) as f64;
                let max = brightness.restrictions.max_brightness.unwrap_or(100) as f64;
                brightness_slider_clone.set_range(min, max);

                // The brightness icon toggles auto brightness, but only when a
                // light sensor exists; otherwise it stays a plain, inert icon.
                brightness_button_clone.set_sensitive(brightness.auto_available);
                if brightness.auto_available
                    && brightness_button_clone.is_active() != brightness.auto_enabled
                {
                    auto_updating_for_update.set(true);
                    brightness_button_clone.set_active(brightness.auto_enabled);
                    auto_updating_for_update.set(false);
                }
            } else {
                brightness_box_clone.set_visible(false);
            }
        } else {
            brightness_box_clone.set_visible(false);
        }

        glib::ControlFlow::Continue
    });

    container
}

/// Find the GDK monitor whose connector name matches `connector` (e.g.
/// "eDP-1", "HDMI-A-1"), so the HUD can anchor its layer-shell surface to a
/// specific output (issue #87). Returns `None` if no monitor reports that
/// connector (e.g. it was just disabled).
fn monitor_by_connector(connector: &str) -> Option<gtk4::gdk::Monitor> {
    use gtk4::gio::prelude::ListModelExt;
    let monitors = gtk4::gdk::Display::default()?.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i))
        .filter_map(|obj| obj.downcast::<gtk4::gdk::Monitor>().ok())
        .find(|m| m.connector().as_deref() == Some(connector))
}

/// Install an empty `CssProvider` at application priority and return it so
/// the caller can refresh its contents on the fly via `apply_scale`.
fn install_css_provider() -> gtk4::CssProvider {
    let provider = gtk4::CssProvider::new();
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("Could not get display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    provider
}

/// Apply the current scale factor to the HUD: regenerate the stylesheet
/// with px values multiplied by `factor`, and resize the window so its
/// physical height stays consistent with the pre-scale value. Called once
/// on construction and again every time shepherdd sends a HudScaleChanged.
fn apply_scale(
    provider: &gtk4::CssProvider,
    window: &gtk4::ApplicationWindow,
    base_height: i32,
    factor: f64,
) {
    let scaled_height = ((base_height as f64) * factor).round() as i32;
    window.set_default_height(scaled_height);
    window.set_exclusive_zone(scaled_height);
    provider.load_from_data(&css_for_scale(factor));
}

/// Build the HUD stylesheet with `factor`-scaled px values. Every `Npx`
/// literal in `CSS_TEMPLATE` is multiplied by `factor` so the layer-shell
/// surface stays a constant physical size when shepherdd drops the
/// compositor scale to 1.0 for an XWayland activity (see the
/// HudScaleChanged event in shepherd-api). Non-px numbers (timings,
/// opacities, rgba components) are passed through unchanged.
fn css_for_scale(factor: f64) -> String {
    scale_px_literals(CSS_TEMPLATE, factor)
}

fn scale_px_literals(template: &str, factor: f64) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len() + 64);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let num_str = &template[start..i];
            if i + 1 < bytes.len() && &bytes[i..i + 2] == b"px" {
                let n: f64 = num_str.parse().unwrap_or(0.0);
                out.push_str(&((n * factor).round() as i32).to_string());
                out.push_str("px");
                i += 2;
            } else {
                out.push_str(num_str);
            }
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

const CSS_TEMPLATE: &str = r#"
        :root {
            --hud-bg: rgba(30, 30, 30, 0.95);
            --text-primary: white;
            --text-secondary: #d8dee9;
            --color-info: #88c0d0;
            --color-warning: #ebcb8b;
            --color-critical: #ff6b6b;
            --color-success: #a3be8c;
            --hover-bg: rgba(255, 255, 255, 0.1);
        }

        .hud-bar {
            background-color: var(--hud-bg);
            border: none;
            margin: 0;
            padding: 6px 12px;
        }

        .app-name {
            font-weight: bold;
            font-size: 14px;
            color: var(--text-primary);
        }

        .time-display {
            font-family: monospace;
            font-size: 14px;
            color: var(--color-info);
        }

        .time-display.time-warning {
            color: var(--color-warning);
        }

        .time-display.time-critical {
            color: var(--color-critical);
            animation: blink 1s infinite;
        }

        @keyframes blink {
            50% { opacity: 0.5; }
        }

        .warning-banner {
            background-color: rgba(235, 203, 139, 0.2);
            border-radius: 4px;
            padding: 4px 12px;
        }

        .warning-banner.warning-info {
            background-color: rgba(136, 192, 208, 0.2);
        }

        .warning-banner.warning-info .warning-text {
            color: var(--color-info);
        }

        .warning-banner.warning-warn {
            background-color: rgba(235, 203, 139, 0.2);
        }

        .warning-banner.warning-warn .warning-text {
            color: var(--color-warning);
        }

        .warning-banner.warning-critical {
            background-color: rgba(255, 107, 107, 0.2);
            animation: blink 1s infinite;
        }

        .warning-banner.warning-critical .warning-text {
            color: var(--color-critical);
        }

        .warning-text {
            color: var(--color-warning);
            font-weight: bold;
        }

        image {
            color: var(--text-primary);
        }

        .indicator-button,
        .control-button {
            min-width: 32px;
            min-height: 32px;
            padding: 4px;
            border-radius: 4px;
            color: var(--text-primary);
        }

        .indicator-button:hover,
        .control-button:hover {
            background-color: var(--hover-bg);
        }

        /* The brightness icon is a toggle: automatic is the default, so it
           stays plain when checked (auto on). It lights up only in the
           *manual* state (unchecked, and only when a sensor makes auto an
           option at all), using the brightness bar's own highlight colour so
           the two read as one control. */
        .brightness-toggle:not(:checked):not(:disabled) {
            background-color: var(--color-warning);
        }

        .brightness-toggle:not(:checked):not(:disabled) image {
            color: #2e3440;
        }

        /* The GTK theme shades a *checked* toggle button by default. Automatic
           brightness (checked) must look completely plain, so clear that
           shading — keeping only the normal hover feedback. */
        .brightness-toggle:checked {
            background-color: transparent;
            background-image: none;
            box-shadow: none;
        }

        .brightness-toggle:checked:hover {
            background-color: var(--hover-bg);
        }

        .close-button {
            min-width: 32px;
            min-height: 32px;
            padding: 4px;
            border-radius: 4px;
            color: var(--color-critical);
        }

        .close-button:hover {
            background-color: rgba(191, 97, 106, 0.3);
        }

        .battery-label {
            font-size: 12px;
            color: var(--text-primary);
        }

        .network-indicator {
            padding: 0 2px;
        }

        .network-indicator.network-online image {
            color: var(--color-success);
        }

        .network-indicator.network-offline image {
            color: var(--color-critical);
        }

        .volume-control {
            padding: 0 4px;
        }

        .volume-slider {
            min-width: 80px;
        }

        .volume-slider trough {
            min-height: 4px;
            border-radius: 2px;
            background-color: rgba(255, 255, 255, 0.2);
        }

        .volume-slider highlight {
            min-height: 4px;
            border-radius: 2px;
            background-color: var(--color-info);
        }

        .volume-slider slider {
            min-width: 12px;
            min-height: 12px;
            border-radius: 50%;
            background-color: var(--text-primary);
        }

        .volume-slider:disabled trough {
            background-color: rgba(255, 255, 255, 0.1);
        }

        .volume-slider:disabled highlight {
            background-color: rgba(136, 192, 208, 0.5);
        }

        .volume-label {
            font-size: 12px;
            color: var(--text-secondary);
            min-width: 3em;
            text-align: right;
        }

        .brightness-control {
            padding: 0 4px;
        }

        .brightness-slider {
            min-width: 80px;
        }

        .brightness-slider trough {
            min-height: 4px;
            border-radius: 2px;
            background-color: rgba(255, 255, 255, 0.2);
        }

        .brightness-slider highlight {
            min-height: 4px;
            border-radius: 2px;
            background-color: var(--color-warning);
        }

        .brightness-slider slider {
            min-width: 12px;
            min-height: 12px;
            border-radius: 50%;
            background-color: var(--text-primary);
        }

        .brightness-slider:disabled trough {
            background-color: rgba(255, 255, 255, 0.1);
        }

        .brightness-slider:disabled highlight {
            background-color: rgba(235, 203, 139, 0.5);
        }

        .brightness-label {
            font-size: 12px;
            color: var(--text-secondary);
            min-width: 3em;
            text-align: right;
        }

        .clock-label {
            font-family: monospace;
            font-size: 14px;
            color: var(--text-primary);
        }

        .mock-time-indicator {
            font-size: 10px;
            font-weight: bold;
            color: var(--color-warning);
            margin-left: 4px;
        }

        /* Opaque dark surface with explicit colors (not theme variables) so
           the prompt keeps strong text contrast regardless of the system GTK
           theme and never lets the bright activity behind it bleed through.
           The arrow (the triangle pointing at the "X") is a separate CSS node
           and must be recolored to match the box. */
        .confirm-close-popover > contents {
            background-color: #1e1e1e;
            border-radius: 8px;
            padding: 14px;
        }

        .confirm-close-popover > arrow {
            background-color: #1e1e1e;
            border: none;
        }

        .confirm-close-message {
            color: #ffffff;
            font-size: 15px;
            font-weight: bold;
        }

        /* Theme buttons paint a gradient via background-image, which a bare
           background-color won't override, so clear it and set explicit
           high-contrast fills: a light Cancel with dark text, a red End with
           white text. */
        .confirm-close-popover button {
            min-height: 32px;
            padding: 6px 14px;
            border-radius: 4px;
            border: none;
            background-image: none;
            color: #2e3440;
            background-color: #d8dee9;
        }

        .confirm-close-popover button:hover {
            background-color: #e5e9f0;
        }

        .confirm-close-popover button.destructive-action {
            color: #ffffff;
            background-color: #bf616a;
        }

        .confirm-close-popover button.destructive-action:hover {
            background-color: #d08770;
        }
    "#;

fn run_event_loop(socket_path: PathBuf, state: SharedState) -> anyhow::Result<()> {
    let rt = Runtime::new()?;

    rt.block_on(async {
        loop {
            tracing::info!("Connecting to shepherdd at {:?}", socket_path);

            match IpcClient::connect(&socket_path).await {
                Ok(mut client) => {
                    tracing::info!("Connected to shepherdd");

                    // Get initial volume before subscribing (can't send RPCs after subscribe)
                    match client.get_volume().await {
                        Ok(info) => {
                            tracing::debug!("Got initial volume: {}%", info.percent);
                            state.set_initial_volume(info);
                        }
                        Err(e) => tracing::warn!("Failed to get initial volume: {}", e),
                    }

                    // Same for brightness. Returns available=false when the
                    // host has no backlight, which is the signal to the UI
                    // that it should hide the slider entirely.
                    match client.get_brightness().await {
                        Ok(info) => {
                            tracing::debug!(
                                "Got initial brightness: {}% (available={})",
                                info.percent,
                                info.available,
                            );
                            state.set_initial_brightness(info);
                        }
                        Err(e) => tracing::warn!("Failed to get initial brightness: {}", e),
                    }

                    // Seed the display arrangement so the mirror/external toggle
                    // and active-output anchor are correct before any hotplug
                    // event fires (issue #87).
                    match client.get_display_state().await {
                        Ok(ds) => state.set_display_state(ds),
                        Err(e) => tracing::warn!("Failed to get initial display state: {}", e),
                    }

                    // Pull a fresh service snapshot so the network indicator
                    // (and any other state-derived UI) is populated even when
                    // no event has fired since the HUD connected.
                    match client.service_state().await {
                        Ok(snapshot) => {
                            state.handle_event(&shepherd_api::Event::new(
                                shepherd_api::EventPayload::StateChanged(snapshot),
                            ));
                        }
                        Err(e) => tracing::warn!("Failed to get initial state: {}", e),
                    }

                    let mut stream = match client.subscribe().await {
                        Ok(stream) => stream,
                        Err(e) => {
                            tracing::error!("Failed to subscribe: {}", e);
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    };

                    loop {
                        match stream.next().await {
                            Ok(event) => {
                                tracing::debug!("Received event: {:?}", event);
                                state.handle_event(&event);
                            }
                            Err(e) => {
                                tracing::error!("Event stream error: {}", e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to connect to shepherdd: {}", e);
                }
            }

            // Wait before reconnecting
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}
