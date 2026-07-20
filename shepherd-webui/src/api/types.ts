// Types matching the shepherd-http REST API

// std::time::Duration serializes as { secs: number, nanos: number }
export interface Duration {
  secs: number;
  nanos: number;
}

export function durationToSecs(d: Duration | null | undefined): number {
  if (!d) return 0;
  return d.secs + d.nanos / 1e9;
}

export function formatDuration(secs: number): string {
  if (secs <= 0) return "0:00";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = Math.floor(secs % 60);
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  return `${m}:${String(s).padStart(2, "0")}`;
}

export function formatDurationHuman(secs: number): string {
  if (secs <= 0) return "0 min";
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  if (h > 0 && m > 0) return `${h}h ${m}m`;
  if (h > 0) return `${h}h`;
  return `${m}m`;
}

export type EntryKindTag =
  | "process"
  | "snap"
  | "steam"
  | "flatpak"
  | "vm"
  | "media"
  | "custom";

export type ReasonCode =
  | { code: "outside_time_window"; next_window_start: string | null }
  | { code: "quota_exhausted"; used: Duration; quota: Duration }
  | { code: "cooldown_active"; available_at: string }
  | { code: "session_active"; entry_id: string; remaining: Duration | null }
  | { code: "unsupported_kind"; kind: EntryKindTag }
  | { code: "not_ready"; kind: EntryKindTag }
  | { code: "disabled"; reason: string | null }
  | { code: "internet_unavailable"; check: string | null }
  | { code: "manually_disabled"; until: string }
  | { code: "required_input_unavailable"; devices: string[] }
  | { code: "tokens_insufficient"; balance: Duration; required: Duration }
  | {
      code: "group_restricted";
      group: string;
      label: string;
      reason: ReasonCode;
    };

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
    // Name the category, so it's clear the limit is shared rather than
    // specific to this activity.
    case "group_restricted":
      return `${r.label}: ${reasonLabel(r.reason)}`;
  }
}

export type SessionState =
  | "launching"
  | "running"
  | "warned"
  | "expiring"
  | "ended";

export interface SessionInfo {
  session_id: string;
  entry_id: string;
  label: string;
  state: SessionState;
  started_at: string;
  deadline: string | null;
  time_remaining: Duration | null;
  warnings_issued: number[];
  /** Whether the HUD's "X" button confirms before ending this activity (issue #78). */
  confirm_on_close: boolean;
}

export interface EntryView {
  entry_id: string;
  label: string;
  icon_ref: string | null;
  kind_tag: EntryKindTag;
  enabled: boolean;
  reasons: ReasonCode[];
  max_run_if_started_now: Duration | null;
}

// `LaunchResponse` is now exported from `./client` — it's a UI-friendly
// normalisation of the on-wire `LaunchOutcome` shape.

export interface DailyOverride {
  /** A limit subject: a bare entry ID, or `group:<id>` for a whole category. */
  subject: string;
  date: string;
  availability: boolean | null;
  quota_delta_seconds: number | null;
  created_at: string;
  updated_at: string;
}

export interface UsageStat {
  entry_id: string;
  label: string;
  date: string;
  duration_seconds: number;
}

export interface VolumeRestrictions {
  max_volume: number | null;
  min_volume: number | null;
  allow_mute: boolean;
  allow_change: boolean;
}

export interface VolumeInfo {
  percent: number;
  muted: boolean;
  available: boolean;
  backend: string | null;
  restrictions: VolumeRestrictions;
}

export interface BrightnessRestrictions {
  max_brightness: number | null;
  min_brightness: number | null;
  allow_change: boolean;
}

export interface BrightnessInfo {
  percent: number;
  available: boolean;
  backend: string | null;
  device: string | null;
  restrictions: BrightnessRestrictions;
  auto_available: boolean;
  auto_enabled: boolean;
}

export interface HealthStatus {
  live: boolean;
  ready: boolean;
  policy_loaded: boolean;
  host_adapter_ok: boolean;
  store_ok: boolean;
}

export interface InternetStatusView {
  target: string;
  available: boolean;
}

export interface ServiceStateSnapshot {
  api_version: number;
  policy_loaded: boolean;
  current_session: SessionInfo | null;
  entry_count: number;
  entries: EntryView[];
  internet_status: InternetStatusView[];
}

export interface WindowInfo {
  id: number;
  name: string | null;
  app_id: string | null;
  window_class: string | null;
  pid: number | null;
  workspace: string | null;
  in_scratchpad: boolean;
  visible: boolean;
  focused: boolean;
}

export interface WindowsResponse {
  windows: WindowInfo[];
}
