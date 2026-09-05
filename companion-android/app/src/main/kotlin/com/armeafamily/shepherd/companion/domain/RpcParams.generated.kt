// GENERATED FILE — DO NOT EDIT BY HAND
//
// Run `cargo run -p shepherd-wire-codegen --bin rpc-codegen`
// after changing the `ManagementService` trait in
// `crates/shepherd-management/src/service.rs`.

package com.armeafamily.shepherd.companion.domain

import com.armeafamily.shepherd.companion.ble.ShepherdJson
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/**
 * The params object for every RPC the device speaks, built from the
 * `ManagementService` trait's own signatures.
 *
 * `ManagementClient` used to spell these keys as string literals, which
 * left a renamed parameter compiling on both sides and failing at run
 * time — the same gap that let the hand-written payload mirrors drift
 * twice before they were generated.
 *
 * A parameter the caller may omit is sent explicitly as `null`, which the
 * daemon reads the same way as an absent key.
 */
object RpcParams {
    /** Params for `health`, which takes none. */
    fun health(): JsonObject = JsonObject(emptyMap())

    /** Params for `service_state`, which takes none. */
    fun serviceState(): JsonObject = JsonObject(emptyMap())

    /** Params for `list_entries`. */
    fun listEntries(at: IsoTimestamp? = null): JsonObject = buildJsonObject {
        put("at", at?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `get_entry`. */
    fun getEntry(id: EntryId, at: IsoTimestamp? = null): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
        put("at", at?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `list_groups`. */
    fun listGroups(at: IsoTimestamp? = null): JsonObject = buildJsonObject {
        put("at", at?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `current_session`, which takes none. */
    fun currentSession(): JsonObject = JsonObject(emptyMap())

    /** Params for `launch`. */
    fun launch(id: EntryId): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
    }

    /** Params for `stop_current`. */
    fun stopCurrent(mode: StopMode? = null): JsonObject = buildJsonObject {
        put("mode", mode?.let { ShepherdJson.encodeToJsonElement(StopMode.serializer(), it) } ?: JsonNull)
    }

    /** Params for `reset_current`, which takes none. */
    fun resetCurrent(): JsonObject = JsonObject(emptyMap())

    /** Params for `extend_current`. */
    fun extendCurrent(seconds: Long): JsonObject = buildJsonObject {
        put("seconds", JsonPrimitive(seconds))
    }

    /** Params for `list_overrides`. */
    fun listOverrides(date: IsoDate? = null): JsonObject = buildJsonObject {
        put("date", date?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `get_override`. */
    fun getOverride(id: LimitSubject, date: IsoDate? = null): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
        put("date", date?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `upsert_override`. */
    fun upsertOverride(id: LimitSubject, date: IsoDate? = null, availability: Boolean? = null, quotaDeltaSeconds: Long? = null): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
        put("date", date?.let { JsonPrimitive(it) } ?: JsonNull)
        put("availability", availability?.let { JsonPrimitive(it) } ?: JsonNull)
        put("quota_delta_seconds", quotaDeltaSeconds?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `delete_override`. */
    fun deleteOverride(id: LimitSubject, date: IsoDate? = null): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
        put("date", date?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `adjust_tokens`. */
    fun adjustTokens(id: LimitSubject, deltaSeconds: Long): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
        put("delta_seconds", JsonPrimitive(deltaSeconds))
    }

    /** Params for `usage_all`. */
    fun usageAll(from: IsoDate? = null, to: IsoDate? = null): JsonObject = buildJsonObject {
        put("from", from?.let { JsonPrimitive(it) } ?: JsonNull)
        put("to", to?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `usage_entry`. */
    fun usageEntry(id: EntryId, from: IsoDate? = null, to: IsoDate? = null): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
        put("from", from?.let { JsonPrimitive(it) } ?: JsonNull)
        put("to", to?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `get_volume`, which takes none. */
    fun getVolume(): JsonObject = JsonObject(emptyMap())

    /** Params for `set_volume`. */
    fun setVolume(percent: Int): JsonObject = buildJsonObject {
        put("percent", JsonPrimitive(percent))
    }

    /** Params for `set_mute`. */
    fun setMute(muted: Boolean): JsonObject = buildJsonObject {
        put("muted", JsonPrimitive(muted))
    }

    /** Params for `volume_up`. */
    fun volumeUp(step: Int): JsonObject = buildJsonObject {
        put("step", JsonPrimitive(step))
    }

    /** Params for `volume_down`. */
    fun volumeDown(step: Int): JsonObject = buildJsonObject {
        put("step", JsonPrimitive(step))
    }

    /** Params for `toggle_mute`, which takes none. */
    fun toggleMute(): JsonObject = JsonObject(emptyMap())

    /** Params for `list_audio_outputs`, which takes none. */
    fun listAudioOutputs(): JsonObject = JsonObject(emptyMap())

    /** Params for `set_audio_output_limits`. */
    fun setAudioOutputLimits(outputKey: String, maxVolume: Int? = null, minVolume: Int? = null): JsonObject = buildJsonObject {
        put("output_key", JsonPrimitive(outputKey))
        put("max_volume", maxVolume?.let { JsonPrimitive(it) } ?: JsonNull)
        put("min_volume", minVolume?.let { JsonPrimitive(it) } ?: JsonNull)
    }

    /** Params for `forget_audio_output`. */
    fun forgetAudioOutput(outputKey: String): JsonObject = buildJsonObject {
        put("output_key", JsonPrimitive(outputKey))
    }

    /** Params for `select_audio_output`. */
    fun selectAudioOutput(outputKey: String): JsonObject = buildJsonObject {
        put("output_key", JsonPrimitive(outputKey))
    }

    /** Params for `get_brightness`, which takes none. */
    fun getBrightness(): JsonObject = JsonObject(emptyMap())

    /** Params for `set_brightness`. */
    fun setBrightness(percent: Int): JsonObject = buildJsonObject {
        put("percent", JsonPrimitive(percent))
    }

    /** Params for `brightness_up`. */
    fun brightnessUp(step: Int): JsonObject = buildJsonObject {
        put("step", JsonPrimitive(step))
    }

    /** Params for `brightness_down`. */
    fun brightnessDown(step: Int): JsonObject = buildJsonObject {
        put("step", JsonPrimitive(step))
    }

    /** Params for `set_auto_brightness`. */
    fun setAutoBrightness(enabled: Boolean): JsonObject = buildJsonObject {
        put("enabled", JsonPrimitive(enabled))
    }

    /** Params for `toggle_auto_brightness`, which takes none. */
    fun toggleAutoBrightness(): JsonObject = JsonObject(emptyMap())

    /** Params for `set_screen_power`. */
    fun setScreenPower(on: Boolean): JsonObject = buildJsonObject {
        put("on", JsonPrimitive(on))
    }

    /** Params for `get_hud_scale`, which takes none. */
    fun getHudScale(): JsonObject = JsonObject(emptyMap())

    /** Params for `get_display_state`, which takes none. */
    fun getDisplayState(): JsonObject = JsonObject(emptyMap())

    /** Params for `set_display_mode`. */
    fun setDisplayMode(mode: DisplayMode): JsonObject = buildJsonObject {
        put("mode", ShepherdJson.encodeToJsonElement(DisplayMode.serializer(), mode))
    }

    /** Params for `ping`, which takes none. */
    fun ping(): JsonObject = JsonObject(emptyMap())

    /** Params for `reload_config`, which takes none. */
    fun reloadConfig(): JsonObject = JsonObject(emptyMap())

    /** Params for `refresh_media`, which takes none. */
    fun refreshMedia(): JsonObject = JsonObject(emptyMap())

    /** Params for `logout`, which takes none. */
    fun logout(): JsonObject = JsonObject(emptyMap())

    /** Params for `list_diagnostics`, which takes none. */
    fun listDiagnostics(): JsonObject = JsonObject(emptyMap())

    /** Params for `list_windows`, which takes none. */
    fun listWindows(): JsonObject = JsonObject(emptyMap())

    /** Params for `act_on_window`. */
    fun actOnWindow(id: Long, action: WindowAction): JsonObject = buildJsonObject {
        put("id", JsonPrimitive(id))
        put("action", ShepherdJson.encodeToJsonElement(WindowAction.serializer(), action))
    }
}
