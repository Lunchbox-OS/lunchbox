//! Bluetooth LE management transport for shepherdd. See `README.md`.

pub mod admin;
pub mod agent;
pub mod claim;
pub mod framing;
pub mod protocol;
pub mod rpc;
pub mod server;

pub use admin::{AdminRecord, AdminRole, AdminStore, check_reset_sentinel};
pub use agent::{NoopPairingDisplay, PairingDisplay, PairingMethod};
pub use claim::{AuthDecision, ClaimMachine, ClaimState, PeerIdentity};
pub use protocol::{
    ErrorCode, RpcError, RpcRequest, RpcResponse, SHEPHERD_DEVICE_INFO_CHAR_UUID,
    SHEPHERD_EVENTS_CHAR_UUID, SHEPHERD_MANAGEMENT_SERVICE_UUID, SHEPHERD_REQUEST_CHAR_UUID,
    SHEPHERD_RESPONSE_CHAR_UUID,
};
pub use server::{BleServer, BleServerConfig};
