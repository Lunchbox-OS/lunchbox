//! lunchbox-pairing-display
//!
//! Sway / `wlr-layer-shell` overlay for the two numbers a parent has to read
//! off the television. Launched as a short-lived subprocess by `lunchboxd` and
//! killed when whatever it is announcing is over. See the crate README,
//! `docs/ai/history/2026-06-20 002 ble-management.md` for pairing and
//! `docs/ai/history/2026-09-07 003 web-management-authentication-scope.md`
//! for setup.
//!
//! Two modes, and the difference in how much screen they take is deliberate:
//!
//! - `--passkey` — BLE pairing. Full-screen, because pairing is a thing
//!   happening *now* that the person at the TV must not miss, and it lasts
//!   seconds.
//! - `--setup-code` — the web management setup code (issue #156). A card in
//!   the corner, because this one is up for minutes while a parent walks to
//!   another room and finds a browser, and blacking out the television for
//!   that long would be its own bug report.

use anyhow::Result;
use clap::{Parser, ValueEnum};
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "lunchbox-pairing-display")]
#[command(about = "Fullscreen passkey overlay for BLE pairing", long_about = None)]
struct Args {
    /// 6-digit passkey BlueZ passed to the pairing agent. Rendered as a
    /// zero-padded string so a low passkey (e.g. 42) still shows as
    /// `000042`.
    #[arg(
        long,
        required_unless_present = "setup_code",
        conflicts_with = "setup_code"
    )]
    passkey: Option<u32>,

    /// Identifier of the peer device — shown verbatim under the
    /// passkey so the user can sanity-check what's trying to pair.
    #[arg(long, required_unless_present = "setup_code")]
    device: Option<String>,

    /// Pairing method the agent selected, so the instruction copy
    /// matches what the phone is actually asking the user to do.
    /// Numeric Comparison (LESC) → `compare`; Passkey Entry
    /// (LE Legacy or some LESC IO-cap combinations) → `enter`.
    #[arg(long, value_enum, required_unless_present = "setup_code")]
    method: Option<PairingMethodArg>,

    /// The web management setup code (issue #156), shown as a corner card
    /// rather than a full-screen overlay.
    #[arg(long)]
    setup_code: Option<String>,

    /// Where to type it — a management URL, shown under the code so the
    /// parent does not have to be told the device's address separately.
    ///
    /// Repeatable, because a device bound to `0.0.0.0` is reachable at one
    /// address per network it is on (issue #182): wifi and a VPN are two
    /// different answers, and which one works depends on where the parent's
    /// laptop is. The daemon passes none while its listener is still coming
    /// up, and `--port` covers that.
    #[arg(long = "url")]
    urls: Vec<String>,

    /// The management port, for the wildcard-bind case where there is no one
    /// URL to name. "Port 8080 on this device" is still most of the answer.
    #[arg(long)]
    port: Option<u16>,

    /// Log level for the overlay's own logs (separate from the GTK
    /// stderr noise).
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PairingMethodArg {
    /// LESC Numeric Comparison — user confirms the number matches.
    Compare,
    /// Passkey Entry — user types the number into the phone.
    Enter,
}

fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level)),
        )
        .init();

    let app = gtk4::Application::builder()
        .application_id("com.lunchbox-os.pairing-display")
        .build();

    let mode = match args.setup_code {
        Some(code) => Mode::Setup {
            code,
            urls: args.urls.clone(),
            port: args.port,
        },
        None => Mode::Pairing {
            // clap's `required_unless_present` has already established these.
            passkey: args
                .passkey
                .expect("clap requires --passkey without --setup-code"),
            device: args.device.clone().expect("clap requires --device"),
            method: args.method.expect("clap requires --method"),
        },
    };
    app.connect_activate(move |app| {
        build_overlay_window(app, &mode);
    });

    // Pass an empty argv to GTK so its GLib option parser doesn't see
    // the clap flags we already consumed above and reject them with
    // "Unknown option --passkey" — which would silently exit 0 before
    // the activate callback ever fires. Anything we want GTK itself to
    // see (e.g. GTK debug flags) would need to be split off from argv
    // before clap; we don't have any today, so empty is fine.
    let empty: [&str; 0] = [];
    let exit_code = app.run_with_args(&empty);
    std::process::exit(exit_code.into());
}

/// What this invocation is showing.
enum Mode {
    Pairing {
        passkey: u32,
        device: String,
        method: PairingMethodArg,
    },
    Setup {
        code: String,
        urls: Vec<String>,
        port: Option<u16>,
    },
}

fn build_overlay_window(app: &gtk4::Application, mode: &Mode) {
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .build();

    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace("lunchbox-pairing-display");
    // Neither mode grabs the keyboard: the person is acting on a phone or a
    // laptop, not on this screen, and stealing focus from a running activity
    // would be worse than either message is urgent.
    window.set_keyboard_mode(KeyboardMode::None);

    match mode {
        Mode::Pairing { .. } => {
            // Full-screen: pairing is happening now and lasts seconds.
            window.add_css_class("pairing-window");
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
                window.set_margin(edge, 0);
            }
        }
        Mode::Setup { .. } => {
            window.add_css_class("setup-window");
            // A corner card: this one is up for minutes while a parent finds a
            // browser, and the child may be mid-activity behind it.
            for edge in [Edge::Bottom, Edge::Right] {
                window.set_anchor(edge, true);
                window.set_margin(edge, 32);
            }
        }
    }
    window.set_exclusive_zone(-1);

    install_css();

    let content = match mode {
        Mode::Pairing {
            passkey,
            device,
            method,
        } => build_content(*passkey, device, *method),
        Mode::Setup { code, urls, port } => build_setup_content(code, urls, *port),
    };
    window.set_child(Some(&content));
    window.present();
}

/// How many addresses the card offers.
///
/// A device on wifi and a VPN has two ways in and both are worth naming; a box
/// running containers has more, and the ones past this are not addresses
/// anybody at the television is going to retype. The count of what is left out
/// is shown rather than silently dropped.
const MAX_SETUP_URLS: usize = 3;

/// The setup card: what the code is for, the code, and where to type it.
fn build_setup_content(code: &str, urls: &[String], port: Option<u16>) -> gtk4::Box {
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    outer.set_halign(gtk4::Align::Center);
    outer.set_valign(gtk4::Align::Center);
    outer.add_css_class("setup-root");

    let header = gtk4::Label::new(Some("Set up management access"));
    header.add_css_class("setup-header");
    outer.append(&header);

    let code_label = gtk4::Label::new(Some(code));
    code_label.add_css_class("setup-code");
    // Not selectable, unlike the pairing passkey: GTK renders a selectable
    // label's contents pre-selected, and on a card this size a fully
    // highlighted number reads as an error state rather than as text you could
    // copy — with nothing on this device to paste it into anyway.
    outer.append(&code_label);

    let instruction_label = gtk4::Label::new(Some(&setup_instruction(urls, port)));
    instruction_label.add_css_class("setup-instruction");
    instruction_label.set_justify(gtk4::Justification::Center);
    instruction_label.set_wrap(true);
    outer.append(&instruction_label);

    // The addresses get their own label rather than joining the sentence: they
    // are the part somebody is copying by eye onto another device, so they are
    // monospaced and left alone by the wrapping that the prose above wants.
    if let Some(addresses) = setup_addresses(urls) {
        let address_label = gtk4::Label::new(Some(&addresses));
        address_label.add_css_class("setup-addresses");
        // Left-justified inside a centred block: a column of addresses that is
        // ragged on both edges is harder to read off a television than one
        // that lines up, and the trailing "and N more" is not an address.
        address_label.set_justify(gtk4::Justification::Left);
        address_label.set_halign(gtk4::Align::Center);
        outer.append(&address_label);
    }

    outer
}

/// The sentence above the addresses.
///
/// Three shapes, because the daemon knows three different amounts: every
/// address it is reachable at, only the port (a listener that has not come up
/// yet, or a device with nothing routable), or nothing at all.
fn setup_instruction(urls: &[String], port: Option<u16>) -> String {
    match (urls.is_empty(), port) {
        (false, _) => {
            let plural = if urls.len() == 1 {
                "this address"
            } else {
                "one of these"
            };
            format!("Enter this code at {plural} on your phone or laptop\nto choose a password:")
        }
        // A listener still binding, or a device with no routable address, has
        // no URL to name — so name the port and let the parent supply the
        // address they already reach the device by.
        (true, Some(port)) => format!(
            "Open this device's address in a browser on port {port},\nand enter this code to \
             choose a password."
        ),
        (true, None) => {
            "Open this device's management page and enter this code to choose a password."
                .to_string()
        }
    }
}

/// The addresses block, or `None` when there are no addresses to show.
fn setup_addresses(urls: &[String]) -> Option<String> {
    if urls.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = urls.iter().take(MAX_SETUP_URLS).cloned().collect();
    let hidden = urls.len().saturating_sub(MAX_SETUP_URLS);
    if hidden > 0 {
        lines.push(format!("and {hidden} more"));
    }
    Some(lines.join("\n"))
}

fn build_content(passkey: u32, device: &str, method: PairingMethodArg) -> gtk4::Box {
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

    let instruction = gtk4::Label::new(Some(instruction_for(method)));
    instruction.add_css_class("pairing-instruction");
    instruction.set_justify(gtk4::Justification::Center);
    instruction.set_wrap(true);
    outer.append(&instruction);

    outer
}

fn instruction_for(method: PairingMethodArg) -> &'static str {
    match method {
        PairingMethodArg::Compare => {
            "If this number matches the one on your phone, tap PAIR (or MATCH) on the phone.\n\
             If it does not match, tap CANCEL (or DON'T MATCH) and tell whoever is in charge of this device."
        }
        PairingMethodArg::Enter => {
            "Type this number into the prompt on your phone to complete pairing.\n\
             If you did not initiate this pairing, tap CANCEL on the phone and tell whoever is in charge of this device."
        }
    }
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
    color: #ffffff;
}
/* The pairing overlay covers the screen, so its window paints the backdrop.
   The setup card is a corner card: its window must stay transparent or it
   would black out the activity behind it, so the card paints its own. */
window.pairing-window {
    background-color: rgba(0, 0, 0, 0.92);
}
window.setup-window {
    background-color: transparent;
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
.setup-root {
    padding: 28px 40px;
    background-color: rgba(0, 0, 0, 0.92);
    border-radius: 18px;
}
.setup-header {
    font-size: 20px;
    font-weight: 600;
    opacity: 0.8;
}
.setup-code {
    font-size: 72px;
    font-weight: 700;
    font-family: monospace;
    letter-spacing: 8px;
    margin-top: 8px;
    margin-bottom: 8px;
}
.setup-instruction {
    font-size: 18px;
    opacity: 0.85;
    max-width: 460px;
}
/* The addresses are what somebody retypes on another device, so they get
   monospace and full opacity — the prose around them can fade, these cannot. */
.setup-addresses {
    font-size: 22px;
    font-family: monospace;
    font-weight: 600;
    margin-top: 4px;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("https://192.168.0.{i}:8080"))
            .collect()
    }

    #[test]
    fn the_addresses_are_named_when_the_daemon_knows_them() {
        // The point of the change: a parent at the television should not have
        // to be told the device's address by some other means.
        let text = setup_instruction(&urls(1), Some(8080));
        assert!(text.contains("this address"), "{text}");
        assert_eq!(
            setup_addresses(&urls(1)).as_deref(),
            Some("https://192.168.0.0:8080")
        );
    }

    #[test]
    fn several_ways_in_all_get_a_line() {
        // A device on wifi and a VPN: which one works depends on where the
        // laptop is, so naming only the first would be a coin flip.
        assert!(setup_instruction(&urls(2), Some(8080)).contains("one of these"));
        assert_eq!(
            setup_addresses(&urls(2)).as_deref(),
            Some("https://192.168.0.0:8080\nhttps://192.168.0.1:8080")
        );
    }

    #[test]
    fn a_box_with_too_many_addresses_says_how_many_it_hid() {
        let text = setup_addresses(&urls(MAX_SETUP_URLS + 2)).expect("addresses");
        assert_eq!(text.lines().count(), MAX_SETUP_URLS + 1);
        assert!(text.ends_with("and 2 more"), "{text}");
    }

    #[test]
    fn a_listener_that_is_not_up_yet_falls_back_to_the_port() {
        // `management_urls` is empty until the listener is actually serving,
        // which on a cold boot it is not — the card still has to say something
        // a parent can act on.
        let text = setup_instruction(&[], Some(8080));
        assert!(text.contains("port 8080"), "{text}");
        assert_eq!(setup_addresses(&[]), None);
    }

    #[test]
    fn knowing_neither_still_reads_as_a_sentence() {
        let text = setup_instruction(&[], None);
        assert!(text.contains("management page"), "{text}");
    }
}
