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
use shepherd_api::{EntryKind, EntryKindTag, ErrorCode, ErrorInfo, Event, EventPayload, Response};
use shepherd_ble::{BleServer, BleServerConfig};
use shepherd_config::load_config;
use shepherd_core::{CoreEngine, CoreEvent};
use shepherd_host_api::{
    BrightnessController, DisplayController, HidpiController, HostAdapter, HostEvent, LightSensor,
    NoOpDisplayController, StopMode as HostStopMode, VolumeController,
};
use shepherd_host_linux::{
    LinuxBrightnessController, LinuxHost, LinuxLightSensor, LinuxVolumeController,
    PipeWireAudioRouter, SwaymsgBackend,
};
use shepherd_http::{AppState as HttpAppState, HttpServer};
use shepherd_ipc::{IpcServer, ServerMessage};
use shepherd_management::{
    AUTO_BRIGHTNESS_SETTING_KEY, AutoBrightnessState, DefaultManagementService, ManagementService,
};
use shepherd_store::{AuditEvent, AuditEventType, SqliteStore, Store};
use shepherd_util::{MonotonicInstant, RateLimiter, default_config_path};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

mod display;
mod display_watch;
mod hidpi;
mod input_devices;
mod internet;
mod pairing_display;
mod system_events;

use display::{DisplayManager, WlMirrorLauncher};
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
    brightness: Arc<LinuxBrightnessController>,
    light_sensor: Arc<LinuxLightSensor>,
    ipc: Arc<IpcServer>,
    store: Arc<dyn Store>,
    rate_limiter: RateLimiter,
    internet_monitor: Option<internet::InternetMonitor>,
    input_monitor: Option<input_devices::InputMonitor>,
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

        // Initialize brightness controller. Logged at debug level on hosts
        // without a backlight (most desktops) so it doesn't spam warnings;
        // the controller itself already logs an info line when one is found.
        let brightness = Arc::new(LinuxBrightnessController::new());
        if !brightness.capabilities().available {
            debug!("No backlight detected, brightness control unavailable");
        }

        // Initialize ambient light sensor (for automatic brightness). Absent
        // on most hardware; the controller logs an info line when one is
        // found and stays quiet otherwise.
        let light_sensor = Arc::new(LinuxLightSensor::new());

        // Initialize core engine
        let engine = CoreEngine::new(policy, store.clone(), host.capabilities().clone());

        // Apply Steam config to the host before any preload so the CEF debug
        // flag is created (only) when interstitial auto-dismiss is enabled.
        host.configure_steam(
            engine.policy().service.steam.auto_dismiss.clone(),
            engine.policy().service.steam.launch_timeout,
        );

        // Initialize internet connectivity monitor (if configured)
        let internet_monitor = internet::InternetMonitor::from_policy(engine.policy());

        // Initialize input-device dependency monitor (issue #96). Only runs when
        // some entry declares `requires_input`.
        let input_monitor = input_devices::InputMonitor::from_policy(engine.policy());

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
            brightness,
            light_sensor,
            ipc: Arc::new(ipc),
            store,
            rate_limiter,
            internet_monitor,
            input_monitor,
        })
    }

    async fn run(mut self) -> Result<()> {
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
            // Hide Steam activities until the preloaded client finishes its
            // initial load (issue #76). Seeded here so the very first served
            // snapshot already gates Steam; the host's readiness watcher flips
            // it to ready (see HostEvent::KindReadinessChanged).
            self.engine.set_kind_readiness(EntryKindTag::Steam, false);
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
        let brightness = self.brightness.clone();
        let light_sensor = self.light_sensor.clone();
        let store = self.store.clone();
        // External monitor / docking controller (issue #87). When docking is
        // disabled in config, a no-op controller is used so the management RPCs
        // still resolve. When enabled, the real `DisplayManager` is also handed
        // to a hotplug watcher and initialized below, and to the HiDPI workaround
        // so the two output-mutating controllers coordinate (the HiDPI apply /
        // restore re-asserts the mirror).
        let display_cfg = { engine.lock().await.policy().service.display.clone() };
        let (display_svc, display_manager): (
            Arc<dyn DisplayController>,
            Option<Arc<DisplayManager>>,
        ) = if display_cfg.docking_enabled {
            let mgr = Arc::new(DisplayManager::new(
                Arc::new(SwaymsgBackend),
                Arc::new(WlMirrorLauncher::new()),
                Arc::new(PipeWireAudioRouter::new()),
                display_cfg.mirror_audio,
                ipc_ref.clone(),
                event_tx.clone(),
            ));
            (mgr.clone() as Arc<dyn DisplayController>, Some(mgr))
        } else {
            (Arc::new(NoOpDisplayController), None)
        };

        // The hidpi manager owns both the IPC server handle and the SSE
        // broadcast channel so it can fan `HudScaleChanged` events out to
        // both subscriber populations without being passed them at each
        // call site (the IPC and HTTP handlers can share the same
        // controller via `Arc<dyn HidpiController>`). It also holds the docking
        // controller so it can re-assert the mirror after changing scales.
        let hidpi = Arc::new(XwaylandHidpi::new(
            ipc_ref.clone(),
            event_tx.clone(),
            display_manager.clone(),
        ));

        // Start management transports (HTTP and/or BLE). Both speak the
        // same shepherd_management::ManagementService, so the service is
        // constructed once and shared.
        let (management_api_config, ble_management_config, auto_brightness_policy) = {
            let eng = engine.lock().await;
            (
                eng.policy().service.management_api.clone(),
                eng.policy().service.ble_management.clone(),
                eng.policy().auto_brightness.clone(),
            )
        };

        // Automatic brightness. Offered only when the host actually exposes a
        // light sensor. The runtime on/off state persists in the store; fall
        // back to the config default the first time (or if the store read
        // fails). Enabling is meaningless without a sensor, so force it off.
        let light_sensor_opt: Option<Arc<dyn LightSensor>> =
            if light_sensor.capabilities().available {
                Some(light_sensor.clone() as Arc<dyn LightSensor>)
            } else {
                None
            };
        let initial_auto_enabled = light_sensor_opt.is_some()
            && match store.get_setting(AUTO_BRIGHTNESS_SETTING_KEY) {
                Ok(Some(v)) => v == "true",
                Ok(None) => auto_brightness_policy.enabled,
                Err(e) => {
                    warn!(error = %e, "Failed to read auto-brightness setting; using config default");
                    auto_brightness_policy.enabled
                }
            };
        let auto_brightness_state =
            Arc::new(Mutex::new(AutoBrightnessState::new(initial_auto_enabled)));
        if light_sensor_opt.is_some() {
            info!(
                enabled = initial_auto_enabled,
                poll_secs = auto_brightness_policy.poll_interval.as_secs(),
                "Automatic brightness available",
            );
        }

        // Construct the management service unconditionally: IPC is
        // always on, and now that IPC dispatches through
        // `dispatch_json` it needs `svc` even when HTTP and BLE are
        // both disabled. The service is cheap to construct — it only
        // holds Arcs of already-live objects.
        // Built as a concrete `Arc<DefaultManagementService>` so the
        // auto-brightness poll loop can call the inherent
        // `auto_brightness_tick`, then shared with the transports as
        // `Arc<dyn ManagementService>`.
        let svc_concrete = {
            let ipc_for_broadcast = ipc_ref.clone();
            let event_tx_for_broadcast = event_tx.clone();
            Arc::new(DefaultManagementService {
                engine: engine.clone(),
                store: store.clone(),
                host: host.clone() as Arc<dyn HostAdapter>,
                volume: volume.clone() as Arc<dyn VolumeController>,
                brightness: brightness.clone() as Arc<dyn BrightnessController>,
                light_sensor: light_sensor_opt.clone(),
                auto_brightness: auto_brightness_state.clone(),
                event_tx: event_tx.clone(),
                broadcast_fn: Arc::new(move |event: Event| {
                    ipc_for_broadcast.broadcast_event(event.clone());
                    let _ = event_tx_for_broadcast.send(event);
                }),
                config_path: config_path.clone(),
                shutdown_tx: shutdown_tx.clone(),
                hidpi: hidpi.clone() as Arc<dyn HidpiController>,
                display: display_svc.clone(),
            })
        };
        let svc: Arc<dyn ManagementService> = svc_concrete.clone();

        // Automatic-brightness poll loop: sample the light sensor on a timer
        // and let the service decide whether to nudge the backlight. Runs only
        // when a sensor exists; ticks are cheap no-ops while auto is off.
        if light_sensor_opt.is_some() {
            let svc_for_auto = svc_concrete.clone();
            let mut auto_shutdown_rx = shutdown_rx.clone();
            let poll_interval = auto_brightness_policy.poll_interval;
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(poll_interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = ticker.tick() => svc_for_auto.auto_brightness_tick().await,
                        _ = auto_shutdown_rx.changed() => {
                            if *auto_shutdown_rx.borrow() {
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Construct the BLE server first so its ClaimMachine can be
        // handed to HttpServer as the source of unified admin bearer
        // tokens. If BLE isn't configured, HTTP falls back to its
        // static-token-only auth.
        let (ble_handle, admin_authority): (
            Option<tokio::task::JoinHandle<()>>,
            Option<Arc<dyn shepherd_management::AdminAuthority>>,
        ) = match ble_management_config {
            Some(ble_cfg) => {
                let bsc = BleServerConfig {
                    device_name: ble_cfg.device_name,
                    firmware_version: env!("CARGO_PKG_VERSION").to_string(),
                    admin_record_path: ble_cfg.admin_record_path,
                    reset_sentinel_path: ble_cfg.reset_sentinel_path,
                    adapter: ble_cfg.adapter,
                };
                // `shepherd-pairing-display` is spawned per pairing
                // attempt to render the Numeric Comparison passkey on
                // the TV. If the binary is missing the pairing path
                // still completes — the user just won't have an
                // on-device visual to compare against.
                let display = Arc::new(pairing_display::SwayPairingDisplay::new());
                match BleServer::new(bsc, svc.clone(), display) {
                    Ok(server) => {
                        let authority =
                            server.claim_machine() as Arc<dyn shepherd_management::AdminAuthority>;
                        let rx = shutdown_rx.clone();
                        let handle = tokio::spawn(async move {
                            if let Err(e) = server.run(rx).await {
                                error!(error = %e, "BLE management server error");
                            }
                        });
                        (Some(handle), Some(authority))
                    }
                    Err(e) => {
                        error!(error = %e, "BLE management server failed to initialize");
                        (None, None)
                    }
                }
            }
            None => (None, None),
        };

        let http_handle = match management_api_config {
            Some(api_cfg) => {
                let http_state = HttpAppState { svc: svc.clone() };
                let http_server =
                    HttpServer::new(http_state, api_cfg).with_admin_authority(admin_authority);
                let http_shutdown_rx = shutdown_rx.clone();
                Some(tokio::spawn(async move {
                    if let Err(e) = http_server.run(http_shutdown_rx).await {
                        error!(error = %e, "HTTP management API error");
                    }
                }))
            }
            None => None,
        };

        // System event watcher (logind + NetworkManager). Always running so the
        // suspend cover (issue #73) works regardless of internet gating: it
        // broadcasts SystemSuspending/SystemResumed and asks for a fresh state
        // snapshot on resume via `resume_rx`. When an internet monitor is
        // configured it also nudges it to re-check immediately on resume /
        // network change instead of waiting for the next poll interval.
        let (resume_tx, mut resume_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let recheck_tx = if let Some(monitor) = self.internet_monitor {
            let engine_ref = engine.clone();
            let ipc_for_monitor = ipc_ref.clone();
            let event_tx_for_monitor = event_tx.clone();
            let (recheck_tx, recheck_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                monitor
                    .run(
                        engine_ref,
                        ipc_for_monitor,
                        event_tx_for_monitor,
                        recheck_rx,
                    )
                    .await;
            });
            Some(recheck_tx)
        } else {
            None
        };

        // Input-device dependency monitor (issue #96): tracks which input device
        // types are connected and re-broadcasts availability on hotplug so
        // input-gated entries (e.g. a typing tutor requiring a keyboard) show and
        // hide as hardware is attached/removed.
        if let Some(monitor) = self.input_monitor {
            let engine_ref = engine.clone();
            let ipc_for_monitor = ipc_ref.clone();
            let event_tx_for_monitor = event_tx.clone();
            tokio::spawn(async move {
                monitor
                    .run(engine_ref, ipc_for_monitor, event_tx_for_monitor)
                    .await;
            });
        }

        {
            let ipc_for_sys = ipc_ref.clone();
            let event_tx_for_sys = event_tx.clone();
            let broadcast: system_events::BroadcastFn = Arc::new(move |event: Event| {
                ipc_for_sys.broadcast_event(event.clone());
                let _ = event_tx_for_sys.send(event);
            });
            system_events::spawn_system_event_watchers(broadcast, recheck_tx, resume_tx);
        }

        // Spawn IPC accept task
        let ipc_accept = ipc_ref.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc_accept.run().await {
                error!(error = %e, "IPC server error");
            }
        });

        // Initialize the display arrangement (detect primary, mirror any already
        // connected external) and watch for hotplug events (issue #87).
        if let Some(mgr) = display_manager {
            let init_mgr = mgr.clone();
            tokio::spawn(async move { init_mgr.initialize().await });
            display_watch::spawn(mgr, shutdown_rx.clone());
        }

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

                // Resumed from suspend - push a fresh state snapshot so clients
                // can drop the suspend cover with up-to-date content.
                Some(()) = resume_rx.recv() => {
                    let state = {
                        let engine = engine.lock().await;
                        engine.get_state()
                    };
                    Self::broadcast(&ipc_ref, &event_tx, Event::new(EventPayload::StateChanged(state)));
                }

                // Config file changed on disk
                Some(()) = config_change_rx.recv() => {
                    // Drain any additional buffered events to debounce rapid saves
                    while config_change_rx.try_recv().is_ok() {}
                    Self::handle_config_reload(&engine, &ipc_ref, &event_tx, &config_path).await;
                }

                // IPC messages
                Some(msg) = ipc_messages.recv() => {
                    Self::handle_ipc_message(&svc, &ipc_ref, &store, &rate_limiter, msg).await;
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

        // Same drain treatment for the BLE server. The bluer adapter
        // release happens in `BleServer::run`'s drop guards on
        // ApplicationHandle / AdvertisementHandle / AgentHandle.
        if let Some(mut handle) = ble_handle {
            match tokio::time::timeout(Duration::from_secs(3), &mut handle).await {
                Ok(Ok(())) => info!("BLE server drained"),
                Ok(Err(e)) => warn!(error = %e, "BLE server task failed during shutdown"),
                Err(_) => {
                    warn!("BLE server did not drain within 3s; aborting");
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
                confirm_on_close,
            } => {
                Self::broadcast(
                    ipc,
                    event_tx,
                    Event::new(EventPayload::SessionStarted {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        label: label.clone(),
                        deadline: *deadline,
                        confirm_on_close: *confirm_on_close,
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

                // Matched against the current session by handle payload: the
                // monitor cannot know the session id, so it reports a
                // fabricated one. An unmatched exit belongs to a previous
                // activity whose reap is only surfacing now, and must not end
                // whatever session replaced it (issue #136).
                let core_event = {
                    let mut engine = engine.lock().await;
                    engine.notify_activity_exited(&handle, status.code, now_mono, now)
                };

                info!(
                    has_event = core_event.is_some(),
                    "notify_activity_exited result"
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

            HostEvent::KindReadinessChanged { kind, ready } => {
                let changed = {
                    let mut engine = engine.lock().await;
                    engine.set_kind_readiness(kind, ready)
                };
                if changed {
                    info!(?kind, ready, "Activity kind readiness changed");
                    // Re-broadcast state so the launcher shows/hides the now
                    // (un)gated entries of this kind.
                    let state = {
                        let engine = engine.lock().await;
                        engine.get_state()
                    };
                    Self::broadcast(ipc, event_tx, Event::new(EventPayload::StateChanged(state)));
                }
            }

            HostEvent::SpawnFailed { session_id, error } => {
                error!(session_id = %session_id, error = %error, "Spawn failed");
            }
        }
    }

    /// Handle one incoming message from the IPC socket.
    ///
    /// Requests are dispatched through `shepherd_management::dispatch_json`
    /// — the generated JSON-RPC router keeps this method thin. The two
    /// wire-method names that don't go through the trait are the
    /// subscribe / unsubscribe pair: they flip a per-client
    /// subscription flag on the writer task *after* the response
    /// frame is on the wire, preventing broadcast events from
    /// arriving before the subscribe acknowledgement.
    async fn handle_ipc_message(
        svc: &Arc<dyn ManagementService>,
        ipc: &Arc<IpcServer>,
        store: &Arc<dyn Store>,
        rate_limiter: &Arc<Mutex<RateLimiter>>,
        msg: ServerMessage,
    ) {
        match msg {
            ServerMessage::Request { client_id, request } => {
                if !rate_limiter.lock().await.check(&client_id) {
                    let resp = Response::error(
                        request.request_id,
                        ErrorInfo::new(ErrorCode::RateLimited, "Too many requests"),
                    );
                    let _ = ipc.send_response(&client_id, resp).await;
                    return;
                }

                if request.api_version != shepherd_api::API_VERSION {
                    let resp = Response::error(
                        request.request_id,
                        ErrorInfo::new(
                            ErrorCode::InvalidRequest,
                            format!(
                                "unsupported api_version {} (server speaks {})",
                                request.api_version,
                                shepherd_api::API_VERSION
                            ),
                        ),
                    );
                    let _ = ipc.send_response(&client_id, resp).await;
                    return;
                }

                match request.method.as_str() {
                    "subscribe_events" => {
                        let resp = Response::success(request.request_id, serde_json::Value::Null);
                        let _ = ipc.send_subscribe_response(&client_id, resp).await;
                    }
                    "unsubscribe_events" => {
                        let resp = Response::success(request.request_id, serde_json::Value::Null);
                        let _ = ipc.send_unsubscribe_response(&client_id, resp).await;
                    }
                    _ => {
                        let resp = dispatch_ipc(
                            svc.as_ref(),
                            &request.method,
                            request.params,
                            request.request_id,
                        )
                        .await;
                        let _ = ipc.send_response(&client_id, resp).await;
                    }
                }
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
                rate_limiter.lock().await.remove_client(&client_id);
            }
        }
    }
}

/// Route an RPC to the trait via `dispatch_json` and translate its
/// error shape onto the IPC wire's `ErrorCode`. Kept as a free
/// function (not a `Service` method) so it doesn't drag the full
/// `Service` fixture into the small set of ManagementError → ErrorCode
/// mappings.
async fn dispatch_ipc(
    svc: &dyn ManagementService,
    method: &str,
    params: serde_json::Value,
    request_id: u64,
) -> Response {
    match shepherd_management::dispatch_json(svc, method, params).await {
        Ok(value) => Response::success(request_id, value),
        Err(shepherd_management::RpcDispatchError::MethodNotFound(m)) => Response::error(
            request_id,
            ErrorInfo::new(ErrorCode::MethodNotFound, format!("unknown method '{m}'")),
        ),
        Err(shepherd_management::RpcDispatchError::InvalidParams(msg)) => {
            Response::error(request_id, ErrorInfo::new(ErrorCode::InvalidParams, msg))
        }
        Err(shepherd_management::RpcDispatchError::Serialization(msg)) => {
            Response::error(request_id, ErrorInfo::new(ErrorCode::Internal, msg))
        }
        Err(shepherd_management::RpcDispatchError::Management(e)) => {
            let (code, msg) = match e {
                shepherd_management::ManagementError::NotFound(m) => (ErrorCode::NotFound, m),
                shepherd_management::ManagementError::BadRequest(m) => (ErrorCode::BadRequest, m),
                shepherd_management::ManagementError::Forbidden(m) => (ErrorCode::Forbidden, m),
                shepherd_management::ManagementError::Conflict(m) => (ErrorCode::Conflict, m),
                shepherd_management::ManagementError::Unprocessable(m) => {
                    (ErrorCode::Unprocessable, m)
                }
                shepherd_management::ManagementError::Internal(m) => (ErrorCode::Internal, m),
            };
            Response::error(request_id, ErrorInfo::new(code, msg))
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
