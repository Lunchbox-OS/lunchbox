//! JSON Schema for the wire types, the source of truth for generated
//! client mirrors.
//!
//! The companion app's Kotlin types used to be hand-written, and drifted:
//! four `ReasonCode` variants went missing, and a renamed `DailyOverride`
//! field went unnoticed until it broke every override lookup on the phone.
//! The RPC schema couldn't catch either, because it only records *type names*
//! from the trait signature, not their fields.
//!
//! [`wire_schema`] closes that gap by emitting the full shape of every type
//! that crosses the wire. `rpc-codegen` renders it to Kotlin; a drift test
//! fails CI if the checked-in output no longer matches.

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::{Map, Value};

/// Every payload type reachable from the management API.
///
/// Listing them as fields of one struct is what puts them all in a single
/// `$defs` block: `schema_for!` walks the graph from here, so a type
/// referenced by anything below comes along automatically. A type that is
/// *only* used as an RPC parameter (never nested in a response) has to be
/// named explicitly, which is why some entries look redundant.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct WireTypes {
    entry_view: lunchbox_api::EntryView,
    group_view: lunchbox_api::GroupView,
    entry_kind: lunchbox_api::EntryKind,
    entry_kind_tag: lunchbox_api::EntryKindTag,
    reason_code: lunchbox_api::ReasonCode,
    session_info: lunchbox_api::SessionInfo,
    session_state: lunchbox_api::SessionState,
    session_end_reason: lunchbox_api::SessionEndReason,
    stop_mode: lunchbox_api::StopMode,
    daily_override: lunchbox_api::DailyOverride,
    usage_stat: lunchbox_api::UsageStat,
    volume_info: lunchbox_api::VolumeInfo,
    volume_restrictions: lunchbox_api::VolumeRestrictions,
    // Reachable only through `list_audio_outputs`, not from `VolumeInfo`, so it
    // has to be rooted explicitly or the companion never gets the type.
    audio_output_record: lunchbox_api::AudioOutputRecord,
    brightness_info: lunchbox_api::BrightnessInfo,
    brightness_restrictions: lunchbox_api::BrightnessRestrictions,
    health_status: lunchbox_api::HealthStatus,
    internet_status_view: lunchbox_api::InternetStatusView,
    service_state_snapshot: lunchbox_api::ServiceStateSnapshot,
    // Reachable only through `network_status` (issue #182); nothing nests it,
    // so it has to be rooted here or neither client gets the type.
    network_status_view: lunchbox_api::NetworkStatusView,
    network_interface_view: lunchbox_api::NetworkInterfaceView,
    network_address_view: lunchbox_api::NetworkAddressView,
    network_interface_kind: lunchbox_api::NetworkInterfaceKind,
    address_family: lunchbox_api::AddressFamily,
    wifi_view: lunchbox_api::WifiView,
    connectivity: lunchbox_api::Connectivity,
    network_source: lunchbox_api::NetworkSource,
    web_listener_view: lunchbox_api::WebListenerView,
    web_listener_state: lunchbox_api::WebListenerState,
    // Choosing a network (issue #194). Rooted here for the same reason as the
    // status types above: `wifi_networks` and `wifi_saved_networks` are the
    // only ways in, and nothing else nests them.
    wifi_scan_view: lunchbox_api::WifiScanView,
    wifi_network: lunchbox_api::WifiNetwork,
    saved_wifi_network: lunchbox_api::SavedWifiNetwork,
    wifi_security: lunchbox_api::WifiSecurity,
    wifi_join_state: lunchbox_api::WifiJoinState,
    wifi_join_failure: lunchbox_api::WifiJoinFailure,
    wifi_join_failure_kind: lunchbox_api::WifiJoinFailureKind,
    wifi_join_request: lunchbox_api::WifiJoinRequest,
    desktop_app: lunchbox_api::DesktopApp,
    window_info: lunchbox_api::WindowInfo,
    window_action: lunchbox_api::WindowAction,
    diagnostic: lunchbox_api::Diagnostic,
    diagnostic_set: lunchbox_api::DiagnosticSet,
    diagnostic_code: lunchbox_api::DiagnosticCode,
    diagnostic_subject: lunchbox_api::DiagnosticSubject,
    diagnostic_severity: lunchbox_api::DiagnosticSeverity,
    warning_severity: lunchbox_api::WarningSeverity,
    warning_threshold: lunchbox_api::WarningThreshold,
    display_mode: lunchbox_api::DisplayMode,
    display_state: lunchbox_api::DisplayState,
    interstitial_kind: lunchbox_api::InterstitialKind,
    input_device_type: lunchbox_api::InputDeviceType,
    input_compat_mode: lunchbox_api::InputCompatMode,
    event: lunchbox_api::Event,
    event_payload: lunchbox_api::EventPayload,
    launch_outcome: lunchbox_management::LaunchOutcome,
    // Web management authentication (issue #156). All three are reachable only
    // as RPC results, never nested in another payload, so they have to be
    // rooted here or the companion never gets the types.
    web_auth_status: lunchbox_management::WebAuthStatus,
    web_session_info: lunchbox_management::WebSessionInfo,
    login_request_info: lunchbox_management::LoginRequestInfo,
    // Reachable only because this crate sits above lunchbox-ble; a generator
    // inside lunchbox-management would hit a dependency cycle.
    device_info: lunchbox_ble::protocol::DeviceInfo,
    claim_state_tag: lunchbox_ble::protocol::ClaimStateTag,
    admin_record: lunchbox_ble::admin::AdminRecord,
    // The admin roster and the enrolment handshake (issue #149). `claim` now
    // answers with the tagged outcome rather than a bare record; the other two
    // live in `lunchbox-management` because both transports return them, and
    // are reachable only as results of the roster RPCs.
    claim_outcome: lunchbox_ble::claim::ClaimOutcome,
    admin_role: lunchbox_ble::admin::AdminRole,
    admin_summary: lunchbox_management::AdminSummary,
    enrolment_request_info: lunchbox_management::EnrolmentRequestInfo,
}

/// The `$defs` block describing every wire type, keyed by Rust type name.
///
/// Returned as a plain object (not a full schema document) because consumers
/// want the definitions, not the synthetic `WireTypes` wrapper.
pub fn wire_schema() -> Map<String, Value> {
    // Inline nothing: every named type must land in `$defs` so it can be
    // rendered as its own Kotlin declaration rather than being expanded at
    // each use site.
    let mut generator = SchemaGenerator::default();
    let schema: Schema = generator.root_schema_for::<WireTypes>();

    schema
        .as_value()
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// [`wire_schema`] as pretty JSON, for embedding in the generated artifacts.
pub fn wire_schema_json() -> String {
    serde_json::to_string_pretty(&Value::Object(wire_schema()))
        .expect("wire schema is serializable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_covers_the_types_that_previously_drifted() {
        let defs = wire_schema();
        for name in [
            "EntryView",
            "GroupView",
            "ReasonCode",
            "DailyOverride",
            "SessionInfo",
        ] {
            assert!(defs.contains_key(name), "missing {name} from $defs");
        }
    }

    #[test]
    fn reason_code_records_every_variant() {
        let defs = wire_schema();
        let reason = defs.get("ReasonCode").expect("ReasonCode in $defs");
        let rendered = serde_json::to_string(reason).unwrap();

        // The four that were missing from the companion, plus the group one.
        for code in [
            "not_ready",
            "required_input_unavailable",
            "tokens_insufficient",
            "group_restricted",
            "quota_exhausted",
        ] {
            assert!(rendered.contains(code), "ReasonCode schema lacks {code}");
        }
    }

    #[test]
    fn string_shaped_newtypes_are_strings() {
        // `LimitSubject` has a hand-written string codec; if the schema ever
        // describes it as an enum object, generated clients would decode the
        // wrong shape.
        let defs = wire_schema();
        let ov = serde_json::to_string(defs.get("DailyOverride").unwrap()).unwrap();
        assert!(
            ov.contains("\"subject\""),
            "DailyOverride should carry a subject field: {ov}"
        );
    }
}
