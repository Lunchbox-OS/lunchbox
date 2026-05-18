//! shepherdd - The shepherd background service
//!
//! This is the main entry point for the shepherdd service.
//! It wires together all the components:
//! - Configuration loading
//! - Store initialization
//! - Core engine
//! - Host adapter (Linux)
//! - IPC server
//! - Volume control

use anyhow::{Context, Result};
use clap::Parser;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use shepherd_api::{
    Command, EntryKind, ErrorCode, ErrorInfo, Event, EventPayload, HealthStatus, Response,
    ResponsePayload, SessionEndReason, StopMode, VolumeInfo, VolumeRestrictions,
};
use shepherd_config::{VolumePolicy, load_config};
use shepherd_core::{CoreEngine, CoreEvent, LaunchDecision, StopDecision};
use shepherd_host_api::{
    HidpiController, HostAdapter, HostEvent, StopMode as HostStopMode, VolumeController,
};
use shepherd_host_linux::{LinuxHost, LinuxVolumeController};
use shepherd_http::{AppState as HttpAppState, HttpServer};
use shepherd_ipc::{IpcServer, ServerMessage};
use shepherd_store::{AuditEvent, AuditEventType, SqliteStore, Store};
use shepherd_util::{ClientId, MonotonicInstant, RateLimiter, default_config_path};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

mod hidpi;
mod internet;

use hidpi::XwaylandHidpi;

/// shepherdd - Policy enforcement service for child-focused computing
#[derive(Parser, Debug)]
#[command(name = "shepherdd")]
#[command(about = "Policy enforcement service for child-focused computing", long_about = None)]
struct Args {
    /// Configuration file path (default: ~/.config/shepherd/config.toml)
    #[arg(short, long, default_value_os_t = default_config_path())]
    config: PathBuf,

    /// Socket path override (or set SHEPHERD_SOCKET env var)
    #[arg(short, long, env = "SHEPHERD_SOCKET")]
    socket: Option<PathBuf>,

    /// Data directory override (or set SHEPHERD_DATA_DIR env var)
    #[arg(short, long, env = "SHEPHERD_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

/// Main service state
struct Service {
    config_path: PathBuf,
    engine: CoreEngine,
    host: Arc<LinuxHost>,
    volume: Arc<LinuxVolumeController>,
    ipc: Arc<IpcServer>,
    store: Arc<dyn Store>,
    rate_limiter: RateLimiter,
    internet_monitor: Option<internet::InternetMonitor>,
}

impl Service {
    async fn new(args: &Args) -> Result<Self> {
        // Load configuration
        let policy = load_config(&args.config)
            .with_context(|| format!("Failed to load config from {:?}", args.config))?;

        info!(
            config_path = %args.config.display(),
            entry_count = policy.entries.len(),
            "Configuration loaded"
        );

        // Determine paths
        let socket_path = args
            .socket
            .clone()
            .unwrap_or_else(|| policy.service.socket_path.clone());

        let data_dir = args
            .data_dir
            .clone()
            .unwrap_or_else(|| policy.service.data_dir.clone());

        // Create data directory
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data directory {:?}", data_dir))?;

        // Initialize store
        let db_path = data_dir.join("shepherdd.db");
        let store: Arc<dyn Store> = Arc::new(
            SqliteStore::open(&db_path)
                .with_context(|| format!("Failed to open database {:?}", db_path))?,
        );

        info!(db_path = %db_path.display(), "Store initialized");

        // Log service start
        store.append_audit(AuditEvent::new(AuditEventType::ServiceStarted))?;

        // Initialize host adapter
        let host = Arc::new(LinuxHost::new());

        // Initialize volume controller
        let volume = Arc::new(LinuxVolumeController::new());
        if volume.capabilities().available {
            info!(
                backend = ?volume.capabilities().backend,
                "Volume controller initialized"
            );
        } else {
            warn!("No sound backend detected, volume control unavailable");
        }

        // Initialize core engine
        let engine = CoreEngine::new(policy, store.clone(), host.capabilities().clone());

        // Initialize internet connectivity monitor (if configured)
        let internet_monitor = internet::InternetMonitor::from_policy(engine.policy());

        // Initialize IPC server
        let mut ipc = IpcServer::new(&socket_path);
        ipc.start().await?;

        info!(socket_path = %socket_path.display(), "IPC server started");

        // Rate limiter: 30 requests per second per client
        let rate_limiter = RateLimiter::new(30, Duration::from_secs(1));

        Ok(Self {
            config_path: args.config.clone(),
            engine,
            host,
            volume,
            ipc: Arc::new(ipc),
            store,
            rate_limiter,
            internet_monitor,
        })
    }

    async fn run(self) -> Result<()> {
        let config_path = self.config_path.clone();

        // Broadcast channel shared by IPC and HTTP SSE
        let (event_tx, _event_rx) = broadcast::channel::<Event>(256);

        // Shutdown signal: any path that should bring down shepherdd flips this
        // to `true`. The main loop, the HTTP server's `with_graceful_shutdown`,
        // and the OS-signal listener task all observe it.
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        // Start host process monitor
        let _monitor_handle = self.host.start_monitor();

        // Preload Steam if any Steam entries are configured so it is ready
        // when a user launches a game (skips Steam's startup sequence)
        let has_steam = self
            .engine
            .policy()
            .entries
            .iter()
            .any(|e| matches!(e.kind, EntryKind::Steam { .. }));
        if has_steam {
            info!("Steam entries detected, preloading Steam in background");
            self.host.preload_steam();
        }

        // Get channels
        let mut host_events = self.host.subscribe();
        let ipc_ref = self.ipc.clone();
        let mut ipc_messages = ipc_ref
            .take_message_receiver()
            .await
            .expect("Message receiver should be available");

        // Wrap mutable state
        let engine = Arc::new(Mutex::new(self.engine));
        let rate_limiter = Arc::new(Mutex::new(self.rate_limiter));
        let host = self.host.clone();
        let volume = self.volume.clone();
        let store = self.store.clone();
        // The hidpi manager owns both the IPC server handle and the SSE
        // broadcast channel so it can fan `HudScaleChanged` events out to
        // both subscriber populations without being passed them at each
        // call site (the IPC and HTTP handlers can share the same
        // controller via `Arc<dyn HidpiController>`).
        let hidpi = Arc::new(XwaylandHidpi::new(ipc_ref.clone(), event_tx.clone()));

        // Start HTTP management API if configured
        let management_api_config = {
            let eng = engine.lock().await;
            eng.policy().service.management_api.clone()
        };
        let http_handle = if let Some(api_cfg) = management_api_config {
            let ipc_for_broadcast = ipc_ref.clone();
            let event_tx_for_broadcast = event_tx.clone();
            let http_state = HttpAppState {
                engine: engine.clone(),
                store: store.clone(),
                host: host.clone() as Arc<dyn HostAdapter>,
                volume: volume.clone() as Arc<dyn VolumeController>,
                event_tx: event_tx.clone(),
                broadcast_fn: Arc::new(move |event: Event| {
                    ipc_for_broadcast.broadcast_event(event.clone());
                    let _ = event_tx_for_broadcast.send(event);
                }),
                config_path: config_path.clone(),
                shutdown_tx: shutdown_tx.clone(),
                hidpi: hidpi.clone() as Arc<dyn HidpiController>,
            };
            let http_server = HttpServer::new(http_state, api_cfg);
            let http_shutdown_rx = shutdown_rx.clone();
            Some(tokio::spawn(async move {
                if let Err(e) = http_server.run(http_shutdown_rx).await {
                    error!(error = %e, "HTTP management API error");
                }
            }))
        } else {
            None
        };

        // Start internet connectivity monitoring (if configured)
        if let Some(monitor) = self.internet_monitor {
            let engine_ref = engine.clone();
            tokio::spawn(async move {
                monitor.run(engine_ref).await;
            });
        }

        // Spawn IPC accept task
        let ipc_accept = ipc_ref.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc_accept.run().await {
                error!(error = %e, "IPC server error");
            }
        });

        // Set up config file watcher
        let (config_change_tx, mut config_change_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watched_path = config_path.clone();
        let _config_watcher: Option<RecommendedWatcher> = {
            let tx = config_change_tx;
            match RecommendedWatcher::new(
                move |result: notify::Result<notify::Event>| {
                    if let Ok(event) = result {
                        let is_relevant = matches!(
                            event.kind,
                            notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                        );
                        if is_relevant && event.paths.iter().any(|p| p == &watched_path) {
                            let _ = tx.send(());
                        }
                    }
                },
                notify::Config::default(),
            ) {
                Ok(mut watcher) => {
                    if let Some(dir) = config_path.parent() {
                        match watcher.watch(dir, RecursiveMode::NonRecursive) {
                            Ok(()) => {
                                info!(
                                    config_path = %config_path.display(),
                                    "Watching config file for changes"
                                );
                                Some(watcher)
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to watch config directory, auto-reload disabled");
                                None
                            }
                        }
                    } else {
                        warn!("Config path has no parent directory, auto-reload disabled");
                        None
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to create config watcher, auto-reload disabled");
                    None
                }
            }
        };

        // Set up signal handlers as a spawned listener that flips the shared
        // shutdown signal. This unifies the OS-signal path with the
        // logout-handler path so the main loop only watches one source.
        let mut sigterm =
            signal(SignalKind::terminate()).context("Failed to create SIGTERM handler")?;
        let mut sigint =
            signal(SignalKind::interrupt()).context("Failed to create SIGINT handler")?;
        let mut sighup = signal(SignalKind::hangup()).context("Failed to create SIGHUP handler")?;
        let signal_shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = sigterm.recv() => info!("Received SIGTERM, shutting down gracefully"),
                _ = sigint.recv() => info!("Received SIGINT, shutting down gracefully"),
                _ = sighup.recv() => info!("Received SIGHUP, shutting down gracefully"),
            }
            let _ = signal_shutdown_tx.send(true);
        });

        // Main event loop
        let tick_interval = Duration::from_millis(100);
        let mut tick_timer = tokio::time::interval(tick_interval);

        info!("Service running");

        loop {
            tokio::select! {
                // Shutdown requested (signal, HTTP logout, or IPC logout)
                Ok(()) = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }

                // Tick timer - check warnings and expiry
                _ = tick_timer.tick() => {
                    let now_mono = MonotonicInstant::now();
                    let now = shepherd_util::now();

                    let events = {
                        let mut engine = engine.lock().await;
                        engine.tick(now_mono, now)
                    };

                    for event in events {
                        Self::handle_core_event(&engine, &host, &ipc_ref, &event_tx, &hidpi, event, now_mono, now).await;
                    }
                }

                // Host events (process exit)
                Some(host_event) = host_events.recv() => {
                    Self::handle_host_event(&engine, &ipc_ref, &event_tx, &hidpi, host_event).await;
                }

                // Config file changed on disk
                Some(()) = config_change_rx.recv() => {
                    // Drain any additional buffered events to debounce rapid saves
                    while config_change_rx.try_recv().is_ok() {}
                    Self::handle_config_reload(&engine, &ipc_ref, &event_tx, &config_path).await;
                }

                // IPC messages
                Some(msg) = ipc_messages.recv() => {
                    Self::handle_ipc_message(&engine, &host, &volume, &ipc_ref, &store, &rate_limiter, &event_tx, &shutdown_tx, &hidpi, &config_path, msg).await;
                }
            }
        }

        // Graceful shutdown
        info!("Shutting down shepherdd");

        // Stop all running sessions
        {
            let engine = engine.lock().await;
            if let Some(session) = engine.current_session() {
                info!(session_id = %session.plan.session_id, "Stopping active session");
                if let Some(handle) = &session.host_handle
                    && let Err(e) = host
                        .stop(
                            handle,
                            HostStopMode::Graceful {
                                timeout: Duration::from_secs(5),
                            },
                        )
                        .await
                {
                    warn!(error = %e, "Failed to stop session gracefully");
                }
            }
        }

        // Restore sway output scales if the XWayland HiDPI workaround was
        // active for the session we just stopped. host.logout() below tears
        // down sway anyway, but this keeps us tidy if logout fails.
        hidpi.restore().await;

        // Stop preloaded Steam (if any) after active sessions are terminated
        host.stop_steam_preload();

        // Exit the desktop session (e.g. `swaymsg exit`). Doing this here, after
        // sessions are stopped and after the HTTP server has begun graceful
        // shutdown, ensures the in-flight logout response is flushed before the
        // browser is torn down with sway.
        if let Err(e) = host.logout().await {
            warn!(error = %e, "Logout (host exit) failed");
        }

        // Wait for the HTTP server to drain. SSE clients will disconnect when
        // sway exits, but we cap the wait so a stuck client cannot block
        // shutdown indefinitely.
        if let Some(mut handle) = http_handle {
            match tokio::time::timeout(Duration::from_secs(3), &mut handle).await {
                Ok(Ok(())) => info!("HTTP server drained"),
                Ok(Err(e)) => warn!(error = %e, "HTTP server task failed during shutdown"),
                Err(_) => {
                    warn!("HTTP server did not drain within 3s; aborting");
                    handle.abort();
                }
            }
        }

        // Log shutdown
        if let Err(e) = store.append_audit(AuditEvent::new(AuditEventType::ServiceStopped)) {
            warn!(error = %e, "Failed to log service shutdown");
        }

        info!("Shutdown complete");
        Ok(())
    }

    /// Broadcast an event to both IPC subscribers and HTTP SSE subscribers
    fn broadcast(ipc: &Arc<IpcServer>, tx: &broadcast::Sender<Event>, event: Event) {
        ipc.broadcast_event(event.clone());
        let _ = tx.send(event);
    }

    async fn handle_config_reload(
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        config_path: &Path,
    ) {
        match load_config(config_path) {
            Ok(policy) => {
                let entry_count = {
                    let event = engine.lock().await.reload_policy(policy);
                    if let CoreEvent::PolicyReloaded { entry_count } = event {
                        entry_count
                    } else {
                        0
                    }
                };
                info!(
                    entry_count,
                    config_path = %config_path.display(),
                    "Config reloaded"
                );
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::PolicyReloaded { entry_count }),
                );
                let state = engine.lock().await.get_state();
                Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
            }
            Err(e) => {
                warn!(error = %e, "Failed to reload config, keeping existing policy");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_core_event(
        engine: &Arc<Mutex<CoreEngine>>,
        host: &Arc<LinuxHost>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        hidpi: &Arc<XwaylandHidpi>,
        event: CoreEvent,
        _now_mono: MonotonicInstant,
        _now: chrono::DateTime<chrono::Local>,
    ) {
        match &event {
            CoreEvent::Warning {
                session_id,
                threshold_seconds,
                time_remaining,
                severity,
                message,
            } => {
                info!(
                    session_id = %session_id,
                    threshold = threshold_seconds,
                    remaining = ?time_remaining,
                    "Warning issued"
                );

                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::WarningIssued {
                        session_id: session_id.clone(),
                        threshold_seconds: *threshold_seconds,
                        time_remaining: *time_remaining,
                        severity: *severity,
                        message: message.clone(),
                    }),
                );
            }

            CoreEvent::ExpireDue { session_id } => {
                info!(session_id = %session_id, "Session expired, stopping");

                // Get the host handle and stop it
                let handle = {
                    let engine = engine.lock().await;
                    engine.current_session().and_then(|s| s.host_handle.clone())
                };

                if let Some(handle) = handle
                    && let Err(e) = host
                        .stop(
                            &handle,
                            HostStopMode::Graceful {
                                timeout: Duration::from_secs(5),
                            },
                        )
                        .await
                {
                    warn!(error = %e, "Failed to stop session gracefully, forcing");
                    let _ = host.stop(&handle, HostStopMode::Force).await;
                }

                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::SessionExpiring {
                        session_id: session_id.clone(),
                    }),
                );
            }

            CoreEvent::SessionStarted {
                session_id,
                entry_id,
                label,
                deadline,
            } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::SessionStarted {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        label: label.clone(),
                        deadline: *deadline,
                    }),
                );
            }

            CoreEvent::SessionEnded {
                session_id,
                entry_id,
                reason,
                duration,
            } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::SessionEnded {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        reason: reason.clone(),
                        duration: *duration,
                    }),
                );

                // Restore the compositor scale if an XWayland HiDPI workaround
                // was in effect (no-op otherwise).
                hidpi.restore().await;

                // Broadcast state change
                let state = {
                    let engine = engine.lock().await;
                    engine.get_state()
                };
                Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
            }

            CoreEvent::PolicyReloaded { entry_count } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::PolicyReloaded {
                        entry_count: *entry_count,
                    }),
                );
            }

            CoreEvent::EntryAvailabilityChanged { entry_id, enabled } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::EntryAvailabilityChanged {
                        entry_id: entry_id.clone(),
                        enabled: *enabled,
                    }),
                );
            }

            CoreEvent::AvailabilitySetChanged => {
                // Time-based availability change - broadcast updated state
                let state = {
                    let engine = engine.lock().await;
                    engine.get_state()
                };
                Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
            }
        }
    }

    async fn handle_host_event(
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
        hidpi: &Arc<XwaylandHidpi>,
        event: HostEvent,
    ) {
        match event {
            HostEvent::Exited { handle, status } => {
                let now_mono = MonotonicInstant::now();
                let now = shepherd_util::now();

                info!(
                    session_id = %handle.session_id,
                    status = ?status,
                    "Host process exited - will end session"
                );

                let core_event = {
                    let mut engine = engine.lock().await;
                    engine.notify_session_exited(status.code, now_mono, now)
                };

                info!(
                    has_event = core_event.is_some(),
                    "notify_session_exited result"
                );

                if let Some(CoreEvent::SessionEnded {
                    session_id,
                    entry_id,
                    reason,
                    duration,
                }) = core_event
                {
                    info!(
                        session_id = %session_id,
                        entry_id = %entry_id,
                        reason = ?reason,
                        duration_secs = duration.as_secs(),
                        "Broadcasting SessionEnded"
                    );
                    Self::broadcast(
                        ipc,
                        event_tx,
                        Event::new(EventPayload::SessionEnded {
                            session_id,
                            entry_id,
                            reason,
                            duration,
                        }),
                    );

                    // Restore the compositor scale (and HUD factor) if an
                    // XWayland HiDPI workaround was in effect for this session.
                    hidpi.restore().await;

                    // Broadcast state change
                    let state = {
                        let engine = engine.lock().await;
                        engine.get_state()
                    };
                    info!("Broadcasting StateChanged");
                    Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
                }
            }

            HostEvent::WindowReady { handle } => {
                debug!(session_id = %handle.session_id, "Window ready");
            }

            HostEvent::SpawnFailed { session_id, error } => {
                error!(session_id = %session_id, error = %error, "Spawn failed");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    async fn handle_ipc_message(
        engine: &Arc<Mutex<CoreEngine>>,
        host: &Arc<LinuxHost>,
        volume: &Arc<LinuxVolumeController>,
        ipc: &Arc<IpcServer>,
        store: &Arc<dyn Store>,
        rate_limiter: &Arc<Mutex<RateLimiter>>,
        event_tx: &broadcast::Sender<Event>,
        shutdown_tx: &tokio::sync::watch::Sender<bool>,
        hidpi: &Arc<XwaylandHidpi>,
        config_path: &Path,
        msg: ServerMessage,
    ) {
        match msg {
            ServerMessage::Request { client_id, request } => {
                // Rate limiting
                {
                    let mut limiter = rate_limiter.lock().await;
                    if !limiter.check(&client_id) {
                        let response = Response::error(
                            request.request_id,
                            ErrorInfo::new(ErrorCode::RateLimited, "Too many requests"),
                        );
                        let _ = ipc.send_response(&client_id, response).await;
                        return;
                    }
                }

                // SubscribeEvents / UnsubscribeEvents must go through dedicated
                // methods so the writer task can flip the subscription flag only
                // AFTER the response frame is on the wire, preventing events from
                // arriving before the subscribe acknowledgement.
                match &request.command {
                    Command::SubscribeEvents => {
                        let response = Response::success(
                            request.request_id,
                            ResponsePayload::Subscribed {
                                client_id: client_id.clone(),
                            },
                        );
                        let _ = ipc.send_subscribe_response(&client_id, response).await;
                        return;
                    }
                    Command::UnsubscribeEvents => {
                        let response =
                            Response::success(request.request_id, ResponsePayload::Unsubscribed);
                        let _ = ipc.send_unsubscribe_response(&client_id, response).await;
                        return;
                    }
                    _ => {}
                }

                let response = Self::handle_command(
                    engine,
                    host,
                    volume,
                    ipc,
                    store,
                    &client_id,
                    request.request_id,
                    request.command,
                    event_tx,
                    shutdown_tx,
                    hidpi,
                    config_path,
                )
                .await;

                let _ = ipc.send_response(&client_id, response).await;
            }

            ServerMessage::ClientConnected { client_id, info } => {
                info!(
                    client_id = %client_id,
                    role = ?info.role,
                    uid = ?info.uid,
                    "Client connected"
                );

                let _ = store.append_audit(AuditEvent::new(AuditEventType::ClientConnected {
                    client_id: client_id.to_string(),
                    role: format!("{:?}", info.role),
                    uid: info.uid,
                }));
            }

            ServerMessage::ClientDisconnected { client_id } => {
                debug!(client_id = %client_id, "Client disconnected");

                let _ = store.append_audit(AuditEvent::new(AuditEventType::ClientDisconnected {
                    client_id: client_id.to_string(),
                }));

                // Clean up rate limiter
                let mut limiter = rate_limiter.lock().await;
                limiter.remove_client(&client_id);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_command(
        engine: &Arc<Mutex<CoreEngine>>,
        host: &Arc<LinuxHost>,
        volume: &Arc<LinuxVolumeController>,
        ipc: &Arc<IpcServer>,
        store: &Arc<dyn Store>,
        client_id: &ClientId,
        request_id: u64,
        command: Command,
        event_tx: &broadcast::Sender<Event>,
        shutdown_tx: &tokio::sync::watch::Sender<bool>,
        hidpi: &Arc<XwaylandHidpi>,
        config_path: &Path,
    ) -> Response {
        let now = shepherd_util::now();
        let now_mono = MonotonicInstant::now();

        match command {
            Command::GetState => {
                let state = engine.lock().await.get_state();
                Response::success(request_id, ResponsePayload::State(state))
            }

            Command::ListEntries { at_time } => {
                let time = at_time.unwrap_or(now);
                let entries = engine.lock().await.list_entries(time);
                Response::success(request_id, ResponsePayload::Entries(entries))
            }

            Command::Launch { entry_id } => {
                let mut eng = engine.lock().await;

                match eng.request_launch(&entry_id, now) {
                    LaunchDecision::Approved(plan) => {
                        // Start the session in the engine
                        let event = eng.start_session(plan.clone(), now, now_mono);

                        // Get the entry kind and any per-entry spawn metadata
                        let entry = eng.policy().get_entry(&entry_id);
                        let entry_kind = entry.map(|e| e.kind.clone());
                        let input_compat =
                            entry.map(|e| e.input_compat.clone()).unwrap_or_default();
                        let input_compat_options =
                            entry.map(|e| e.input_compat_options).unwrap_or_default();
                        let needs_hidpi = entry.is_some_and(|e| e.xwayland_native_resolution);

                        // Build spawn options with log path if capture_child_output is enabled
                        let spawn_options = if eng.policy().service.capture_child_output {
                            let log_dir = &eng.policy().service.child_log_dir;
                            // Create log filename: <entry_id>_<session_id>_<timestamp>.log
                            let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
                            let log_filename = format!(
                                "{}_{}.log",
                                entry_id.as_str().replace(['/', '\\', ' '], "_"),
                                timestamp
                            );
                            let log_path = log_dir.join(log_filename);
                            shepherd_host_api::SpawnOptions {
                                capture_stdout: true,
                                capture_stderr: true,
                                log_path: Some(log_path),
                                input_compat,
                                input_compat_options,
                                ..Default::default()
                            }
                        } else {
                            shepherd_host_api::SpawnOptions {
                                input_compat,
                                input_compat_options,
                                ..Default::default()
                            }
                        };

                        drop(eng); // Release lock before spawning

                        // Apply the XWayland HiDPI workaround before spawning
                        // so the client sees the panel's native scale on its
                        // first map. Restored below if the spawn fails.
                        if needs_hidpi {
                            hidpi.apply().await;
                        }

                        if let Some(kind) = entry_kind {
                            match host
                                .spawn(plan.session_id.clone(), &kind, spawn_options)
                                .await
                            {
                                Ok(handle) => {
                                    // Attach handle to session
                                    let mut eng = engine.lock().await;
                                    eng.attach_host_handle(handle);

                                    // Broadcast session started
                                    if let CoreEvent::SessionStarted {
                                        session_id,
                                        entry_id,
                                        label,
                                        deadline,
                                    } = event
                                    {
                                        Self::broadcast(
                                            ipc,
                                            event_tx,
                                            Event::new(EventPayload::SessionStarted {
                                                session_id: session_id.clone(),
                                                entry_id,
                                                label,
                                                deadline,
                                            }),
                                        );

                                        Response::success(
                                            request_id,
                                            ResponsePayload::LaunchApproved {
                                                session_id,
                                                deadline,
                                            },
                                        )
                                    } else {
                                        Response::error(
                                            request_id,
                                            ErrorInfo::new(
                                                ErrorCode::InternalError,
                                                "Unexpected event",
                                            ),
                                        )
                                    }
                                }
                                Err(e) => {
                                    // Spawn failed: roll back the scale
                                    // change so the launcher comes back to a
                                    // correctly-scaled HUD.
                                    hidpi.restore().await;

                                    // Notify session ended with error and broadcast to subscribers
                                    let mut eng = engine.lock().await;
                                    if let Some(CoreEvent::SessionEnded {
                                        session_id,
                                        entry_id,
                                        reason,
                                        duration,
                                    }) = eng.notify_session_exited(Some(-1), now_mono, now)
                                    {
                                        Self::broadcast(
                                            ipc,
                                            event_tx,
                                            Event::new(EventPayload::SessionEnded {
                                                session_id,
                                                entry_id,
                                                reason,
                                                duration,
                                            }),
                                        );

                                        // Broadcast state change so clients return to idle
                                        let state = eng.get_state();
                                        Self::broadcast(
                                            ipc,
                                            event_tx,
                                            Event::new(EventPayload::StateChanged(state)),
                                        );
                                    }

                                    Response::error(
                                        request_id,
                                        ErrorInfo::new(
                                            ErrorCode::HostError,
                                            format!("Spawn failed: {}", e),
                                        ),
                                    )
                                }
                            }
                        } else {
                            Response::error(
                                request_id,
                                ErrorInfo::new(ErrorCode::EntryNotFound, "Entry not found"),
                            )
                        }
                    }
                    LaunchDecision::Denied { reasons } => {
                        Response::success(request_id, ResponsePayload::LaunchDenied { reasons })
                    }
                }
            }

            Command::StopCurrent { mode } => {
                let mut eng = engine.lock().await;

                // Get handle before stopping in engine
                let handle = eng.current_session().and_then(|s| s.host_handle.clone());

                let reason = match mode {
                    StopMode::Graceful => SessionEndReason::UserStop,
                    StopMode::Force => SessionEndReason::AdminStop,
                };

                match eng.stop_current(reason.clone(), now_mono, now) {
                    StopDecision::Stopped(result) => {
                        // Broadcast SessionEnded event so UIs know to transition
                        info!(
                            session_id = %result.session_id,
                            reason = ?result.reason,
                            "Broadcasting SessionEnded from StopCurrent"
                        );
                        Self::broadcast(
                            ipc,
                            event_tx,
                            Event::new(EventPayload::SessionEnded {
                                session_id: result.session_id,
                                entry_id: result.entry_id,
                                reason: result.reason,
                                duration: result.duration,
                            }),
                        );

                        // Also broadcast StateChanged so UIs can update their entry list
                        let snapshot = eng.get_state();
                        Self::broadcast(
                            ipc,
                            event_tx,
                            Event::new(EventPayload::StateChanged(snapshot)),
                        );

                        drop(eng); // Release lock before host operations

                        // Restore output scales / HUD factor before the host
                        // stops the process so the launcher reappears at its
                        // normal size. (handle_host_event will see the engine
                        // already transitioned and not double-restore.)
                        hidpi.restore().await;

                        // Stop the actual process
                        if let Some(h) = handle {
                            let host_mode = match mode {
                                StopMode::Graceful => HostStopMode::Graceful {
                                    timeout: Duration::from_secs(5),
                                },
                                StopMode::Force => HostStopMode::Force,
                            };
                            let _ = host.stop(&h, host_mode).await;
                        }

                        Response::success(request_id, ResponsePayload::Stopped)
                    }
                    StopDecision::NoActiveSession => Response::error(
                        request_id,
                        ErrorInfo::new(ErrorCode::NoActiveSession, "No active session"),
                    ),
                }
            }

            Command::ReloadConfig => {
                // Check permission
                if let Some(info) = ipc.get_client_info(client_id).await
                    && !info.role.can_reload_config()
                {
                    return Response::error(
                        request_id,
                        ErrorInfo::new(ErrorCode::PermissionDenied, "Admin role required"),
                    );
                }

                match load_config(config_path) {
                    Ok(policy) => {
                        let entry_count = {
                            let event = engine.lock().await.reload_policy(policy);
                            if let CoreEvent::PolicyReloaded { entry_count } = event {
                                entry_count
                            } else {
                                0
                            }
                        };
                        Self::broadcast(
                            ipc,
                            event_tx,
                            Event::new(EventPayload::PolicyReloaded { entry_count }),
                        );
                        let state = engine.lock().await.get_state();
                        Self::broadcast(
                            ipc,
                            event_tx,
                            Event::new(EventPayload::StateChanged(state)),
                        );
                        Response::success(request_id, ResponsePayload::ConfigReloaded)
                    }
                    Err(e) => Response::error(
                        request_id,
                        ErrorInfo::new(
                            ErrorCode::InternalError,
                            format!("Config reload failed: {e}"),
                        ),
                    ),
                }
            }

            Command::SubscribeEvents | Command::UnsubscribeEvents => {
                // Handled before handle_command is called; unreachable in practice.
                unreachable!("subscribe/unsubscribe handled in handle_ipc_message")
            }

            Command::GetHealth => {
                let _eng = engine.lock().await;
                let health = HealthStatus {
                    live: true,
                    ready: true,
                    policy_loaded: true,
                    host_adapter_ok: host.is_healthy(),
                    store_ok: store.is_healthy(),
                };
                Response::success(request_id, ResponsePayload::Health(health))
            }

            Command::ExtendCurrent { by } => {
                // Check permission
                if let Some(info) = ipc.get_client_info(client_id).await
                    && !info.role.can_extend()
                {
                    return Response::error(
                        request_id,
                        ErrorInfo::new(ErrorCode::PermissionDenied, "Admin role required"),
                    );
                }

                let mut eng = engine.lock().await;
                match eng.extend_current(by, now_mono, now) {
                    Some(new_deadline) => {
                        let state = eng.get_state();
                        drop(eng);
                        Self::broadcast(
                            ipc,
                            event_tx,
                            Event::new(EventPayload::StateChanged(state)),
                        );
                        Response::success(
                            request_id,
                            ResponsePayload::Extended {
                                new_deadline: Some(new_deadline),
                            },
                        )
                    }
                    None => Response::error(
                        request_id,
                        ErrorInfo::new(
                            ErrorCode::NoActiveSession,
                            "No active session or session is unlimited",
                        ),
                    ),
                }
            }

            Command::GetVolume => {
                let restrictions = Self::get_current_volume_restrictions(engine).await;

                match volume.get_status().await {
                    Ok(status) => {
                        let info = VolumeInfo {
                            percent: status.percent,
                            muted: status.muted,
                            available: volume.capabilities().available,
                            backend: volume.capabilities().backend.clone(),
                            restrictions,
                        };
                        Response::success(request_id, ResponsePayload::Volume(info))
                    }
                    Err(e) => {
                        let info = VolumeInfo {
                            percent: 0,
                            muted: false,
                            available: false,
                            backend: None,
                            restrictions,
                        };
                        warn!(error = %e, "Failed to get volume status");
                        Response::success(request_id, ResponsePayload::Volume(info))
                    }
                }
            }

            Command::SetVolume { percent } => {
                let restrictions = Self::get_current_volume_restrictions(engine).await;

                if !restrictions.allow_change {
                    return Response::success(
                        request_id,
                        ResponsePayload::VolumeDenied {
                            reason: "Volume changes are not allowed".into(),
                        },
                    );
                }

                let clamped = restrictions.clamp_volume(percent);

                match volume.set_volume(clamped).await {
                    Ok(()) => {
                        // Broadcast volume change
                        if let Ok(status) = volume.get_status().await {
                            Self::broadcast(
                                ipc,
                                event_tx,
                                Event::new(EventPayload::VolumeChanged {
                                    percent: status.percent,
                                    muted: status.muted,
                                }),
                            );
                        }
                        Response::success(request_id, ResponsePayload::VolumeSet)
                    }
                    Err(e) => Response::success(
                        request_id,
                        ResponsePayload::VolumeDenied {
                            reason: e.to_string(),
                        },
                    ),
                }
            }

            Command::ToggleMute => {
                let restrictions = Self::get_current_volume_restrictions(engine).await;

                if !restrictions.allow_mute {
                    return Response::success(
                        request_id,
                        ResponsePayload::VolumeDenied {
                            reason: "Mute toggle is not allowed".into(),
                        },
                    );
                }

                match volume.toggle_mute().await {
                    Ok(()) => {
                        if let Ok(status) = volume.get_status().await {
                            Self::broadcast(
                                ipc,
                                event_tx,
                                Event::new(EventPayload::VolumeChanged {
                                    percent: status.percent,
                                    muted: status.muted,
                                }),
                            );
                        }
                        Response::success(request_id, ResponsePayload::VolumeSet)
                    }
                    Err(e) => Response::success(
                        request_id,
                        ResponsePayload::VolumeDenied {
                            reason: e.to_string(),
                        },
                    ),
                }
            }

            Command::SetMute { muted } => {
                let restrictions = Self::get_current_volume_restrictions(engine).await;

                if !restrictions.allow_mute {
                    return Response::success(
                        request_id,
                        ResponsePayload::VolumeDenied {
                            reason: "Mute toggle is not allowed".into(),
                        },
                    );
                }

                match volume.set_mute(muted).await {
                    Ok(()) => {
                        if let Ok(status) = volume.get_status().await {
                            Self::broadcast(
                                ipc,
                                event_tx,
                                Event::new(EventPayload::VolumeChanged {
                                    percent: status.percent,
                                    muted: status.muted,
                                }),
                            );
                        }
                        Response::success(request_id, ResponsePayload::VolumeSet)
                    }
                    Err(e) => Response::success(
                        request_id,
                        ResponsePayload::VolumeDenied {
                            reason: e.to_string(),
                        },
                    ),
                }
            }

            Command::Logout => {
                info!("Logout requested via IPC");
                let _ = shutdown_tx.send(true);
                Response::success(request_id, ResponsePayload::LoggedOut)
            }

            Command::Ping => Response::success(request_id, ResponsePayload::Pong),
        }
    }

    /// Get the current volume restrictions based on policy and active session
    async fn get_current_volume_restrictions(
        engine: &Arc<Mutex<CoreEngine>>,
    ) -> VolumeRestrictions {
        let eng = engine.lock().await;

        // Check if there's an active session with volume restrictions
        if let Some(session) = eng.current_session()
            && let Some(entry) = eng.policy().get_entry(&session.plan.entry_id)
            && let Some(ref vol_policy) = entry.volume
        {
            return Self::convert_volume_policy(vol_policy);
        }

        // Fall back to global policy
        Self::convert_volume_policy(&eng.policy().volume)
    }

    fn convert_volume_policy(policy: &VolumePolicy) -> VolumeRestrictions {
        VolumeRestrictions {
            max_volume: policy.max_volume,
            min_volume: policy.min_volume,
            allow_mute: policy.allow_mute,
            allow_change: policy.allow_change,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();

    info!(version = env!("CARGO_PKG_VERSION"), "shepherdd starting");

    // Create and run the service
    let service = Service::new(&args).await?;
    service.run().await
}
