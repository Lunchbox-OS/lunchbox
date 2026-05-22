//! HUD Application
//!
//! The main GTK4 application for the HUD overlay.
//! Uses gtk4-layer-shell to create an always-visible overlay.

use crate::battery::BatteryStatus;
use crate::state::{SessionState, SharedState};
use crate::time_display::TimeDisplay;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use shepherd_api::Command;
use shepherd_ipc::IpcClient;
use shepherd_util::default_socket_path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use tokio::runtime::Runtime;

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

    let brightness_icon = gtk4::Image::from_icon_name("display-brightness-medium-symbolic");
    brightness_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    brightness_box.append(&brightness_icon);

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

    let state_for_action = state.clone();
    action_button.connect_clicked(move |_| {
        let session_state = state_for_action.session_state();
        let socket_path = default_socket_path();
        if let Some(session_id) = session_state.session_id() {
            tracing::info!("Requesting end session for {}", session_id);
            std::thread::spawn(move || {
                let rt = Runtime::new().expect("Failed to create runtime");
                rt.block_on(async {
                    match IpcClient::connect(&socket_path).await {
                        Ok(mut client) => {
                            let cmd = Command::StopCurrent {
                                mode: shepherd_api::StopMode::Graceful,
                            };
                            if let Err(e) = client.send(cmd).await {
                                tracing::error!("Failed to send StopCurrent: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to connect to shepherdd: {}", e);
                        }
                    }
                });
            });
        } else {
            tracing::info!("Requesting logout");
            std::thread::spawn(move || {
                let rt = Runtime::new().expect("Failed to create runtime");
                rt.block_on(async {
                    match IpcClient::connect(&socket_path).await {
                        Ok(mut client) => {
                            if let Err(e) = client.send(Command::Logout).await {
                                tracing::error!("Failed to send Logout: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to connect to shepherdd: {}", e);
                        }
                    }
                });
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
    let clock_label_clone = clock_label.clone();
    let action_button_clone = action_button.clone();
    let action_icon_clone = action_icon.clone();
    let network_box_clone = network_box.clone();
    let network_icon_clone = network_icon.clone();
    // All icons we resize when the HUD scale factor changes.
    let scaled_icons: [gtk4::Image; 6] = [
        warning_icon.clone(),
        battery_icon.clone(),
        volume_icon.clone(),
        brightness_icon.clone(),
        action_icon.clone(),
        network_icon.clone(),
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

        // Update wall clock display
        let current_time = shepherd_util::now();
        if clock_format_full {
            clock_label_clone.set_text(&shepherd_util::format_datetime_full(&current_time));
        } else {
            clock_label_clone.set_text(&shepherd_util::format_clock_time(&current_time));
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
        if internet.is_empty() {
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

        // Update battery
        let battery = BatteryStatus::read();
        let has_battery = battery.percent.is_some();
        battery_box_clone.set_visible(has_battery);
        if has_battery {
            battery_icon_clone.set_icon_name(Some(battery.icon_name()));
            if let Some(percent) = battery.percent {
                battery_label_clone.set_text(&format!("{}%", percent));
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
    "#;

fn run_event_loop(socket_path: PathBuf, state: SharedState) -> anyhow::Result<()> {
    let rt = Runtime::new()?;

    rt.block_on(async {
        loop {
            tracing::info!("Connecting to shepherdd at {:?}", socket_path);

            match IpcClient::connect(&socket_path).await {
                Ok(mut client) => {
                    tracing::info!("Connected to shepherdd");

                    // Get initial volume before subscribing (can't send commands after subscribe)
                    match client.send(Command::GetVolume).await {
                        Ok(response) => {
                            if let shepherd_api::ResponseResult::Ok(
                                shepherd_api::ResponsePayload::Volume(info),
                            ) = response.result
                            {
                                tracing::debug!("Got initial volume: {}%", info.percent);
                                state.set_initial_volume(info);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to get initial volume: {}", e);
                        }
                    }

                    // Same for brightness. Returns available=false when the
                    // host has no backlight, which is the signal to the UI
                    // that it should hide the slider entirely.
                    match client.send(Command::GetBrightness).await {
                        Ok(response) => {
                            if let shepherd_api::ResponseResult::Ok(
                                shepherd_api::ResponsePayload::Brightness(info),
                            ) = response.result
                            {
                                tracing::debug!(
                                    "Got initial brightness: {}% (available={})",
                                    info.percent,
                                    info.available,
                                );
                                state.set_initial_brightness(info);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to get initial brightness: {}", e);
                        }
                    }

                    // Pull a fresh service snapshot so the network indicator
                    // (and any other state-derived UI) is populated even when
                    // no event has fired since the HUD connected.
                    match client.send(Command::GetState).await {
                        Ok(response) => {
                            if let shepherd_api::ResponseResult::Ok(
                                shepherd_api::ResponsePayload::State(snapshot),
                            ) = response.result
                            {
                                state.handle_event(&shepherd_api::Event::new(
                                    shepherd_api::EventPayload::StateChanged(snapshot),
                                ));
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to get initial state: {}", e);
                        }
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
