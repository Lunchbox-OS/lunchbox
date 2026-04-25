//! Local-network management HTTP API for shepherdd
//!
//! Exposes REST endpoints and SSE event streaming so parents can
//! control sessions, set daily overrides, and view usage analytics
//! from a browser or mobile app on the LAN.

pub mod auth;
pub mod error;
pub mod handlers;
pub mod state;

pub use state::AppState;

use anyhow::Context;
use shepherd_config::ManagementApiConfig;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::info;

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
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("Failed to bind management API to {addr}"))?;
        info!(%addr, "Management HTTP API listening");
        axum::serve(listener, app).await?;
        Ok(())
    }
}
