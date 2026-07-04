//! GATT identifiers and JSON-RPC envelope schema for the BLE management
//! transport.
//!
//! Wire format on the request/response/events characteristics: each
//! logical frame is `u16` LE length followed by that many bytes of UTF-8
//! JSON, fragmented across ATT writes/notifies if needed. See
//! [`framing`](crate::framing) for the reassembly buffer.

use serde::{Deserialize, Serialize};
use shepherd_management::ManagementError;
use uuid::Uuid;

/// Shepherd Management Service UUID. Stable across firmware versions;
/// the companion app uses this to filter advertisements.
pub const SHEPHERD_MANAGEMENT_SERVICE_UUID: Uuid =
    Uuid::from_u128(0x8c0c0001_3b21_4abc_9e3f_0a9c1f2e3d40);

/// Readable pre-pairing; reports claim state, firmware version, and the
/// supported protocol version.
pub const SHEPHERD_DEVICE_INFO_CHAR_UUID: Uuid =
    Uuid::from_u128(0x8c0c0002_3b21_4abc_9e3f_0a9c1f2e3d40);

/// Client writes length-prefixed JSON-RPC requests here. Encrypted-link
/// required.
pub const SHEPHERD_REQUEST_CHAR_UUID: Uuid =
    Uuid::from_u128(0x8c0c0003_3b21_4abc_9e3f_0a9c1f2e3d40);

/// Server notifies length-prefixed JSON-RPC responses here, correlated
/// to requests by `id`.
pub const SHEPHERD_RESPONSE_CHAR_UUID: Uuid =
    Uuid::from_u128(0x8c0c0004_3b21_4abc_9e3f_0a9c1f2e3d40);

/// Server notifies serialized `shepherd_api::Event` JSON here (mirror of
/// the HTTP SSE stream).
pub const SHEPHERD_EVENTS_CHAR_UUID: Uuid = Uuid::from_u128(0x8c0c0005_3b21_4abc_9e3f_0a9c1f2e3d40);

/// Current BLE management protocol version. Bumped on backwards-
/// incompatible wire changes; the companion app rejects unknown
/// versions.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum logical-frame size accepted on the request characteristic.
/// Prevents a malicious or buggy client from advertising a huge length
/// prefix and forcing the server to buffer indefinitely.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u32,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
}

/// Wire-level error codes. Includes the JSON-RPC-style framing errors
/// (`parse_error`, `invalid_request`, `method_not_found`,
/// `invalid_params`), the claim-machine gates (`not_claimed`,
/// `permission_denied`), and the [`ManagementError`] variants surfaced
/// directly so the companion app can render meaningful UX without
/// string-matching messages.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    NotClaimed,
    AlreadyClaimed,
    PermissionDenied,
    NotFound,
    BadRequest,
    Forbidden,
    Conflict,
    Unprocessable,
    Internal,
}

impl From<&ManagementError> for ErrorCode {
    fn from(e: &ManagementError) -> Self {
        match e {
            ManagementError::NotFound(_) => ErrorCode::NotFound,
            ManagementError::BadRequest(_) => ErrorCode::BadRequest,
            ManagementError::Forbidden(_) => ErrorCode::Forbidden,
            ManagementError::Conflict(_) => ErrorCode::Conflict,
            ManagementError::Unprocessable(_) => ErrorCode::Unprocessable,
            ManagementError::Internal(_) => ErrorCode::Internal,
        }
    }
}

impl RpcResponse {
    pub fn ok(id: u32, result: serde_json::Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u32, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }

    pub fn from_management_err(id: u32, e: ManagementError) -> Self {
        let code = ErrorCode::from(&e);
        let message = match e {
            ManagementError::NotFound(s)
            | ManagementError::BadRequest(s)
            | ManagementError::Forbidden(s)
            | ManagementError::Conflict(s)
            | ManagementError::Unprocessable(s)
            | ManagementError::Internal(s) => s,
        };
        Self::err(id, code, message)
    }
}

/// Shape returned from the `DeviceInfo` characteristic. Readable
/// unencrypted; carries only what the companion app needs to decide
/// whether to initiate pairing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub protocol_version: u32,
    pub firmware_version: String,
    pub claim_state: ClaimStateTag,
    pub device_name: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStateTag {
    Unclaimed,
    Claimed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_request_minimal_round_trip() {
        let req = RpcRequest {
            id: 7,
            method: "health".to_string(),
            params: serde_json::Value::Null,
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: RpcRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.method, "health");
    }

    #[test]
    fn rpc_request_omits_params() {
        // The default `params` field accepts a missing key.
        let req: RpcRequest = serde_json::from_str(r#"{"id":3,"method":"health"}"#).unwrap();
        assert_eq!(req.id, 3);
        assert!(req.params.is_null());
    }

    #[test]
    fn rpc_response_ok_omits_error() {
        let r = RpcResponse::ok(1, serde_json::json!({"x": 1}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("error"));
        assert!(s.contains(r#""result":{"x":1}"#));
    }

    #[test]
    fn rpc_response_err_omits_result() {
        let r = RpcResponse::err(1, ErrorCode::NotFound, "no such entry");
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("result"));
        assert!(s.contains(r#""code":"not_found""#));
    }

    #[test]
    fn management_error_maps_codes() {
        let cases = [
            (ManagementError::NotFound("x".into()), ErrorCode::NotFound),
            (ManagementError::Forbidden("x".into()), ErrorCode::Forbidden),
            (
                ManagementError::BadRequest("x".into()),
                ErrorCode::BadRequest,
            ),
            (ManagementError::Conflict("x".into()), ErrorCode::Conflict),
            (
                ManagementError::Unprocessable("x".into()),
                ErrorCode::Unprocessable,
            ),
            (ManagementError::Internal("x".into()), ErrorCode::Internal),
        ];
        for (err, expected) in cases {
            assert_eq!(ErrorCode::from(&err), expected);
        }
    }

    #[test]
    fn service_uuid_is_stable() {
        // Sanity: the UUID constants are deliberately stable. If a
        // future change rotates them, the companion app's filter
        // breaks. This test exists as a tripwire on that intent.
        assert_eq!(
            SHEPHERD_MANAGEMENT_SERVICE_UUID.to_string(),
            "8c0c0001-3b21-4abc-9e3f-0a9c1f2e3d40"
        );
    }
}
