//! Reading the host's own networking, for the management UIs (issue #182).
//!
//! Read-only by design: this is the capability that answers "what is this
//! device's address", not one that changes it.
//!
//! Kept behind a trait for the same reason [`crate::VolumeController`] is —
//! `lunchbox-management` must not link a D-Bus client, and a status page has to
//! be testable without a NetworkManager to talk to.

use async_trait::async_trait;
use lunchbox_api::{Connectivity, NetworkInterfaceView, NetworkSource};

/// What a host was able to say about its own networking.
///
/// The raw parts. Every judgement about what they *mean* — which interface is
/// a way in, what URL reaches the web UI, what order to show them in — is made
/// once in [`lunchbox_api::NetworkStatusView::new`], so a second host
/// implementation cannot quietly disagree with the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkSnapshot {
    pub connectivity: Connectivity,
    /// Which backend answered, so a UI can explain an empty field rather than
    /// rendering a blank.
    pub source: NetworkSource,
    pub interfaces: Vec<NetworkInterfaceView>,
}

impl NetworkSnapshot {
    /// Nothing could be read. Distinct from "read successfully, found
    /// nothing": a UI says "unavailable" for one and "not connected" for the
    /// other.
    pub fn unavailable() -> Self {
        Self {
            connectivity: Connectivity::Unknown,
            source: NetworkSource::Unavailable,
            interfaces: Vec::new(),
        }
    }
}

/// Somewhere to read the host's network state from.
///
/// Deliberately infallible. Every caller is a status page whose only response
/// to an error would be to render "unavailable", which
/// [`NetworkSnapshot::unavailable`] already says — and a provider is far better
/// placed to log *why* than a caller holding an opaque error. Partial answers
/// are normal and expected: no NetworkManager still yields interfaces and
/// addresses, just no SSID.
#[async_trait]
pub trait NetworkInfoProvider: Send + Sync {
    async fn snapshot(&self) -> NetworkSnapshot;
}

/// A provider that knows nothing, for tests and for hosts with no networking
/// backend at all.
pub struct NullNetworkInfo;

#[async_trait]
impl NetworkInfoProvider for NullNetworkInfo {
    async fn snapshot(&self) -> NetworkSnapshot {
        NetworkSnapshot::unavailable()
    }
}

/// A provider that returns whatever it was built with. For tests that need a
/// specific shape of device.
pub struct StaticNetworkInfo(pub NetworkSnapshot);

#[async_trait]
impl NetworkInfoProvider for StaticNetworkInfo {
    async fn snapshot(&self) -> NetworkSnapshot {
        self.0.clone()
    }
}
