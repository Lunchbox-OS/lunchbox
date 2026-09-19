//! Bluetooth LE management transport for lunchboxd. See `README.md`.

pub mod admin;
pub mod agent;
pub mod claim;
pub mod framing;
pub mod outbox;
pub mod protocol;
pub mod rpc;
pub mod server;

#[cfg(test)]
mod testsupport;

pub use admin::{AdminRecord, AdminRole, AdminStore, check_reset_sentinel};
pub use agent::{NoopPairingDisplay, PairingDisplay, PairingMethod};
pub use claim::{AuthDecision, ClaimMachine, ClaimOutcome, ClaimState, PeerIdentity};
pub use protocol::{
    ErrorCode, LUNCHBOX_DEVICE_INFO_CHAR_UUID, LUNCHBOX_EVENTS_CHAR_UUID,
    LUNCHBOX_MANAGEMENT_SERVICE_UUID, LUNCHBOX_REQUEST_CHAR_UUID, LUNCHBOX_RESPONSE_CHAR_UUID,
    RpcError, RpcRequest, RpcResponse,
};
pub use server::{BleServer, BleServerConfig};
