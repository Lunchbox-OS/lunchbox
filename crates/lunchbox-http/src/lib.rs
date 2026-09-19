//! Local-network management HTTP API for lunchboxd.
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
//!   `lunchbox_api::Event`.
//!
//! There used to be a full REST surface (`/entries`, `/sessions`,
//! `/volume`, ...) but every consumer now speaks the RPC endpoint,
//! and adding a new operation shouldn't require touching four places
//! for one trait method.

pub mod auth;
pub mod files;
pub mod handlers;
pub mod state;
pub mod tls;
pub mod web_assets;

pub use auth::{AuthSources, Identity};
pub use files::FileService;
pub use state::AppState;

use anyhow::Context;
use lunchbox_config::ManagementApiConfig;
use lunchbox_management::{AdminAuthority, WebAuth, WebListenerHandle};
use lunchbox_util::ProtectedFiles;
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
    web: Option<Arc<WebAuth>>,
    files: Option<Arc<dyn ProtectedFiles>>,
    /// Names to put in a generated certificate's SAN list, so a parent who
    /// reaches the device by hostname does not get a second warning about the
    /// name on top of the one about the issuer.
    hostnames: Vec<String>,
}

impl HttpServer {
    pub fn new(state: AppState, config: ManagementApiConfig) -> Self {
        Self {
            state,
            config,
            admin: None,
            listener_status: WebListenerHandle::default(),
            web: None,
            files: None,
            hostnames: Vec::new(),
        }
    }

    /// Plug in the web credential store (issue #156).
    ///
    /// Not an `Option`: a management API without a store is the fail-open
    /// state this replaced, and [`Self::run`] refuses to serve without one.
    /// An embedding that genuinely wants the trait over HTTP with no login
    /// machinery builds the router directly with
    /// [`AuthSources::without_credential_store`].
    pub fn with_web_auth(mut self, web: Arc<WebAuth>) -> Self {
        self.web = Some(web);
        self
    }

    /// Where a generated TLS certificate is kept. Required for
    /// `tls.mode = "self_signed"`; ignored by every other mode.
    pub fn with_protected_files(mut self, files: Arc<dyn ProtectedFiles>) -> Self {
        self.files = Some(files);
        self
    }

    /// Extra DNS names for a generated certificate.
    pub fn with_hostnames(mut self, hostnames: Vec<String>) -> Self {
        self.hostnames = hostnames;
        self
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
        let tls = self.build_tls(&addr)?;
        // Refusing to start is the point: a server built without a store
        // would authenticate nobody and therefore admit everybody, on a
        // device whose first boot has no other credential at all. Better a
        // daemon that will not come up than one that comes up open.
        let web = self.web.clone().context(
            "the management API was built without a web credential store, which would serve \
             administration to anyone who can reach the port",
        )?;
        let sources = AuthSources::new(web)
            .with_static_token(self.config.auth_token.clone())
            .with_admin(self.admin.clone())
            .with_secure_cookies(tls.is_some());
        let app = handlers::router(self.state, sources);
        let listener = bind_with_retry(addr, self.config.bind_retry).await?;
        // The bound address, not the configured one: with `port = 0` they are
        // different, and this is the one somebody can connect to.
        let bound = listener.local_addr().unwrap_or(addr);
        // Published, not just logged: the scheme decides whether the URLs the
        // network page and the setup card hand out say `http` or `https`, and
        // a TLS listener answers a plaintext request with a reset (issue #182).
        let tls_on = tls.is_some();
        self.listener_status.set_listening(bound, tls_on);
        let scheme = if tls_on { "https" } else { "http" };
        info!(addr = %bound, scheme, "Management API listening");

        // `ConnectInfo` rather than a bare service: the login throttle counts
        // failures per peer, and a peer it cannot see is one bucket for the
        // whole network — which would let a guesser lock the parent out.
        let service = app.into_make_service_with_connect_info::<SocketAddr>();

        match tls {
            None => {
                axum::serve(listener, service)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.wait_for(|v| *v).await;
                    })
                    .await?;
            }
            Some(config) => {
                let std_listener = listener.into_std()?;
                let handle = axum_server::Handle::new();
                let shutdown_handle = handle.clone();
                tokio::spawn(async move {
                    let _ = shutdown_rx.wait_for(|v| *v).await;
                    // The same grace the plaintext path gets: in-flight
                    // requests finish, a hung one does not hold up shutdown.
                    shutdown_handle.graceful_shutdown(Some(Duration::from_secs(5)));
                });
                axum_server::from_tcp_rustls(
                    std_listener,
                    axum_server::tls_rustls::RustlsConfig::from_config(config),
                )
                .handle(handle)
                .serve(service)
                .await?;
            }
        }
        Ok(())
    }

    /// Resolve the configured mode into a rustls config, or `None` for
    /// plaintext.
    ///
    /// Installing the `ring` provider here rather than in `main`: this is the
    /// only place in the workspace that terminates TLS, and a provider
    /// installed lazily at the point of use cannot be forgotten by a second
    /// binary that links this crate. `install_default` returning `Err` means
    /// somebody already installed one, which is fine.
    fn build_tls(&self, addr: &SocketAddr) -> anyhow::Result<Option<Arc<rustls::ServerConfig>>> {
        if !self.config.tls.is_tls() {
            return Ok(None);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let files = self.files.clone().context(
            "TLS is configured but the server was built without a place to keep a certificate",
        )?;
        let addresses = local_addresses(addr);
        tls::server_config(&self.config.tls, &files, &self.hostnames, &addresses)
    }
}

/// Addresses to name in a generated certificate.
///
/// The bind address itself when it is a concrete one; loopback always, because
/// `dev shot` and anything else on the device reaches it that way. A wildcard
/// bind contributes nothing — a certificate cannot name "every address this
/// machine will ever have" — so a device on a DHCP lease that moves will show
/// a name mismatch until the certificate is regenerated. That is a known
/// sharp edge of the self-signed mode and the reason `files` mode exists.
fn local_addresses(addr: &SocketAddr) -> Vec<std::net::IpAddr> {
    let mut out = vec![
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
    ];
    let bind = addr.ip();
    if !bind.is_unspecified() && !bind.is_loopback() {
        out.push(bind);
    }
    out
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
