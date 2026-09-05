// GENERATED FILE — DO NOT EDIT BY HAND
//
// Run `cargo run -p shepherd-wire-codegen --bin rpc-codegen`
// after changing the `ManagementService` trait in
// `crates/shepherd-management/src/service.rs`.

package com.armeafamily.shepherd.companion.ble

/**
 * Wire-name constants for every RPC exposed by the shepherd device.
 * Mirrors the trait annotated with `#[management_rpc]` on the Rust side,
 * generated from that trait's `RPC_SCHEMA_JSON` so a drift between the
 * two sides is a CI failure, not a silent runtime miss.
 */
object RpcMethods {
    const val HEALTH: String = "health"
    const val SERVICE_STATE: String = "service_state"
    const val LIST_ENTRIES: String = "list_entries"
    const val GET_ENTRY: String = "get_entry"
    const val LIST_GROUPS: String = "list_groups"
    const val CURRENT_SESSION: String = "current_session"
    const val LAUNCH: String = "launch"
    const val STOP_CURRENT: String = "stop_current"
    const val RESET_CURRENT: String = "reset_current"
    const val EXTEND_CURRENT: String = "extend_current"
    const val LIST_OVERRIDES: String = "list_overrides"
    const val GET_OVERRIDE: String = "get_override"
    const val UPSERT_OVERRIDE: String = "upsert_override"
    const val DELETE_OVERRIDE: String = "delete_override"
    const val ADJUST_TOKENS: String = "adjust_tokens"
    const val USAGE_ALL: String = "usage_all"
    const val USAGE_ENTRY: String = "usage_entry"
    const val GET_VOLUME: String = "get_volume"
    const val SET_VOLUME: String = "set_volume"
    const val SET_MUTE: String = "set_mute"
    const val VOLUME_UP: String = "volume_up"
    const val VOLUME_DOWN: String = "volume_down"
    const val TOGGLE_MUTE: String = "toggle_mute"
    const val LIST_AUDIO_OUTPUTS: String = "list_audio_outputs"
    const val SET_AUDIO_OUTPUT_LIMITS: String = "set_audio_output_limits"
    const val FORGET_AUDIO_OUTPUT: String = "forget_audio_output"
    const val SELECT_AUDIO_OUTPUT: String = "select_audio_output"
    const val GET_BRIGHTNESS: String = "get_brightness"
    const val SET_BRIGHTNESS: String = "set_brightness"
    const val BRIGHTNESS_UP: String = "brightness_up"
    const val BRIGHTNESS_DOWN: String = "brightness_down"
    const val SET_AUTO_BRIGHTNESS: String = "set_auto_brightness"
    const val TOGGLE_AUTO_BRIGHTNESS: String = "toggle_auto_brightness"
    const val SET_SCREEN_POWER: String = "set_screen_power"
    const val GET_HUD_SCALE: String = "get_hud_scale"
    const val GET_DISPLAY_STATE: String = "get_display_state"
    const val SET_DISPLAY_MODE: String = "set_display_mode"
    const val PING: String = "ping"
    const val RELOAD_CONFIG: String = "reload_config"
    const val REFRESH_MEDIA: String = "refresh_media"
    const val LOGOUT: String = "logout"
    const val LIST_DIAGNOSTICS: String = "list_diagnostics"
    const val LIST_WINDOWS: String = "list_windows"
    const val ACT_ON_WINDOW: String = "act_on_window"

    /**
     * For methods whose result on the wire is `{"<field>": <value>}`
     * (via `#[rpc(wrap_result = "<field>")]`), the field name to unwrap.
     * `null` for methods whose result is a bare value or a full object.
     */
    fun wrapField(method: String): String? = when (method) {
        "extend_current" -> "new_deadline"
        "delete_override" -> "deleted"
        "reload_config" -> "entry_count"
        else -> null
    }
}
