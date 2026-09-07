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
    entry_view: shepherd_api::EntryView,
    group_view: shepherd_api::GroupView,
    entry_kind: shepherd_api::EntryKind,
    entry_kind_tag: shepherd_api::EntryKindTag,
    reason_code: shepherd_api::ReasonCode,
    session_info: shepherd_api::SessionInfo,
    session_state: shepherd_api::SessionState,
    session_end_reason: shepherd_api::SessionEndReason,
    stop_mode: shepherd_api::StopMode,
    daily_override: shepherd_api::DailyOverride,
    usage_stat: shepherd_api::UsageStat,
    volume_info: shepherd_api::VolumeInfo,
    volume_restrictions: shepherd_api::VolumeRestrictions,
    // Reachable only through `list_audio_outputs`, not from `VolumeInfo`, so it
    // has to be rooted explicitly or the companion never gets the type.
    audio_output_record: shepherd_api::AudioOutputRecord,
    brightness_info: shepherd_api::BrightnessInfo,
    brightness_restrictions: shepherd_api::BrightnessRestrictions,
    health_status: shepherd_api::HealthStatus,
    internet_status_view: shepherd_api::InternetStatusView,
    service_state_snapshot: shepherd_api::ServiceStateSnapshot,
    // Reachable only through `network_status` (issue #182); nothing nests it,
    // so it has to be rooted here or neither client gets the type.
    network_status_view: shepherd_api::NetworkStatusView,
    network_interface_view: shepherd_api::NetworkInterfaceView,
    network_address_view: shepherd_api::NetworkAddressView,
    network_interface_kind: shepherd_api::NetworkInterfaceKind,
    address_family: shepherd_api::AddressFamily,
    wifi_view: shepherd_api::WifiView,
    connectivity: shepherd_api::Connectivity,
    network_source: shepherd_api::NetworkSource,
    web_listener_view: shepherd_api::WebListenerView,
    web_listener_state: shepherd_api::WebListenerState,
    window_info: shepherd_api::WindowInfo,
    window_action: shepherd_api::WindowAction,
    diagnostic: shepherd_api::Diagnostic,
    diagnostic_set: shepherd_api::DiagnosticSet,
    diagnostic_code: shepherd_api::DiagnosticCode,
    diagnostic_subject: shepherd_api::DiagnosticSubject,
    diagnostic_severity: shepherd_api::DiagnosticSeverity,
    warning_severity: shepherd_api::WarningSeverity,
    warning_threshold: shepherd_api::WarningThreshold,
    display_mode: shepherd_api::DisplayMode,
    display_state: shepherd_api::DisplayState,
    interstitial_kind: shepherd_api::InterstitialKind,
    input_device_type: shepherd_api::InputDeviceType,
    input_compat_mode: shepherd_api::InputCompatMode,
    event: shepherd_api::Event,
    event_payload: shepherd_api::EventPayload,
    launch_outcome: shepherd_management::LaunchOutcome,
    // Web management authentication (issue #156). All three are reachable only
    // as RPC results, never nested in another payload, so they have to be
    // rooted here or the companion never gets the types.
    web_auth_status: shepherd_management::WebAuthStatus,
    web_session_info: shepherd_management::WebSessionInfo,
    login_request_info: shepherd_management::LoginRequestInfo,
    // Reachable only because this crate sits above shepherd-ble; a generator
    // inside shepherd-management would hit a dependency cycle.
    device_info: shepherd_ble::protocol::DeviceInfo,
    claim_state_tag: shepherd_ble::protocol::ClaimStateTag,
    admin_record: shepherd_ble::admin::AdminRecord,
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
