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

pub use state::AppState;

use anyhow::Context;
use shepherd_config::ManagementApiConfig;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tracing::{info, warn};

const BIND_RETRY_INTERVAL: Duration = Duration::from_secs(2);

pub struct HttpServer {
    state: AppState,
    config: ManagementApiConfig,
}

impl HttpServer {
    pub fn new(state: AppState, config: ManagementApiConfig) -> Self {
        Self { state, config }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let addr = SocketAddr::new(self.config.bind, self.config.port);
        let app = handlers::router(self.state, self.config.auth_token);
        let listener = bind_with_retry(addr, self.config.bind_retry).await?;
        info!(%addr, "Management HTTP API listening");
        axum::serve(listener, app).await?;
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
