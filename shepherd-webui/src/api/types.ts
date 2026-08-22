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
  | { code: "protection_unavailable" }
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

export type SessionState =
  | "launching"
  | "running"
  | "warned"
  | "expiring"
  /** Teardown requested; the activity is still up until the host confirms. */
  | "stopping"
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

/**
 * A token gate's current state (issue #8). Banked time is a currency: source
 * activities earn it, the gated activity spends it.
 */
export interface TokenStatus {
  balance: Duration;
  minimum: Duration;
  /**
   * Whether the gate is open. Not simply `balance >= minimum` — once open it
   * stays open until the balance is spent to zero.
   */
  unlocked: boolean;
  max_balance: Duration | null;
  carry_over: boolean;
}

export interface EntryView {
  entry_id: string;
  label: string;
  icon_ref: string | null;
  kind_tag: EntryKindTag;
  enabled: boolean;
  /** Category this activity shares a schedule and budget with (issue #5). */
  group?: string | null;
  reasons: ReasonCode[];
  /** This activity's own token gate (issue #8), if it has one. */
  tokens?: TokenStatus | null;
  max_run_if_started_now: Duration | null;
}

/** A category of activities sharing one schedule and one combined budget. */
export interface GroupView {
  group_id: string;
  label: string;
  member_ids: string[];
  enabled: boolean;
  reasons: ReasonCode[];
  used_today: Duration;
  daily_quota: Duration | null;
  max_run_if_started_now: Duration | null;
  /** The category's token gate (issue #8), shared by every member. */
  tokens?: TokenStatus | null;
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

export type AudioOutputKind =
  | "speakers"
  | "headphones"
  | "hdmi"
  | "digital"
  | "line_out"
  | "bluetooth"
  | "unknown";

/**
 * The audio output a volume reading applies to. `key` is stable across reboots
 * (`<device.name>:output:<route.name>`); `description` is for display only.
 */
export interface AudioOutput {
  key: string;
  description: string;
  kind: AudioOutputKind;
}

/**
 * An audio output the device has seen, with any per-output limit set for it.
 * Rows appear by discovery — plug the device in and it shows up — so nobody has
 * to work out how a device identifies itself.
 */
export interface AudioOutputRecord {
  output: AudioOutput;
  max_volume: number | null;
  min_volume: number | null;
  last_seen: string;
  active: boolean;
}

export interface VolumeInfo {
  percent: number;
  muted: boolean;
  available: boolean;
  backend: string | null;
  restrictions: VolumeRestrictions;
  output: AudioOutput | null;
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

/**
 * An administrator-facing condition that is currently true of the device
 * (issue #143) — a missing dependency, a protection that is not in effect.
 *
 * Distinct from the time-limit warnings shown to the child: those are about
 * their session, these are about the device being misconfigured or missing
 * something. A diagnostic is state, not an event — it is raised while the
 * condition holds and disappears when it stops.
 */
export type DiagnosticCode =
  | "firewall_unenforceable"
  | "firewall_not_applied"
  | "browser_policy_ignored"
  | "yt_dlp_missing"
  | "media_cache_disk_low"
  | "media_library_unreadable"
  | "no_sound_backend"
  | "input_devices_unavailable"
  | "ble_pairing_agent_unavailable";

/** Critical means the config promises a protection the device is not providing. */
export type DiagnosticSeverity = "critical" | "warning" | "info";

/** What a diagnostic is about: the device, or one configured activity. */
export type DiagnosticSubject =
  | { type: "service" }
  | { type: "entry"; entry_id: string };

export interface Diagnostic {
  code: DiagnosticCode;
  subject: DiagnosticSubject;
  severity: DiagnosticSeverity;
  message: string;
  /** What to do about it, when there is a concrete answer. */
  remedy: string | null;
  /** When the condition started — not when it was last checked. */
  since: string;
}

export interface DiagnosticSet {
  /** Sorted most severe first, then by subject, then by code. */
  items: Diagnostic[];
  /**
   * Whether the daemon's cap hid anything. Must be surfaced: showing part of
   * the problems while implying it is all of them is worse than showing none.
   */
  truncated: boolean;
}

/** The activity a diagnostic concerns, or null if it concerns the device. */
export function diagnosticEntryId(d: Diagnostic): string | null {
  return d.subject.type === "entry" ? d.subject.entry_id : null;
}

export interface ServiceStateSnapshot {
  api_version: number;
  policy_loaded: boolean;
  current_session: SessionInfo | null;
  entry_count: number;
  entries: EntryView[];
  internet_status: InternetStatusView[];
  diagnostics: DiagnosticSet;
}

/**
 * What shepherd is supervising behind a window.
 *
 * The compositor only knows pids; shepherdd matches them against the
 * processes it actually spawned. `escaped` and `unowned` are the two that
 * mean "nothing is watching this" — an activity that outlived its teardown,
 * and a surface that belongs to no session at all.
 */
export type WindowOwner = "shepherd" | "activity" | "escaped" | "unowned";

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
  owner: WindowOwner;
}

