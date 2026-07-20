//! Result types returned by [`ManagementService`](crate::ManagementService).

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use shepherd_api::ReasonCode;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub enum LaunchOutcome {
    Approved {
        session_id: String,
        deadline: Option<DateTime<Local>>,
    },
    Denied {
        reasons: Vec<ReasonCode>,
    },
}
