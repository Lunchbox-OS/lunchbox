//! Transport-agnostic management service for shepherdd. See `README.md`.

pub mod auth;
pub mod auto_brightness;
pub mod dispatch;
pub mod error;
pub mod kotlin_types;
pub mod service;
pub mod types;
pub mod wire_schema;

pub use auth::AdminAuthority;
pub use auto_brightness::{AutoAction, AutoBrightnessCurve, AutoBrightnessState};
pub use dispatch::RpcDispatchError;
pub use error::{ManagementError, ManagementResult};
pub use service::{
    AUTO_BRIGHTNESS_SETTING_KEY, DefaultManagementService, ManagementService, RPC_SCHEMA_JSON,
    dispatch_json,
};
pub use types::LaunchOutcome;
