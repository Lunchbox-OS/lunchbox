//! Lunchbox Launcher UI - Main grid interface
//!
//! This is the primary user-facing shell for the kiosk-style environment.
//! It displays available entries from lunchboxd and allows launching them.

mod app;
mod badge;
mod client;
mod compartment;
mod field;
mod grid;
mod item;
mod state;
mod theme;

use crate::client::CommandClient;
use anyhow::Result;
use clap::Parser;
use lunchbox_ipc::IpcClient;
use lunchbox_util::default_socket_path;
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

/// What a media-key CLI flag routes into on the IPC.
enum MediaCall {
    VolumeUp(u8),
    VolumeDown(u8),
    ToggleMute,
    BrightnessUp(u8),
    BrightnessDown(u8),
}

/// Default step (percent) for the volume up/down CLI flags. Matches the
/// keyboard step most desktops use for XF86Audio* keys.
const DEFAULT_VOLUME_STEP: u8 = 5;

/// Default step (percent) for the brightness up/down CLI flags. 5% per
/// press mirrors what GNOME/KDE use for XF86MonBrightness* keys.
const DEFAULT_BRIGHTNESS_STEP: u8 = 5;

/// Lunchbox Launcher - Child-friendly kiosk launcher
#[derive(Parser, Debug)]
#[command(name = "lunchbox-launcher")]
#[command(about = "GTK4 launcher UI for lunchboxd", long_about = None)]
struct Args {
    /// Socket path for lunchboxd connection (or set LUNCHBOX_SOCKET env var)
    #[arg(short, long, env = "LUNCHBOX_SOCKET")]
    socket: Option<PathBuf>,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Send StopCurrent to lunchboxd and exit (for compositor keybindings)
    #[arg(long)]
    stop_current: bool,

    /// Blank the displays, unless an activity is on screen. Used by swayidle's
    /// idle timeout: on a hardened device `swaymsg "output * dpms off"` cannot
    /// reach the compositor, so the blanking goes through lunchboxd's held
    /// connection instead (issue #144).
    ///
    /// Replaces the old `--is-idle-allowed` gate, which was a separate process
    /// whose answer could be stale by the time the blank ran. lunchboxd now
    /// checks and acts under one lock.
    #[arg(long)]
    screen_off: bool,

    /// Wake the displays (swayidle's `resume` command).
    #[arg(long)]
    screen_on: bool,

    /// Tell lunchboxd the seat has been idle long enough to leave
    /// administrator mode (issue #154), and exit. The daemon decides whether to
    /// act: it leaves the mode only when nothing the caregiver opened is still
    /// on screen. A no-op when the device is not in administrator mode, so
    /// swayidle can call it unconditionally.
    #[arg(long)]
    admin_idle_timeout: bool,

    /// Send VolumeUp to lunchboxd and exit. Intended for compositor
    /// keybindings on XF86AudioRaiseVolume so the change goes through the
    /// configured volume policy.
    #[arg(long, value_name = "STEP", num_args = 0..=1, default_missing_value = "5")]
    volume_up: Option<u8>,

    /// Send VolumeDown to lunchboxd and exit (XF86AudioLowerVolume binding).
    #[arg(long, value_name = "STEP", num_args = 0..=1, default_missing_value = "5")]
    volume_down: Option<u8>,

    /// Send ToggleMute to lunchboxd and exit (XF86AudioMute binding).
    #[arg(long)]
    toggle_mute: bool,

    /// Send BrightnessUp to lunchboxd and exit. Intended for compositor
    /// keybindings on XF86MonBrightnessUp so the change goes through the
    /// configured brightness policy.
    #[arg(long, value_name = "STEP", num_args = 0..=1, default_missing_value = "5")]
    brightness_up: Option<u8>,

    /// Send BrightnessDown to lunchboxd and exit (XF86MonBrightnessDown
    /// binding).
    #[arg(long, value_name = "STEP", num_args = 0..=1, default_missing_value = "5")]
    brightness_down: Option<u8>,
}

/// Send a single one-shot media call to lunchboxd. Used by the volume-
/// and brightness-button CLI flags: the keypress fires the launcher
/// binary with `--volume-up` etc., it connects, calls, exits. lunchboxd
/// handles the policy clamp and broadcasts the corresponding `*Changed`
/// event so the HUD stays in sync.
///
/// `kind` is a short string used only in logs/errors so volume and
/// brightness failures are distinguishable in the journal.
async fn send_media_call(socket_path: &Path, kind: &str, call: MediaCall) -> Result<()> {
    let mut client = IpcClient::connect(socket_path).await?;
    let result = match call {
        MediaCall::VolumeUp(step) => client.volume_up(step).await.map(|_| ()),
        MediaCall::VolumeDown(step) => client.volume_down(step).await.map(|_| ()),
        MediaCall::ToggleMute => client.toggle_mute().await.map(|_| ()),
        MediaCall::BrightnessUp(step) => client.brightness_up(step).await.map(|_| ()),
        MediaCall::BrightnessDown(step) => client.brightness_down(step).await.map(|_| ()),
    };
    match result {
        Ok(()) => Ok(()),
        Err(lunchbox_ipc::IpcError::ServerError(msg)) => {
            // Policy denials look like Forbidden on the wire, which
            // is expected behaviour and mustn't crash the launcher.
            tracing::info!("{} change denied: {}", kind, msg);
            Ok(())
        }
        Err(e) => anyhow::bail!("{} request failed: {}", kind, e),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level)),
        )
        .init();

    tracing::info!("Starting Lunchbox Launcher UI");

    // Determine socket path with fallback to default
    let socket_path = args.socket.unwrap_or_else(default_socket_path);

    if args.stop_current {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async move {
            let client = CommandClient::new(&socket_path);
            match client.stop_current().await {
                Ok(()) => {
                    tracing::info!("stop_current succeeded");
                    Ok(())
                }
                Err(e) => {
                    // "no active session" comes back from ManagementError::NotFound
                    // → wire ErrorCode::NotFound → IpcError::ServerError.
                    // Any other error is fatal for the caller.
                    let msg = e.to_string();
                    if msg.to_ascii_lowercase().contains("no active session") {
                        tracing::debug!("No active session to stop");
                        Ok(())
                    } else {
                        anyhow::bail!("stop_current failed: {}", msg)
                    }
                }
            }
        })?;
        return Ok(());
    }

    if let Some(step) = args.volume_up {
        let step = if step == 0 { DEFAULT_VOLUME_STEP } else { step };
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_media_call(
            &socket_path,
            "Volume",
            MediaCall::VolumeUp(step),
        ))?;
        return Ok(());
    }

    if let Some(step) = args.volume_down {
        let step = if step == 0 { DEFAULT_VOLUME_STEP } else { step };
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_media_call(
            &socket_path,
            "Volume",
            MediaCall::VolumeDown(step),
        ))?;
        return Ok(());
    }

    if args.toggle_mute {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_media_call(
            &socket_path,
            "Volume",
            MediaCall::ToggleMute,
        ))?;
        return Ok(());
    }

    if let Some(step) = args.brightness_up {
        let step = if step == 0 {
            DEFAULT_BRIGHTNESS_STEP
        } else {
            step
        };
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_media_call(
            &socket_path,
            "Brightness",
            MediaCall::BrightnessUp(step),
        ))?;
        return Ok(());
    }

    if let Some(step) = args.brightness_down {
        let step = if step == 0 {
            DEFAULT_BRIGHTNESS_STEP
        } else {
            step
        };
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_media_call(
            &socket_path,
            "Brightness",
            MediaCall::BrightnessDown(step),
        ))?;
        return Ok(());
    }

    if args.screen_off || args.screen_on {
        let on = args.screen_on;
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            let mut client = IpcClient::connect(&socket_path).await?;
            match client.set_screen_power(on).await {
                // A suppressed blank is the expected answer while an activity
                // is up, not a failure: swayidle fires on its timer regardless.
                Ok(false) => tracing::info!("Screen blank suppressed: an activity is running"),
                Ok(true) => tracing::info!(on, "Screen power set"),
                Err(e) => anyhow::bail!("Screen power request failed: {}", e),
            }
            Ok::<(), anyhow::Error>(())
        })?;
        return Ok(());
    }

    if args.admin_idle_timeout {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            let client = CommandClient::new(&socket_path);
            match client.admin_idle_timeout().await {
                Ok(true) => tracing::info!("Administrator mode left after the idle timeout"),
                Ok(false) => tracing::debug!("Idle timeout: nothing to do"),
                Err(e) => tracing::warn!(error = %e, "Idle timeout request failed"),
            }
        });
        return Ok(());
    }

    // Run GTK application
    let application = app::LauncherApp::new(socket_path);
    let exit_code = application.run();

    std::process::exit(exit_code);
}
