//! shepherd-pairing-display
//!
//! Full-screen Sway / `wlr-layer-shell` overlay that shows the BLE
//! Numeric Comparison passkey during pairing. Launched as a
//! short-lived subprocess by `shepherdd` and killed when the pairing
//! window closes. See the crate README and
//! `docs/ai/history/2026-06-20 002 ble-management.md`.

use anyhow::Result;
use clap::Parser;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "shepherd-pairing-display")]
#[command(about = "Fullscreen passkey overlay for BLE Numeric Comparison pairing", long_about = None)]
struct Args {
    /// 6-digit passkey BlueZ passed to the pairing agent. Rendered as a
    /// zero-padded string so a low passkey (e.g. 42) still shows as
    /// `000042`.
    #[arg(long)]
    passkey: u32,

    /// Identifier of the peer device — shown verbatim under the
    /// passkey so the user can sanity-check what's trying to pair.
    #[arg(long)]
    device: String,

    /// Log level for the overlay's own logs (separate from the GTK
    /// stderr noise).
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level)),
        )
        .init();

    let app = gtk4::Application::builder()
        .application_id("org.shepherd.pairing-display")
        .build();

    let passkey = args.passkey;
    let device = args.device;
    app.connect_activate(move |app| {
        build_overlay_window(app, passkey, &device);
    });

    let exit_code = app.run();
    std::process::exit(exit_code.into());
}

fn build_overlay_window(app: &gtk4::Application, passkey: u32, device: &str) {
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .build();

    // Full-screen overlay on the compositor's top layer so it covers
    // whatever activity is running. We don't grab keyboard input — the
    // user is acting on their phone, not the TV.
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace("shepherd-pairing-display");
    window.set_keyboard_mode(KeyboardMode::None);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
        window.set_margin(edge, 0);
    }
    window.set_exclusive_zone(-1);

    install_css();

    let content = build_content(passkey, device);
    window.set_child(Some(&content));
    window.present();
}

fn build_content(passkey: u32, device: &str) -> gtk4::Box {
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 24);
    outer.set_halign(gtk4::Align::Center);
    outer.set_valign(gtk4::Align::Center);
    outer.add_css_class("pairing-root");

    let header = gtk4::Label::new(Some("Bluetooth pairing request"));
    header.add_css_class("pairing-header");
    outer.append(&header);

    let device_label = gtk4::Label::new(Some(&format!("From device {device}")));
    device_label.add_css_class("pairing-device");
    outer.append(&device_label);

    let passkey_label = gtk4::Label::new(Some(&format_passkey(passkey)));
    passkey_label.add_css_class("pairing-passkey");
    // Selectable so a parent can copy the number off the TV if their
    // phone display is misbehaving.
    passkey_label.set_selectable(true);
    outer.append(&passkey_label);

    let instruction = gtk4::Label::new(Some(
        "If this number matches the one on your phone, tap MATCH on the phone.\n\
         If it does not match, tap DON'T MATCH and tell whoever is in charge of this device.",
    ));
    instruction.add_css_class("pairing-instruction");
    instruction.set_justify(gtk4::Justification::Center);
    instruction.set_wrap(true);
    outer.append(&instruction);

    outer
}

fn install_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn format_passkey(passkey: u32) -> String {
    format!("{passkey:06}")
}

/// Stylesheet for the overlay. Dark background + huge passkey numeral
/// chosen so the number is readable from across the room. Kept inline
/// rather than a `style.css` file so the binary stays self-contained.
const CSS: &str = r#"
window {
    background-color: rgba(0, 0, 0, 0.92);
    color: #ffffff;
}
.pairing-root {
    padding: 64px;
}
.pairing-header {
    font-size: 28px;
    font-weight: 600;
    opacity: 0.85;
}
.pairing-device {
    font-size: 20px;
    opacity: 0.7;
    font-family: monospace;
}
.pairing-passkey {
    font-size: 220px;
    font-weight: 700;
    font-family: monospace;
    letter-spacing: 12px;
    margin-top: 24px;
    margin-bottom: 24px;
}
.pairing-instruction {
    font-size: 22px;
    opacity: 0.85;
    max-width: 720px;
}
"#;
