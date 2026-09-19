// GENERATED FILE — DO NOT EDIT BY HAND
//
// Run `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`
// after changing the `ManagementService` trait in
// `crates/lunchbox-management/src/service.rs`.

package com.lunchboxos.companion.ble

/**
 * Wire-name constants for every RPC exposed by the Lunchbox device.
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
    const val GET_HUD_ORIENTATION: String = "get_hud_orientation"
    const val GET_DISPLAY_STATE: String = "get_display_state"
    const val SET_DISPLAY_MODE: String = "set_display_mode"
    const val PING: String = "ping"
    const val RELOAD_CONFIG: String = "reload_config"
    const val REFRESH_MEDIA: String = "refresh_media"
    const val WEB_AUTH_STATUS: String = "web_auth_status"
    const val SET_WEB_PASSWORD: String = "set_web_password"
    const val LIST_WEB_SESSIONS: String = "list_web_sessions"
    const val REVOKE_WEB_SESSION: String = "revoke_web_session"
    const val LIST_LOGIN_REQUESTS: String = "list_login_requests"
    const val APPROVE_LOGIN_REQUEST: String = "approve_login_request"
    const val DENY_LOGIN_REQUEST: String = "deny_login_request"
    const val LIST_ADMINS: String = "list_admins"
    const val REVOKE_ADMIN: String = "revoke_admin"
    const val LIST_ENROLMENT_REQUESTS: String = "list_enrolment_requests"
    const val APPROVE_ENROLMENT_REQUEST: String = "approve_enrolment_request"
    const val DENY_ENROLMENT_REQUEST: String = "deny_enrolment_request"
    const val LOGOUT: String = "logout"
    const val LIST_DIAGNOSTICS: String = "list_diagnostics"
    const val NETWORK_STATUS: String = "network_status"
    const val ENTER_ADMIN_MODE: String = "enter_admin_mode"
    const val EXIT_ADMIN_MODE: String = "exit_admin_mode"
    const val ADMIN_IDLE_TIMEOUT: String = "admin_idle_timeout"
    const val LOCK_DEVICE: String = "lock_device"
    const val UNLOCK_DEVICE: String = "unlock_device"
    const val LIST_DESKTOP_APPS: String = "list_desktop_apps"
    const val LAUNCH_DESKTOP_APP: String = "launch_desktop_app"
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
