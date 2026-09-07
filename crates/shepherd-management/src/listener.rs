//! What the web management interface is actually doing (issue #182).
//!
//! The configured `bind` and `port` are an intention, not an outcome. The
//! daemon retries a bind whose address does not exist yet — a ZeroTier
//! interface still coming up at login is the case
//! `service.management_api.bind_retry_seconds` was added for — and a bind that
//! never succeeds only ever reached a log line on a device whose whole purpose
//! is to be administered from somewhere else.
//!
//! So the HTTP server publishes its real state here and the management service
//! reads it. Lives in this crate rather than in `shepherd-http` because
//! `shepherd-http` depends on *this* one: the other direction is a cycle.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use shepherd_api::{WebListenerState, WebListenerView};

/// A shared, cheap-to-clone view of the web listener's state.
///
/// Written by whoever owns the listener, read by the management service. Not a
/// channel: nothing needs to be woken when this changes — a UI asks for the
/// network status when somebody opens the page, and the answer is whatever is
/// true at that moment.
#[derive(Clone, Debug)]
pub struct WebListenerHandle {
    inner: Arc<RwLock<WebListenerView>>,
}

impl WebListenerHandle {
    /// No web interface is configured. Nothing is wrong; there is nothing to
    /// report a URL for.
    pub fn disabled() -> Self {
        Self::from_view(WebListenerView::disabled())
    }

    /// Configured for `addr` and not serving yet. The state a listener starts
    /// in, and stays in for as long as `bind_retry_seconds` allows.
    ///
    /// `tls` decides the scheme in the URLs a UI offers, so it is taken here
    /// rather than left to each consumer: the answer lives in the config, and
    /// getting it wrong hands somebody a URL their browser cannot open.
    pub fn configured(addr: SocketAddr, tls: bool) -> Self {
        Self::from_view(WebListenerView {
            state: WebListenerState::Binding,
            addr: Some(addr.to_string()),
            port: Some(addr.port()),
            tls,
            error: None,
        })
    }

    fn from_view(view: WebListenerView) -> Self {
        Self {
            inner: Arc::new(RwLock::new(view)),
        }
    }

    /// The listener is serving on `addr`, with or without TLS.
    ///
    /// Both come from the listener rather than from the config: `port = 0`
    /// binds somewhere else entirely, and `tls.mode = "auto"` resolves to
    /// plaintext or self-signed depending on where the bind landed.
    pub fn set_listening(&self, addr: SocketAddr, tls: bool) {
        self.set(WebListenerView {
            state: WebListenerState::Listening,
            addr: Some(addr.to_string()),
            port: Some(addr.port()),
            tls,
            error: None,
        });
    }

    /// The listener gave up. `error` is shown to an administrator verbatim, so
    /// it should read as a reason rather than as a type name.
    pub fn set_failed(&self, error: impl std::fmt::Display) {
        let current = self.get();
        self.set(WebListenerView {
            state: WebListenerState::Failed,
            error: Some(error.to_string()),
            ..current
        });
    }

    /// The current state.
    pub fn get(&self) -> WebListenerView {
        // A poisoned lock here would mean a panic while swapping four fields.
        // Reporting the listener as unknown is a worse answer than reporting
        // the last one written, and neither is worth propagating a panic
        // through a status page.
        match self.inner.read() {
            Ok(view) => view.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set(&self, view: WebListenerView) {
        match self.inner.write() {
            Ok(mut slot) => *slot = view,
            Err(poisoned) => *poisoned.into_inner() = view,
        }
    }
}

impl Default for WebListenerHandle {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        "0.0.0.0:8080".parse().unwrap()
    }

    #[test]
    fn a_configured_listener_starts_out_binding() {
        let handle = WebListenerHandle::configured(addr(), true);
        let view = handle.get();
        assert_eq!(view.state, WebListenerState::Binding);
        assert_eq!(view.addr.as_deref(), Some("0.0.0.0:8080"));
        assert_eq!(view.port, Some(8080));
        assert!(view.tls, "the scheme is known before the bind succeeds");
    }

    #[test]
    fn a_clone_sees_what_the_original_wrote() {
        // The point of the type: the HTTP server holds one end and the
        // management service the other.
        let handle = WebListenerHandle::configured(addr(), false);
        let reader = handle.clone();
        handle.set_listening(addr(), true);
        assert_eq!(reader.get().state, WebListenerState::Listening);
        // `tls.mode = "auto"` is only resolved once the bind lands, so the
        // listener gets the last word on the scheme, not the config.
        assert!(reader.get().tls);
    }

    #[test]
    fn a_failure_keeps_the_address_it_failed_on() {
        // "Failed" without saying what it was trying to bind is not a report
        // anybody can act on.
        let handle = WebListenerHandle::configured(addr(), true);
        handle.set_failed("Address not available");
        let view = handle.get();
        assert_eq!(view.state, WebListenerState::Failed);
        assert_eq!(view.addr.as_deref(), Some("0.0.0.0:8080"));
        assert_eq!(view.error.as_deref(), Some("Address not available"));
    }

    #[test]
    fn the_default_is_no_web_interface_at_all() {
        assert_eq!(
            WebListenerHandle::default().get().state,
            WebListenerState::Disabled
        );
    }
}
