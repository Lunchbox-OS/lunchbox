// Wire types for the lunchbox-http REST API.
//
// The payload shapes are **generated** from the Rust definitions — see
// `wire-types.generated.ts` — and are only re-exported here so that the rest of
// the UI keeps importing wire types from one place. To change one, edit
// `crates/lunchbox-api/src/types.rs` and run
// `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`; the drift test fails
// CI until the regenerated file is checked in.
//
// They used to be hand-written, which meant nothing connected them to the
// daemon: `tsc` checks TypeScript against TypeScript, so a renamed field or a
// new enum variant was invisible until it broke at runtime. That is exactly how
// the companion's Kotlin mirrors drifted twice before they were generated.
//
// What still belongs in this file is presentation — turning a wire value into
// something a person reads.

export type {
  AudioOutput,
  AudioOutputKind,
  AudioOutputRecord,
  BrightnessInfo,
  BrightnessRestrictions,
  Connectivity,
  DailyOverride,
  Diagnostic,
  DiagnosticCode,
  DiagnosticSet,
  DiagnosticSeverity,
  DiagnosticSubject,
  Duration,
  EntryKindTag,
  EntryView,
  GroupView,
  HealthStatus,
  InternetStatusView,
  NetworkAddressView,
  NetworkInterfaceKind,
  NetworkInterfaceView,
  NetworkSource,
  NetworkStatusView,
  ReasonCode,
  ServiceStateSnapshot,
  SessionInfo,
  SessionState,
  TokenStatus,
  UsageStat,
  VolumeInfo,
  VolumeRestrictions,
  SavedWifiNetwork,
  WebListenerState,
  WebListenerView,
  WifiJoinFailure,
  WifiJoinFailureKind,
  WifiJoinRequest,
  WifiJoinState,
  WifiNetwork,
  WifiScanView,
  WifiSecurity,
  WifiView,
  WindowInfo,
  WindowOwner,
} from "./wire-types.generated";

import type { Diagnostic, Duration, ReasonCode } from "./wire-types.generated";

// std::time::Duration serializes as { secs: number, nanos: number }
export function durationToSecs(d: Duration | null | undefined): number {
  if (!d) return 0;
  return d.secs + d.nanos / 1e9;
}

// Moved to ../shared/duration so the config editor can use them without
// importing the API layer; re-exported here so existing call sites keep working.
export { formatDuration, formatDurationHuman } from "../shared/duration";

export function reasonLabel(r: ReasonCode): string {
  switch (r.code) {
    case "outside_time_window":
      return "Outside allowed hours";
    case "quota_exhausted":
      return "Daily time limit reached";
    case "cooldown_active":
      return "Cooling down";
    case "session_active":
      return "Another session is running";
    case "unsupported_kind":
      return "Not supported on this device";
    case "not_ready":
      return "Still starting up";
    case "disabled":
      return r.reason ?? "Disabled";
    case "internet_unavailable":
      return "No internet connection";
    case "manually_disabled":
      return "Disabled for today";
    case "required_input_unavailable":
      return r.devices.length > 0
        ? `Requires: ${r.devices.join(", ")}`
        : "Requires an input device";
    case "tokens_insufficient":
      return "Not enough time earned yet";
    // Applies to every entry at once and clears itself when the caregiver
    // leaves the mode, so it reads as a state of the device rather than a
    // restriction on this activity.
    case "admin_mode":
      return "Administrator mode is on";
    // Deliberately vague: this is the child-facing half, and nothing they can
    // do fixes it. The detail is the matching diagnostic on the admin side.
    case "protection_unavailable":
      return "Unavailable until set up";
    // Name the category, so it's clear the limit is shared rather than
    // specific to this activity.
    case "group_restricted":
      return `${r.label}: ${reasonLabel(r.reason)}`;
  }
}

export function diagnosticEntryId(d: Diagnostic): string | null {
  return d.subject.type === "entry" ? d.subject.entry_id : null;
}
