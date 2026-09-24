//! Changing which wireless network the device is on (issue #194).
//!
//! The counterpart to [`crate::NetworkInfoProvider`], which only reads. Behind
//! a trait for the same two reasons: `lunchbox-management` must not link a
//! D-Bus client, and neither form can be tested against a real access point.
//!
//! **Reads and writes are both here, but they do not run in the same place.**
//! On an installed device the reads happen in `lunchboxd` — NetworkManager's
//! properties need no privilege — while the writes are forwarded to the state
//! custodian, which holds the polkit grant that `lunchboxd`'s own uid must
//! not. One trait covers both because a caller has no business knowing which
//! of its calls crossed a socket; see `lunchbox-host-linux`'s `wifi` module
//! for where the split is actually made.

use async_trait::async_trait;
use lunchbox_api::{SavedWifiNetwork, WifiJoinRequest, WifiJoinState, WifiNetwork};
use thiserror::Error;

/// Why a wireless operation could not be carried out.
///
/// Distinct from [`lunchbox_api::WifiJoinFailure`], which is about a join that
/// was accepted and later came to nothing. This is about a call that never
/// started.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum WifiError {
    /// This device has no wireless adapter, or none NetworkManager manages.
    #[error("This device has no Wi-Fi adapter")]
    NoAdapter,

    /// The device may not write a profile.
    ///
    /// On an installed device this means the custodian holds no
    /// NetworkManager grant — the polkit rule is missing, or `lunchboxd` is
    /// running without a custodian at all. It is reported rather than retried:
    /// there is nothing a caller can do differently, and a UI that says
    /// "failed" here sends somebody looking for a network problem that does
    /// not exist.
    #[error("This device is not allowed to change Wi-Fi settings")]
    NotAuthorized,

    /// No saved profile with that id. Usually a stale list in a UI that has
    /// been open while somebody else forgot the network.
    #[error("That saved network no longer exists")]
    UnknownNetwork,

    /// The request was malformed. Carries
    /// [`lunchbox_api::WifiRequestError`]'s sentence.
    #[error("{0}")]
    Rejected(String),

    /// The backend failed for a reason of its own.
    #[error("Wi-Fi backend error: {0}")]
    Backend(String),
}

pub type WifiResult<T> = Result<T, WifiError>;

/// What the host could see of the wireless world.
///
/// The raw parts, exactly as [`crate::NetworkSnapshot`] is for status: every
/// judgement about what they mean — merging a mesh into one row, ordering,
/// capping for BLE — is made once in
/// [`lunchbox_api::aggregate_networks`], so a second backend cannot quietly
/// disagree with the first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiSnapshot {
    /// Whether a managed wireless adapter exists at all.
    pub supported: bool,
    /// Whether its radio is on.
    pub radio_enabled: bool,
    /// One entry per access point heard, before aggregation.
    pub networks: Vec<WifiNetwork>,
    /// How long ago the last scan finished, in seconds. `None` when none has.
    pub last_scan_age_s: Option<u64>,
}

impl WifiSnapshot {
    /// No adapter. Distinct from an adapter that saw nothing: a UI says "this
    /// device has no Wi-Fi" for one and "no networks found" for the other.
    pub fn unsupported() -> Self {
        Self::default()
    }
}

/// Somewhere to read and change the device's wireless configuration.
#[async_trait]
pub trait WifiController: Send + Sync {
    /// Ask for a fresh scan, and return without waiting for it.
    ///
    /// A scan takes seconds and its results arrive as property changes, so
    /// there is nothing useful to wait for. "Too soon since the last one" is
    /// success: the caller wanted fresh results and fresh results are already
    /// on the way.
    async fn scan(&self) -> WifiResult<()>;

    /// What is in range now.
    ///
    /// Infallible for the same reason [`crate::NetworkInfoProvider::snapshot`]
    /// is: every caller is a page whose only response to an error is to render
    /// nothing, which [`WifiSnapshot::unsupported`] already says, and the
    /// backend is far better placed to log why.
    async fn networks(&self) -> WifiSnapshot;

    /// Profiles this device already knows, wireless ones only.
    async fn saved(&self) -> WifiResult<Vec<SavedWifiNetwork>>;

    /// Remember a network, and join it now if the request says to.
    ///
    /// Returns as soon as the profile is written and — for a join — as soon as
    /// NetworkManager has accepted the activation. It does **not** wait for
    /// the network to come up; ask [`WifiController::join_state`] for that.
    ///
    /// Saving a network there is already a profile for updates that profile
    /// rather than adding a second one. Measured: NetworkManager's D-Bus API
    /// will happily hold two profiles with the same name and SSID, which a
    /// list cannot tell apart.
    async fn save(&self, request: &WifiJoinRequest) -> WifiResult<SavedWifiNetwork>;

    /// Join a network this device already has a profile for.
    async fn connect(&self, id: &str) -> WifiResult<()>;

    /// Delete a saved profile. `false` when there was nothing to delete.
    async fn forget(&self, id: &str) -> WifiResult<bool>;

    /// How the most recent join is going.
    ///
    /// Polled rather than pushed, and rather than awaited: a join takes
    /// anywhere from 3 to 45 seconds, which outlives the companion's 15-second
    /// RPC timeout, so the result cannot be returned by the call that started
    /// it.
    async fn join_state(&self) -> WifiJoinState;

    /// Whether writes can be attempted at all.
    ///
    /// Checked once at startup, not per call, so the UIs can disable a form
    /// before a parent types a password into it. A device answering `false`
    /// also raises a Health diagnostic naming the missing polkit rule.
    async fn can_configure(&self) -> bool;
}

/// A controller for a device with no wireless hardware, and for tests that
/// need the feature absent.
pub struct NullWifi;

#[async_trait]
impl WifiController for NullWifi {
    async fn scan(&self) -> WifiResult<()> {
        Err(WifiError::NoAdapter)
    }

    async fn networks(&self) -> WifiSnapshot {
        WifiSnapshot::unsupported()
    }

    async fn saved(&self) -> WifiResult<Vec<SavedWifiNetwork>> {
        Err(WifiError::NoAdapter)
    }

    async fn save(&self, _request: &WifiJoinRequest) -> WifiResult<SavedWifiNetwork> {
        Err(WifiError::NoAdapter)
    }

    async fn connect(&self, _id: &str) -> WifiResult<()> {
        Err(WifiError::NoAdapter)
    }

    async fn forget(&self, _id: &str) -> WifiResult<bool> {
        Err(WifiError::NoAdapter)
    }

    async fn join_state(&self) -> WifiJoinState {
        WifiJoinState::Idle
    }

    async fn can_configure(&self) -> bool {
        false
    }
}
