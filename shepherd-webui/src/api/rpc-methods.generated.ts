// GENERATED FILE — DO NOT EDIT BY HAND
//
// Run `cargo run -p shepherd-wire-codegen --bin rpc-codegen`
// after changing the `ManagementService` trait in
// `crates/shepherd-management/src/service.rs`.

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
  | "get_hud_scale"
  | "get_display_state"
  | "set_display_mode"
  | "ping"
  | "reload_config"
  | "logout"
  | "list_diagnostics"
  | "list_windows"
  | "act_on_window";

/** Wrap-field lookup for methods whose wire result is `{"<field>": <value>}`. */
export const RPC_WRAP_FIELDS: Partial<Record<RpcMethod, string>> = {
  "extend_current": "new_deadline",
  "delete_override": "deleted",
  "reload_config": "entry_count",
};
