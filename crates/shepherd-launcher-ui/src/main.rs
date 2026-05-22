//! Shepherd Launcher UI - Main grid interface
//!
//! This is the primary user-facing shell for the kiosk-style environment.
//! It displays available entries from shepherdd and allows launching them.

mod app;
mod client;
mod grid;
mod state;
mod tile;

use crate::client::CommandClient;
use anyhow::Result;
use clap::Parser;
use shepherd_api::{Command, ErrorCode, ResponsePayload, ResponseResult};
use shepherd_ipc::IpcClient;
use shepherd_util::default_socket_path;
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

/// Default step (percent) for the volume up/down CLI flags. Matches the
/// keyboard step most desktops use for XF86Audio* keys.
const DEFAULT_VOLUME_STEP: u8 = 5;

/// Shepherd Launcher - Child-friendly kiosk launcher
#[derive(Parser, Debug)]
#[command(name = "shepherd-launcher")]
#[command(about = "GTK4 launcher UI for shepherdd", long_about = None)]
struct Args {
    /// Socket path for shepherdd connection (or set SHEPHERD_SOCKET env var)
    #[arg(short, long, env = "SHEPHERD_SOCKET")]
    socket: Option<PathBuf>,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Send StopCurrent to shepherdd and exit (for compositor keybindings)
    #[arg(long)]
    stop_current: bool,

    /// Exit 0 if no activity is running (idle/screen-off is allowed), 1 if a session
    /// is active (idle should be suppressed). Used by swayidle to gate DPMS.
    #[arg(long)]
    is_idle_allowed: bool,

    /// Send VolumeUp to shepherdd and exit. Intended for compositor
    /// keybindings on XF86AudioRaiseVolume so the change goes through the
    /// configured volume policy.
    #[arg(long, value_name = "STEP", num_args = 0..=1, default_missing_value = "5")]
    volume_up: Option<u8>,

    /// Send VolumeDown to shepherdd and exit (XF86AudioLowerVolume binding).
    #[arg(long, value_name = "STEP", num_args = 0..=1, default_missing_value = "5")]
    volume_down: Option<u8>,

    /// Send ToggleMute to shepherdd and exit (XF86AudioMute binding).
    #[arg(long)]
    toggle_mute: bool,
}

/// Send a single one-shot command to shepherdd. Used by the volume-button
/// CLI flags below: the keypress fires the launcher binary with `--volume-up`
/// or similar, it connects, sends the command, and exits. shepherdd handles
/// the policy clamp and broadcasts a VolumeChanged event so the HUD stays
/// in sync.
async fn send_volume_command(socket_path: &Path, command: Command) -> Result<()> {
    let mut client = IpcClient::connect(socket_path).await?;
    let response = client.send(command).await?;
    match response.result {
        ResponseResult::Ok(ResponsePayload::VolumeSet) => Ok(()),
        ResponseResult::Ok(ResponsePayload::VolumeDenied { reason }) => {
            tracing::info!("Volume change denied: {}", reason);
            Ok(())
        }
        ResponseResult::Ok(payload) => {
            anyhow::bail!("Unexpected volume response: {:?}", payload)
        }
        ResponseResult::Err(err) => anyhow::bail!("Volume request failed: {}", err.message),
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

    tracing::info!("Starting Shepherd Launcher UI");

    // Determine socket path with fallback to default
    let socket_path = args.socket.unwrap_or_else(default_socket_path);

    if args.stop_current {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async move {
            let client = CommandClient::new(&socket_path);
            match client.stop_current().await {
                Ok(response) => match response.result {
                    ResponseResult::Ok(ResponsePayload::Stopped) => {
                        tracing::info!("StopCurrent succeeded");
                        Ok(())
                    }
                    ResponseResult::Err(err) if err.code == ErrorCode::NoActiveSession => {
                        tracing::debug!("No active session to stop");
                        Ok(())
                    }
                    ResponseResult::Err(err) => {
                        anyhow::bail!("StopCurrent failed: {}", err.message)
                    }
                    ResponseResult::Ok(payload) => {
                        anyhow::bail!("Unexpected StopCurrent response: {:?}", payload)
                    }
                },
                Err(e) => anyhow::bail!("Failed to send StopCurrent: {}", e),
            }
        })?;
        return Ok(());
    }

    if let Some(step) = args.volume_up {
        let step = if step == 0 { DEFAULT_VOLUME_STEP } else { step };
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_volume_command(
            &socket_path,
            Command::VolumeUp { step },
        ))?;
        return Ok(());
    }

    if let Some(step) = args.volume_down {
        let step = if step == 0 { DEFAULT_VOLUME_STEP } else { step };
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_volume_command(
            &socket_path,
            Command::VolumeDown { step },
        ))?;
        return Ok(());
    }

    if args.toggle_mute {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(send_volume_command(&socket_path, Command::ToggleMute))?;
        return Ok(());
    }

    if args.is_idle_allowed {
        let runtime = tokio::runtime::Runtime::new()?;
        let session_active = runtime.block_on(async {
            let client = CommandClient::new(&socket_path);
            match client.get_state().await {
                Ok(response) => match response.result {
                    ResponseResult::Ok(ResponsePayload::State(state)) => {
                        state.current_session.is_some()
                    }
                    _ => false,
                },
                Err(_) => false,
            }
        });
        // Exit 1 (false) when a session is active so swayidle skips the DPMS command.
        // Exit 0 (true/success) when idle is allowed.
        std::process::exit(if session_active { 1 } else { 0 });
    }

    // Run GTK application
    let application = app::LauncherApp::new(socket_path);
    let exit_code = application.run();

    std::process::exit(exit_code);
}
