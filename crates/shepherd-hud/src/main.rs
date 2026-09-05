//! Shepherd HUD - Always-visible overlay
//!
//! This is the heads-up display that remains visible during active sessions.
//! It shows time remaining, battery, volume, and provides session controls.

mod analog_clock;
mod app;
mod battery;
mod brightness;
mod orientation;
mod page_turn;
mod rotated_label;
mod state;
mod time_display;
mod volume;

use anyhow::Result;
use clap::Parser;
use shepherd_util::default_socket_path;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Shepherd HUD - Always-visible overlay for shepherdd sessions
#[derive(Parser, Debug)]
#[command(name = "shepherd-hud")]
#[command(about = "GTK4 layer-shell HUD for shepherdd", long_about = None)]
struct Args {
    /// Socket path for shepherdd connection (or set SHEPHERD_SOCKET env var)
    #[arg(short, long, env = "SHEPHERD_SOCKET")]
    socket: Option<PathBuf>,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Anchor position (top, bottom, left)
    ///
    /// `left` gives the vertical HUD (issue #171): the same bar rotated a
    /// quarter turn, down the left edge of the screen.
    #[arg(short, long, env = "SHEPHERD_HUD_ANCHOR", default_value = "top")]
    anchor: String,

    /// Thickness of the HUD bar in pixels — its height when the bar is
    /// horizontal, its width when it runs down the side.
    #[arg(long, default_value = "48")]
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

    tracing::info!("Starting Shepherd HUD");

    // Determine socket path with fallback to default
    let socket_path = args.socket.unwrap_or_else(default_socket_path);

    // Run GTK application
    let orientation = orientation::HudOrientation::parse(&args.anchor);
    let application = app::HudApp::new(socket_path, orientation, args.height);
    let exit_code = application.run();

    std::process::exit(exit_code);
}
