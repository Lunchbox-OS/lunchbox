//! Local-network management HTTP API for shepherdd.
//!
//! Exposes exactly two endpoints on the LAN:
//!
//! - `POST /api/v1/rpc` — JSON-RPC pass-through into every
//!   `ManagementService` method. Wire shape is
//!   `{ "method": "<name>", "params": <object|null> }` with the trait
//!   method's return value as the response body; errors come back as
//!   4xx/5xx with `{ "error": <code>, "message": <string> }`. See
//!   [`handlers::rpc`] for the code/status mapping.
//! - `GET /api/v1/events` — Server-Sent Events stream of every
//!   `shepherd_api::Event`.
//!
//! There used to be a full REST surface (`/entries`, `/sessions`,
//! `/volume`, ...) but every consumer now speaks the RPC endpoint,
//! and adding a new operation shouldn't require touching four places
//! for one trait method.

pub mod auth;
pub mod handlers;
pub mod state;
pub mod web_assets;

pub use auth::AuthSources;
pub use state::AppState;

use anyhow::Context;
use shepherd_config::ManagementApiConfig;
use shepherd_management::{AdminAuthority, WebListenerHandle};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};

const BIND_RETRY_INTERVAL: Duration = Duration::from_secs(2);

pub struct HttpServer {
    state: AppState,
    config: ManagementApiConfig,
    admin: Option<Arc<dyn AdminAuthority>>,
    listener_status: WebListenerHandle,
}

impl HttpServer {
    pub fn new(state: AppState, config: ManagementApiConfig) -> Self {
        Self {
            state,
            config,
            admin: None,
            listener_status: WebListenerHandle::default(),
        }
    }

    /// Plug in a BLE-claim-derived [`AdminAuthority`] so the bearer
    /// token minted at claim time is accepted on HTTP as well as BLE.
    /// `None` (the default) preserves the legacy static-token-only
    /// behaviour.
    pub fn with_admin_authority(mut self, admin: Option<Arc<dyn AdminAuthority>>) -> Self {
        self.admin = admin;
        self
    }

    /// Publish where this server actually ends up listening (issue #182).
    ///
    /// The configured address is an intention: the bind is retried while its
    /// address does not exist, and `port = 0` means "whatever is free". The
    /// network status page reports the bound address, so it has to come from
    /// the listener rather than from the config.
    pub fn with_listener_status(mut self, listener_status: WebListenerHandle) -> Self {
        self.listener_status = listener_status;
        self
    }

    pub async fn run(self, mut shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<()> {
        let addr = SocketAddr::new(self.config.bind, self.config.port);
        let sources = AuthSources {
            static_token: self.config.auth_token.clone(),
            admin: self.admin.clone(),
        };
        let app = handlers::router(self.state, sources);
        let listener = bind_with_retry(addr, self.config.bind_retry).await?;
        // The bound address, not the configured one: with `port = 0` they are
        // different, and this is the one somebody can connect to.
        let bound = listener.local_addr().unwrap_or(addr);
        self.listener_status.set_listening(bound);
        info!(addr = %bound, "Management HTTP API listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.wait_for(|v| *v).await;
            })
            .await?;
        Ok(())
    }
}

/// Bind a TcpListener, retrying on EADDRNOTAVAIL until either the listener
/// succeeds or `retry_for` elapses. `retry_for == None` means retry forever.
/// Other errors fail immediately.
async fn bind_with_retry(
    addr: SocketAddr,
    retry_for: Option<Duration>,
) -> anyhow::Result<TcpListener> {
    let started = Instant::now();
    let mut warned = false;
    loop {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == io::ErrorKind::AddrNotAvailable => {
                if !warned {
                    warn!(
                        %addr,
                        error = %e,
                        "Management API address not yet available; will retry until it appears",
                    );
                    warned = true;
                }
                if let Some(limit) = retry_for
                    && started.elapsed() >= limit
                {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to bind management API to {addr} within {}s",
                            limit.as_secs()
                        )
                    });
                }
                tokio::time::sleep(BIND_RETRY_INTERVAL).await;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to bind management API to {addr}"));
            }
        }
    }
}
