//! RPC method dispatch: translate an [`RpcRequest`] into a
//! [`ManagementService`] trait call, then encode the result back as an
//! [`RpcResponse`].
//!
//! Claim-flow methods (`claim`, `factory_reset`) are *not* handled here
//! — they live on [`crate::claim::ClaimMachine`] and the server routes
//! them before falling through to this dispatcher.

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use shepherd_api::{StopMode, WindowAction};
use shepherd_management::ManagementService;
use shepherd_util::EntryId;

use crate::protocol::{ErrorCode, RpcRequest, RpcResponse};

/// Dispatch one `ManagementService` method by name. Returns an
/// `RpcResponse` ready to be framed and notified.
pub async fn dispatch_management(svc: &dyn ManagementService, request: RpcRequest) -> RpcResponse {
    let id = request.id;
    macro_rules! parse {
        ($t:ty) => {
            match serde_json::from_value::<$t>(request.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return RpcResponse::err(id, ErrorCode::InvalidParams, e.to_string());
                }
            }
        };
    }

    match request.method.as_str() {
        "health" => json_ok(id, svc.health().await),
        "service_state" => json_ok(id, svc.service_state().await),

        "list_entries" => {
            let p = parse!(AtTime);
            json_ok(id, svc.list_entries(p.at_or_now()).await)
        }
        "get_entry" => {
            let p = parse!(EntryAtTime);
            match svc.get_entry(&p.id, p.at_or_now()).await {
                Ok(v) => json_ok(id, v),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }

        "current_session" => json_ok(id, svc.current_session().await),
        "launch" => {
            let p = parse!(EntryRef);
            match svc.launch(p.id).await {
                Ok(v) => json_ok(id, v),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }
        "stop_current" => {
            let p = parse!(StopParams);
            match svc.stop_current(p.mode.unwrap_or(StopMode::Graceful)).await {
                Ok(()) => json_ok(id, serde_json::Value::Null),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }
        "extend_current" => {
            let p = parse!(ExtendParams);
            match svc.extend_current(p.seconds).await {
                Ok(v) => json_ok(id, ExtendResult { new_deadline: v }),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }

        "list_overrides" => {
            let p = parse!(DateOpt);
            match svc.list_overrides(p.date_or_today()).await {
                Ok(v) => json_ok(id, v),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }
        "get_override" => {
            let p = parse!(EntryDate);
            match svc.get_override(&p.id, p.date_or_today()).await {
                Ok(v) => json_ok(id, v),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }
        "upsert_override" => {
            let p = parse!(UpsertOverrideParams);
            match svc
                .upsert_override(
                    &p.id,
                    p.date_or_today(),
                    p.availability,
                    p.quota_delta_seconds,
                )
                .await
            {
                Ok(v) => json_ok(id, v),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }
        "delete_override" => {
            let p = parse!(EntryDate);
            match svc.delete_override(&p.id, p.date_or_today()).await {
                Ok(deleted) => json_ok(id, DeleteResult { deleted }),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }

        "usage_all" => {
            let p = parse!(DateRange);
            match svc.usage_all(p.range_from(), p.range_to()).await {
                Ok(v) => json_ok(id, v),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }
        "usage_entry" => {
            let p = parse!(EntryDateRange);
            match svc.usage_entry(&p.id, p.range_from(), p.range_to()).await {
                Ok(v) => json_ok(id, v),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }

        "get_volume" => unwrap(id, svc.get_volume().await),
        "set_volume" => {
            let p = parse!(PercentParam);
            unwrap(id, svc.set_volume(p.percent).await)
        }
        "set_mute" => {
            let p = parse!(MuteParam);
            unwrap(id, svc.set_mute(p.muted).await)
        }

        "get_brightness" => unwrap(id, svc.get_brightness().await),
        "set_brightness" => {
            let p = parse!(PercentParam);
            unwrap(id, svc.set_brightness(p.percent).await)
        }

        "reload_config" => match svc.reload_config().await {
            Ok(entry_count) => json_ok(id, ReloadResult { entry_count }),
            Err(e) => RpcResponse::from_management_err(id, e),
        },

        "logout" => {
            svc.logout().await;
            json_ok(id, serde_json::Value::Null)
        }

        "list_windows" => unwrap(id, svc.list_windows().await),
        "act_on_window" => {
            let p = parse!(ActOnWindowParams);
            match svc.act_on_window(p.id, p.action).await {
                Ok(()) => json_ok(id, serde_json::Value::Null),
                Err(e) => RpcResponse::from_management_err(id, e),
            }
        }

        other => RpcResponse::err(
            id,
            ErrorCode::MethodNotFound,
            format!("unknown method '{other}'"),
        ),
    }
}

fn json_ok<T: Serialize>(id: u32, value: T) -> RpcResponse {
    match serde_json::to_value(value) {
        Ok(v) => RpcResponse::ok(id, v),
        Err(e) => RpcResponse::err(id, ErrorCode::Internal, e.to_string()),
    }
}

fn unwrap<T: Serialize>(id: u32, result: shepherd_management::ManagementResult<T>) -> RpcResponse {
    match result {
        Ok(v) => json_ok(id, v),
        Err(e) => RpcResponse::from_management_err(id, e),
    }
}

// ---------------------------------------------------------------------------
// Param structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct AtTime {
    at: Option<DateTime<Local>>,
}

impl AtTime {
    fn at_or_now(&self) -> DateTime<Local> {
        self.at.unwrap_or_else(shepherd_util::now)
    }
}

#[derive(Debug, Deserialize)]
struct EntryRef {
    id: EntryId,
}

#[derive(Debug, Deserialize)]
struct EntryAtTime {
    id: EntryId,
    #[serde(default)]
    at: Option<DateTime<Local>>,
}

impl EntryAtTime {
    fn at_or_now(&self) -> DateTime<Local> {
        self.at.unwrap_or_else(shepherd_util::now)
    }
}

#[derive(Debug, Deserialize, Default)]
struct StopParams {
    #[serde(default)]
    mode: Option<StopMode>,
}

#[derive(Debug, Deserialize)]
struct ExtendParams {
    seconds: i64,
}

#[derive(Debug, Serialize)]
struct ExtendResult {
    new_deadline: Option<DateTime<Local>>,
}

#[derive(Debug, Deserialize, Default)]
struct DateOpt {
    #[serde(default)]
    date: Option<NaiveDate>,
}

impl DateOpt {
    fn date_or_today(&self) -> NaiveDate {
        self.date
            .unwrap_or_else(|| shepherd_util::now().date_naive())
    }
}

#[derive(Debug, Deserialize)]
struct EntryDate {
    id: EntryId,
    #[serde(default)]
    date: Option<NaiveDate>,
}

impl EntryDate {
    fn date_or_today(&self) -> NaiveDate {
        self.date
            .unwrap_or_else(|| shepherd_util::now().date_naive())
    }
}

#[derive(Debug, Deserialize)]
struct UpsertOverrideParams {
    id: EntryId,
    #[serde(default)]
    date: Option<NaiveDate>,
    #[serde(default)]
    availability: Option<bool>,
    #[serde(default)]
    quota_delta_seconds: Option<i64>,
}

impl UpsertOverrideParams {
    fn date_or_today(&self) -> NaiveDate {
        self.date
            .unwrap_or_else(|| shepherd_util::now().date_naive())
    }
}

#[derive(Debug, Serialize)]
struct DeleteResult {
    deleted: bool,
}

#[derive(Debug, Deserialize, Default)]
struct DateRange {
    #[serde(default)]
    from: Option<NaiveDate>,
    #[serde(default)]
    to: Option<NaiveDate>,
}

impl DateRange {
    fn range_from(&self) -> NaiveDate {
        self.from
            .unwrap_or_else(|| shepherd_util::now().date_naive())
    }
    fn range_to(&self) -> NaiveDate {
        self.to.unwrap_or_else(|| shepherd_util::now().date_naive())
    }
}

#[derive(Debug, Deserialize)]
struct EntryDateRange {
    id: EntryId,
    #[serde(default)]
    from: Option<NaiveDate>,
    #[serde(default)]
    to: Option<NaiveDate>,
}

impl EntryDateRange {
    fn range_from(&self) -> NaiveDate {
        self.from
            .unwrap_or_else(|| shepherd_util::now().date_naive())
    }
    fn range_to(&self) -> NaiveDate {
        self.to.unwrap_or_else(|| shepherd_util::now().date_naive())
    }
}

#[derive(Debug, Deserialize)]
struct PercentParam {
    percent: u8,
}

#[derive(Debug, Deserialize)]
struct MuteParam {
    muted: bool,
}

#[derive(Debug, Serialize)]
struct ReloadResult {
    entry_count: usize,
}

#[derive(Debug, Deserialize)]
struct ActOnWindowParams {
    id: u64,
    action: WindowAction,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::{MockSvc, req};

    #[tokio::test]
    async fn health_dispatches_to_service() {
        let svc = MockSvc::new();
        let resp = dispatch_management(&svc, req(1, "health", serde_json::Value::Null)).await;
        assert_eq!(resp.id, 1);
        assert!(resp.error.is_none());
        assert_eq!(*svc.health_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let svc = MockSvc::new();
        let resp =
            dispatch_management(&svc, req(9, "no_such_method", serde_json::Value::Null)).await;
        assert_eq!(resp.id, 9);
        let err = resp.error.unwrap();
        assert_eq!(err.code, ErrorCode::MethodNotFound);
    }

    #[tokio::test]
    async fn invalid_params_returns_invalid_params() {
        let svc = MockSvc::new();
        // get_entry needs an `id`, so {} is invalid.
        let resp = dispatch_management(&svc, req(3, "get_entry", serde_json::json!({}))).await;
        assert_eq!(resp.error.unwrap().code, ErrorCode::InvalidParams);
    }

    #[tokio::test]
    async fn management_error_maps_to_rpc_error() {
        let svc = MockSvc::new();
        let resp = dispatch_management(
            &svc,
            req(4, "get_entry", serde_json::json!({"id": "missing"})),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn logout_returns_null_result() {
        let svc = MockSvc::new();
        let resp = dispatch_management(&svc, req(5, "logout", serde_json::Value::Null)).await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::Value::Null));
    }
}
