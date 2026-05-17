//! Background internet connectivity check.
//!
//! `spawn_checker` starts a thread that TCP-connects to a host derived from
//! the supplied URL and updates an `Arc<AtomicBool>` with the result every
//! [`CHECK_INTERVAL`].  The same URL format as shepherdd's
//! `internet.check` config field is accepted (`https://…`, `http://…`,
//! `tcp://host:port`).

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{debug, warn};

const CHECK_INTERVAL: Duration = Duration::from_secs(60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Spawn a background connectivity check thread and return a handle to the
/// latest known online status.  The initial value is `false`; it is updated
/// after the first check completes (typically within [`CHECK_TIMEOUT`]).
pub fn spawn_checker(check_url: String) -> Arc<AtomicBool> {
    let online = Arc::new(AtomicBool::new(false));
    let flag = online.clone();
    std::thread::Builder::new()
        .name("connectivity-check".into())
        .spawn(move || {
            loop {
                flag.store(check_once(&check_url), Ordering::Relaxed);
                std::thread::sleep(CHECK_INTERVAL);
            }
        })
        .ok();
    online
}

fn check_once(url: &str) -> bool {
    let Some((host, port)) = parse_host_port(url) else {
        warn!("unparseable connectivity check URL: {url}");
        return false;
    };
    let addrs = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a,
        Err(e) => {
            debug!("could not resolve connectivity check host {host}: {e}");
            return false;
        }
    };
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CHECK_TIMEOUT) {
            Ok(_) => {
                debug!("connectivity check passed: {url}");
                return true;
            }
            Err(e) => debug!("connectivity check addr {addr} failed: {e}"),
        }
    }
    debug!("connectivity check failed: {url}");
    false
}

fn parse_host_port(url: &str) -> Option<(String, u16)> {
    // Strip scheme prefix.
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    // Take only the host[:port] component (before any path).
    let host_port = rest.split('/').next().unwrap_or(rest).trim();

    let default_port: u16 = if url.starts_with("https://") {
        443
    } else if url.starts_with("http://") {
        80
    } else {
        0 // tcp:// — port must be explicit
    };

    if let Some((host, port_str)) = host_port.rsplit_once(':') {
        let port: u16 = port_str.parse().ok()?;
        if port == 0 {
            return None;
        }
        Some((host.to_string(), port))
    } else if default_port > 0 {
        Some((host_port.to_string(), default_port))
    } else {
        None
    }
}
