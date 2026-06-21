//! Local-network management HTTP API for shepherdd
//!
//! Exposes REST endpoints and SSE event streaming so parents can
//! control sessions, set daily overrides, and view usage analytics
//! from a browser or mobile app on the LAN.

pub mod auth;
pub mod error;
pub mod handlers;
pub mod state;
pub mod web_assets;

pub use auth::AuthSources;
pub use state::AppState;

use anyhow::Context;
use shepherd_config::ManagementApiConfig;
use shepherd_management::AdminAuthority;
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
}

impl HttpServer {
    pub fn new(state: AppState, config: ManagementApiConfig) -> Self {
        Self {
            state,
            config,
            admin: None,
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

    pub async fn run(self, mut shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<()> {
        let addr = SocketAddr::new(self.config.bind, self.config.port);
        let sources = AuthSources {
            static_token: self.config.auth_token.clone(),
            admin: self.admin.clone(),
        };
        let app = handlers::router(self.state, sources);
        let listener = bind_with_retry(addr, self.config.bind_retry).await?;
        info!(%addr, "Management HTTP API listening");
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
