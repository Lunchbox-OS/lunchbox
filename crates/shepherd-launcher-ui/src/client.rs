//! IPC client wrapper for the launcher UI

use anyhow::{Context, Result};
use shepherd_api::{ReasonCode, ServiceStateSnapshot};
use shepherd_ipc::{IpcClient, LaunchOutcome};
use shepherd_util::EntryId;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::state::{LauncherState, SharedState};

/// Messages from UI to client task
#[derive(Debug)]
#[allow(dead_code)]
pub enum ClientCommand {
    /// Request to launch an entry
    Launch(EntryId),
    /// Request to stop current session
    StopCurrent,
    /// Request fresh state
    RefreshState,
    /// Shutdown the client
    Shutdown,
}

/// Client connection manager
pub struct ServiceClient {
    socket_path: std::path::PathBuf,
    state: SharedState,
    command_rx: mpsc::UnboundedReceiver<ClientCommand>,
}

impl ServiceClient {
    pub fn new(
        socket_path: impl AsRef<Path>,
        state: SharedState,
        command_rx: mpsc::UnboundedReceiver<ClientCommand>,
    ) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            state,
            command_rx,
        }
    }

    /// Run the client connection loop
    pub async fn run(mut self) {
        loop {
            match self.connect_and_run().await {
                Ok(()) => {
                    info!("Client loop exited normally");
                    break;
                }
                Err(e) => {
                    error!(error = %e, "Connection error");
                    self.state.set(LauncherState::Disconnected);

                    // Wait before reconnecting
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn connect_and_run(&mut self) -> Result<()> {
        self.state.set(LauncherState::Connecting);

        info!(path = %self.socket_path.display(), "Connecting to shepherdd");

        let mut client = IpcClient::connect(&self.socket_path)
            .await
            .context("Failed to connect to shepherdd")?;

        info!("Connected to shepherdd");

        // Get initial state (includes entries)
        info!("Fetching initial service_state");
        let snapshot = client.service_state().await?;
        self.apply_snapshot(snapshot);

        // Now consume client for event stream (this sends subscribe_events internally)
        info!("Subscribing to events");
        let mut events = client.subscribe().await?;
        info!("Subscribed to events, entering event loop");

        // Main event loop
        loop {
            tokio::select! {
                // Handle commands from UI
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        ClientCommand::Shutdown => {
                            info!("Shutdown requested");
                            return Ok(());
                        }
                        ClientCommand::Launch(_entry_id) => {
                            // We can't send commands after subscribing since client is consumed
                            // Need to reconnect for commands
                            warn!("Launch command received but cannot send after subscribe");
                            // For now, trigger a reconnect
                            return Ok(());
                        }
                        ClientCommand::StopCurrent => {
                            warn!("Stop command received but cannot send after subscribe");
                            return Ok(());
                        }
                        ClientCommand::RefreshState => {
                            // Trigger reconnect to refresh
                            return Ok(());
                        }
                    }
                }

                // Handle events from shepherdd
                event_result = events.next() => {
                    match event_result {
                        Ok(event) => {
                            info!(event = ?event, "Received event from shepherdd (client.rs)");
                            self.state.handle_event(event);
                        }
                        Err(e) => {
                            error!(error = %e, "Event stream error");
                            return Err(e.into());
                        }
                    }
                }
            }
        }
    }

    /// Translate a `ServiceStateSnapshot` (result of `service_state`)
    /// into the launcher's higher-level `LauncherState`.
    fn apply_snapshot(&self, snapshot: ServiceStateSnapshot) {
        if let Some(session) = snapshot.current_session {
            let now = shepherd_util::now();
            let time_remaining = session.deadline.and_then(|d| {
                if d > now {
                    (d - now).to_std().ok()
                } else {
                    Some(Duration::ZERO)
                }
            });
            self.state.set(LauncherState::SessionActive {
                session_id: session.session_id,
                entry_label: session.label,
                time_remaining,
            });
        } else {
            self.state.set(LauncherState::Idle {
                entries: snapshot.entries,
            });
        }
    }
}

/// Separate command client for sending one-shot RPCs (subscribes
/// consume the connection, so a stateful command channel needs its
/// own client per call).
pub struct CommandClient {
    socket_path: std::path::PathBuf,
}

impl CommandClient {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
        }
    }

    pub async fn launch(&self, entry_id: &EntryId) -> Result<LaunchOutcomeOwned> {
        let mut client = IpcClient::connect(&self.socket_path).await?;
        let outcome = client.launch(entry_id.clone()).await?;
        Ok(outcome.into())
    }

    #[allow(dead_code)]
    pub async fn stop_current(&self) -> Result<()> {
        let mut client = IpcClient::connect(&self.socket_path).await?;
        client
            .stop_current(shepherd_api::StopMode::Graceful)
            .await
            .map_err(Into::into)
    }

    pub async fn get_state(&self) -> Result<ServiceStateSnapshot> {
        let mut client = IpcClient::connect(&self.socket_path).await?;
        client.service_state().await.map_err(Into::into)
    }

    #[allow(dead_code)]
    pub async fn list_entries(&self) -> Result<Vec<shepherd_api::EntryView>> {
        let mut client = IpcClient::connect(&self.socket_path).await?;
        client.list_entries().await.map_err(Into::into)
    }
}

/// Owned + human-friendly launch-outcome shape used by the UI layer.
/// The IPC helper returns the raw wire form; the launcher's error
/// path prefers a rendered reason string.
#[derive(Debug, Clone)]
pub enum LaunchOutcomeOwned {
    Approved {
        session_id: String,
        deadline: Option<chrono::DateTime<chrono::Local>>,
    },
    Denied {
        message: String,
    },
}

impl From<LaunchOutcome> for LaunchOutcomeOwned {
    fn from(v: LaunchOutcome) -> Self {
        match v {
            LaunchOutcome::Approved {
                session_id,
                deadline,
            } => LaunchOutcomeOwned::Approved {
                session_id,
                deadline,
            },
            LaunchOutcome::Denied { reasons } => LaunchOutcomeOwned::Denied {
                message: reasons
                    .iter()
                    .map(reason_to_message)
                    .collect::<Vec<_>>()
                    .join(", "),
            },
        }
    }
}

/// Convert a ReasonCode enum variant to a human-readable message
fn reason_to_message(reason: &ReasonCode) -> &'static str {
    match reason {
        ReasonCode::OutsideTimeWindow { .. } => "Outside allowed time window",
        ReasonCode::QuotaExhausted { .. } => "Daily quota exhausted",
        ReasonCode::CooldownActive { .. } => "Cooldown period active",
        ReasonCode::SessionActive { .. } => "Another session is active",
        ReasonCode::UnsupportedKind { .. } => "Entry type not supported",
        ReasonCode::NotReady { .. } => "Still starting up",
        ReasonCode::Disabled { .. } => "Entry disabled",
        ReasonCode::InternetUnavailable { .. } => "Internet connection unavailable",
        ReasonCode::ManuallyDisabled { .. } => "Disabled by parent for today",
        ReasonCode::RequiredInputUnavailable { .. } => "Requires an input device",
    }
}
