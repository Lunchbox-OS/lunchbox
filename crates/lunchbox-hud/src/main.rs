//! Lunchbox HUD - Always-visible overlay
//!
//! This is the heads-up display that remains visible during active sessions.
//! It shows time remaining, battery, volume, and provides session controls.

mod app;
mod battery;
mod brightness;
mod orientation;
mod page_turn;
mod rotated_label;
mod state;
mod theme;
mod time_display;
mod volume;

use anyhow::Result;
use clap::Parser;
use lunchbox_util::default_socket_path;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Lunchbox HUD - Always-visible overlay for lunchboxd sessions
#[derive(Parser, Debug)]
#[command(name = "lunchbox-hud")]
#[command(about = "GTK4 layer-shell HUD for lunchboxd", long_about = None)]
struct Args {
    /// Socket path for lunchboxd connection (or set LUNCHBOX_SOCKET env var)
    #[arg(short, long, env = "LUNCHBOX_SOCKET")]
    socket: Option<PathBuf>,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Pin the HUD to a screen edge (top, bottom, left), ignoring config
    ///
    /// `left` gives the vertical HUD (issue #171): the same bar rotated a
    /// quarter turn, down the left edge of the screen.
    ///
    /// **Absent — which is how `sway.conf` starts the HUD — the edge comes
    /// from lunchboxd**, which resolves `[service.hud]` against the running
    /// activity's own `hud_orientation` and pushes changes as they happen.
    /// Passing this pins the bar and makes the HUD ignore those, which is what
    /// makes it useful for development (`LUNCHBOX_HUD_ANCHOR=left`) and a
    /// footgun on a device.
    #[arg(short, long, env = "LUNCHBOX_HUD_ANCHOR")]
    anchor: Option<String>,

    /// Thickness of the HUD bar in pixels — its height when the bar is
    /// horizontal, its width when it runs down the side.
    ///
    /// Defaults to the branding's own `space.hud-h`, because the thickness is a
    /// design decision rather than a taste. It is 48px: the branding hand-off
    /// drew the bar at 56 and it was built that way, then taken back down,
    /// because eight pixels of every activity is a lot to pay for a bar that
    /// reads no better at 56 (issue #209).
    #[arg(long, default_value_t = lunchbox_branding::tokens::SPACE_HUD_H)]
    height: i32,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level)),
        )
        .init();

    tracing::info!("Starting Lunchbox HUD");

    // Determine socket path with fallback to default
    let socket_path = args.socket.unwrap_or_else(default_socket_path);

    // Run GTK application
    let pinned = args.anchor.as_deref().map(orientation::parse_anchor);
    let application = app::HudApp::new(socket_path, pinned, args.height);
    let exit_code = application.run();

    std::process::exit(exit_code);
}
