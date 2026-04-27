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
  | { code: "disabled"; reason: string | null }
  | { code: "internet_unavailable"; check: string | null }
  | { code: "manually_disabled"; until: string };

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
    case "disabled":
      return r.reason ?? "Disabled";
    case "internet_unavailable":
      return "No internet connection";
    case "manually_disabled":
      return "Disabled for today";
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

export type LaunchResponse =
  | { result: "approved"; session_id: string; deadline: string | null }
  | { result: "denied"; reasons: ReasonCode[] };

export interface DailyOverride {
  entry_id: string;
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

export interface MaintenanceState {
  active: boolean;
  activated_at: string | null;
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

export interface HealthStatus {
  live: boolean;
  ready: boolean;
  policy_loaded: boolean;
  host_adapter_ok: boolean;
  store_ok: boolean;
}

export interface ServiceStateSnapshot {
  api_version: number;
  policy_loaded: boolean;
  current_session: SessionInfo | null;
  entry_count: number;
  entries: EntryView[];
}
