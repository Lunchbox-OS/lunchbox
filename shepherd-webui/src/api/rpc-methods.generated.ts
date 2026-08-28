// GENERATED FILE — DO NOT EDIT BY HAND
//
// Run `cargo run -p shepherd-wire-codegen --bin rpc-codegen`
// after changing the `ManagementService` trait in
// `crates/shepherd-management/src/service.rs`.

import type {
  AudioOutputRecord,
  BrightnessInfo,
  DailyOverride,
  DesktopApp,
  DiagnosticSet,
  DisplayMode,
  DisplayState,
  EntryId,
  EntryView,
  GroupView,
  HealthStatus,
  HudOrientation,
  IsoDate,
  IsoTimestamp,
  LaunchOutcome,
  LimitSubject,
  LoginRequestInfo,
  NetworkStatusView,
  ServiceStateSnapshot,
  SessionInfo,
  StopMode,
  TokenStatus,
  UsageStat,
  VolumeInfo,
  WebAuthStatus,
  WebSessionInfo,
  WindowAction,
  WindowInfo,
} from "./wire-types.generated";

/**
 * Every RPC method the shepherd device speaks. The web-ui client is
 * REST-shaped and doesn't dispatch by name, but references such as
 * feature-flag names or telemetry event names benefit from a compile-time
 * check that the string matches a real RPC.
 */
export type RpcMethod =
  | "health"
  | "service_state"
  | "list_entries"
  | "get_entry"
  | "list_groups"
  | "current_session"
  | "launch"
  | "stop_current"
  | "reset_current"
  | "extend_current"
  | "list_overrides"
  | "get_override"
  | "upsert_override"
  | "delete_override"
  | "adjust_tokens"
  | "usage_all"
  | "usage_entry"
  | "get_volume"
  | "set_volume"
  | "set_mute"
  | "volume_up"
  | "volume_down"
  | "toggle_mute"
  | "list_audio_outputs"
  | "set_audio_output_limits"
  | "forget_audio_output"
  | "select_audio_output"
  | "get_brightness"
  | "set_brightness"
  | "brightness_up"
  | "brightness_down"
  | "set_auto_brightness"
  | "toggle_auto_brightness"
  | "set_screen_power"
  | "get_hud_scale"
  | "get_hud_orientation"
  | "get_display_state"
  | "set_display_mode"
  | "ping"
  | "reload_config"
  | "refresh_media"
  | "web_auth_status"
  | "set_web_password"
  | "list_web_sessions"
  | "revoke_web_session"
  | "list_login_requests"
  | "approve_login_request"
  | "deny_login_request"
  | "logout"
  | "list_diagnostics"
  | "network_status"
  | "enter_admin_mode"
  | "exit_admin_mode"
  | "admin_idle_timeout"
  | "list_desktop_apps"
  | "launch_desktop_app"
  | "list_windows"
  | "act_on_window";

/** Wrap-field lookup for methods whose wire result is `{"<field>": <value>}`. */
export const RPC_WRAP_FIELDS: Partial<Record<RpcMethod, string>> = {
  "extend_current": "new_deadline",
  "delete_override": "deleted",
  "reload_config": "entry_count",
};

/**
 * The params object each method takes.
 *
 * Keys are the wire form (snake_case), because that is what the daemon
 * deserializes into the trait method's arguments. An optional key may be
 * left out entirely; `JSON.stringify` drops an `undefined` value, which
 * the daemon reads the same way as an absent one.
 */
export interface RpcParamsMap {
  "health": Record<string, never>;
  "service_state": Record<string, never>;
  "list_entries": {
    at?: IsoTimestamp;
  };
  "get_entry": {
    id: EntryId;
    at?: IsoTimestamp;
  };
  "list_groups": {
    at?: IsoTimestamp;
  };
  "current_session": Record<string, never>;
  "launch": {
    id: EntryId;
  };
  "stop_current": {
    mode?: StopMode;
  };
  "reset_current": Record<string, never>;
  "extend_current": {
    seconds: number;
  };
  "list_overrides": {
    date?: IsoDate;
  };
  "get_override": {
    id: LimitSubject;
    date?: IsoDate;
  };
  "upsert_override": {
    id: LimitSubject;
    date?: IsoDate;
    availability: boolean | null;
    quota_delta_seconds: number | null;
  };
  "delete_override": {
    id: LimitSubject;
    date?: IsoDate;
  };
  "adjust_tokens": {
    id: LimitSubject;
    delta_seconds: number;
  };
  "usage_all": {
    from?: IsoDate;
    to?: IsoDate;
  };
  "usage_entry": {
    id: EntryId;
    from?: IsoDate;
    to?: IsoDate;
  };
  "get_volume": Record<string, never>;
  "set_volume": {
    percent: number;
  };
  "set_mute": {
    muted: boolean;
  };
  "volume_up": {
    step: number;
  };
  "volume_down": {
    step: number;
  };
  "toggle_mute": Record<string, never>;
  "list_audio_outputs": Record<string, never>;
  "set_audio_output_limits": {
    output_key: string;
    max_volume?: number | null;
    min_volume?: number | null;
  };
  "forget_audio_output": {
    output_key: string;
  };
  "select_audio_output": {
    output_key: string;
  };
  "get_brightness": Record<string, never>;
  "set_brightness": {
    percent: number;
  };
  "brightness_up": {
    step: number;
  };
  "brightness_down": {
    step: number;
  };
  "set_auto_brightness": {
    enabled: boolean;
  };
  "toggle_auto_brightness": Record<string, never>;
  "set_screen_power": {
    on: boolean;
  };
  "get_hud_scale": Record<string, never>;
  "get_hud_orientation": Record<string, never>;
  "get_display_state": Record<string, never>;
  "set_display_mode": {
    mode: DisplayMode;
  };
  "ping": Record<string, never>;
  "reload_config": Record<string, never>;
  "refresh_media": Record<string, never>;
  "web_auth_status": Record<string, never>;
  "set_web_password": {
    password: string;
  };
  "list_web_sessions": Record<string, never>;
  "revoke_web_session": {
    id: string;
  };
  "list_login_requests": Record<string, never>;
  "approve_login_request": {
    id: string;
  };
  "deny_login_request": {
    id: string;
  };
  "logout": Record<string, never>;
  "list_diagnostics": Record<string, never>;
  "network_status": Record<string, never>;
  "enter_admin_mode": Record<string, never>;
  "exit_admin_mode": Record<string, never>;
  "admin_idle_timeout": Record<string, never>;
  "list_desktop_apps": Record<string, never>;
  "launch_desktop_app": {
    id: string;
  };
  "list_windows": Record<string, never>;
  "act_on_window": {
    id: number;
    action: WindowAction;
  };
}

export type RpcParams<M extends RpcMethod> = RpcParamsMap[M];

/**
 * What each method answers with, as it arrives on the wire.
 *
 * Methods carrying a `RPC_WRAP_FIELDS` entry are typed as the wrapping
 * object rather than the value inside it, so the type matches the bytes
 * and the unwrap stays visible at the call site.
 */
export interface RpcResultMap {
  "health": HealthStatus;
  "service_state": ServiceStateSnapshot;
  "list_entries": EntryView[];
  "get_entry": EntryView;
  "list_groups": GroupView[];
  "current_session": SessionInfo | null;
  "launch": LaunchOutcome;
  "stop_current": null;
  "reset_current": null;
  "extend_current": { new_deadline: IsoTimestamp | null };
  "list_overrides": DailyOverride[];
  "get_override": DailyOverride | null;
  "upsert_override": DailyOverride;
  "delete_override": { deleted: boolean };
  "adjust_tokens": TokenStatus;
  "usage_all": UsageStat[];
  "usage_entry": UsageStat[];
  "get_volume": VolumeInfo;
  "set_volume": VolumeInfo;
  "set_mute": VolumeInfo;
  "volume_up": VolumeInfo;
  "volume_down": VolumeInfo;
  "toggle_mute": VolumeInfo;
  "list_audio_outputs": AudioOutputRecord[];
  "set_audio_output_limits": AudioOutputRecord;
  "forget_audio_output": boolean;
  "select_audio_output": VolumeInfo;
  "get_brightness": BrightnessInfo;
  "set_brightness": BrightnessInfo;
  "brightness_up": BrightnessInfo;
  "brightness_down": BrightnessInfo;
  "set_auto_brightness": BrightnessInfo;
  "toggle_auto_brightness": BrightnessInfo;
  "set_screen_power": boolean;
  "get_hud_scale": number;
  "get_hud_orientation": HudOrientation;
  "get_display_state": DisplayState;
  "set_display_mode": DisplayState;
  "ping": null;
  "reload_config": { entry_count: number };
  "refresh_media": null;
  "web_auth_status": WebAuthStatus;
  "set_web_password": null;
  "list_web_sessions": WebSessionInfo[];
  "revoke_web_session": null;
  "list_login_requests": LoginRequestInfo[];
  "approve_login_request": null;
  "deny_login_request": null;
  "logout": null;
  "list_diagnostics": DiagnosticSet;
  "network_status": NetworkStatusView;
  "enter_admin_mode": null;
  "exit_admin_mode": null;
  "admin_idle_timeout": boolean;
  "list_desktop_apps": DesktopApp[];
  "launch_desktop_app": null;
  "list_windows": WindowInfo[];
  "act_on_window": null;
}

export type RpcResult<M extends RpcMethod> = RpcResultMap[M];
