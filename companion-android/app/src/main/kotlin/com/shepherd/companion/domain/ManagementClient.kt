package com.shepherd.companion.domain

import com.shepherd.companion.ble.ShepherdConnection
import com.shepherd.companion.ble.ShepherdJson
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/**
 * Typed façade over [ShepherdConnection], mirroring the device's
 * `ManagementService` trait plus the claim-flow methods. Each call
 * serialises params, sends the RPC, and decodes the result; errors
 * surface as [com.shepherd.companion.ble.RpcException].
 *
 * Method-name strings and param keys are the stable wire contract from
 * `crates/shepherd-ble/src/rpc.rs`.
 */
class ManagementClient(private val connection: ShepherdConnection) {

    // --- claim flow ----------------------------------------------------

    suspend fun claim(deviceName: String): AdminRecord =
        decode(call("claim", buildJsonObject { put("device_name", JsonPrimitive(deviceName)) }))

    suspend fun factoryReset() {
        call("factory_reset", JsonObject(emptyMap()))
    }

    // --- health / state ------------------------------------------------

    suspend fun health(): HealthStatus = decode(call("health", empty()))

    suspend fun serviceState(): ServiceStateSnapshot = decode(call("service_state", empty()))

    // --- entries -------------------------------------------------------

    suspend fun listEntries(at: IsoTimestamp? = null): List<EntryView> =
        decode(call("list_entries", buildJsonObject { put("at", at.toJson()) }))

    suspend fun getEntry(id: String, at: IsoTimestamp? = null): EntryView =
        decode(call("get_entry", buildJsonObject {
            put("id", JsonPrimitive(id))
            put("at", at.toJson())
        }))

    // --- sessions ------------------------------------------------------

    suspend fun currentSession(): SessionInfo? = decodeNullable(call("current_session", empty()))

    suspend fun launch(id: String): LaunchOutcome =
        decode(call("launch", buildJsonObject { put("id", JsonPrimitive(id)) }))

    suspend fun stopCurrent(mode: StopMode = StopMode.GRACEFUL) {
        call("stop_current", buildJsonObject { put("mode", ShepherdJson.encodeToJsonElement(StopMode.serializer(), mode)) })
    }

    suspend fun extendCurrent(seconds: Long): ExtendResult =
        decode(call("extend_current", buildJsonObject { put("seconds", JsonPrimitive(seconds)) }))

    // --- overrides -----------------------------------------------------

    suspend fun listOverrides(date: IsoDate? = null): List<DailyOverride> =
        decode(call("list_overrides", buildJsonObject { put("date", date.toJson()) }))

    suspend fun getOverride(id: String, date: IsoDate? = null): DailyOverride? =
        decodeNullable(call("get_override", buildJsonObject {
            put("id", JsonPrimitive(id))
            put("date", date.toJson())
        }))

    suspend fun upsertOverride(
        id: String,
        date: IsoDate? = null,
        availability: Boolean? = null,
        quotaDeltaSeconds: Long? = null,
    ): DailyOverride =
        decode(call("upsert_override", buildJsonObject {
            put("id", JsonPrimitive(id))
            put("date", date.toJson())
            put("availability", availability?.let(::JsonPrimitive) ?: JsonNull)
            put("quota_delta_seconds", quotaDeltaSeconds?.let(::JsonPrimitive) ?: JsonNull)
        }))

    suspend fun deleteOverride(id: String, date: IsoDate? = null): DeleteResult =
        decode(call("delete_override", buildJsonObject {
            put("id", JsonPrimitive(id))
            put("date", date.toJson())
        }))

    // --- usage ---------------------------------------------------------

    suspend fun usageAll(from: IsoDate, to: IsoDate): List<UsageStat> =
        decode(call("usage_all", buildJsonObject {
            put("from", JsonPrimitive(from))
            put("to", JsonPrimitive(to))
        }))

    suspend fun usageEntry(id: String, from: IsoDate, to: IsoDate): List<UsageStat> =
        decode(call("usage_entry", buildJsonObject {
            put("id", JsonPrimitive(id))
            put("from", JsonPrimitive(from))
            put("to", JsonPrimitive(to))
        }))

    // --- volume / brightness ------------------------------------------

    suspend fun getVolume(): VolumeInfo = decode(call("get_volume", empty()))

    suspend fun setVolume(percent: Int): VolumeInfo =
        decode(call("set_volume", buildJsonObject { put("percent", JsonPrimitive(percent)) }))

    suspend fun setMute(muted: Boolean): VolumeInfo =
        decode(call("set_mute", buildJsonObject { put("muted", JsonPrimitive(muted)) }))

    suspend fun getBrightness(): BrightnessInfo = decode(call("get_brightness", empty()))

    suspend fun setBrightness(percent: Int): BrightnessInfo =
        decode(call("set_brightness", buildJsonObject { put("percent", JsonPrimitive(percent)) }))

    // --- misc ----------------------------------------------------------

    suspend fun reloadConfig(): ReloadResult = decode(call("reload_config", empty()))

    suspend fun logout() {
        call("logout", empty())
    }

    suspend fun listWindows(): List<WindowInfo> = decode(call("list_windows", empty()))

    suspend fun actOnWindow(id: Long, action: WindowAction) {
        call("act_on_window", buildJsonObject {
            put("id", JsonPrimitive(id))
            put("action", ShepherdJson.encodeToJsonElement(WindowAction.serializer(), action))
        })
    }

    // --- plumbing ------------------------------------------------------

    private suspend fun call(method: String, params: JsonElement): JsonElement =
        connection.call(method, params)

    private fun empty(): JsonElement = JsonObject(emptyMap())

    private fun IsoTimestamp?.toJson(): JsonElement =
        this?.let(::JsonPrimitive) ?: JsonNull

    private inline fun <reified T> decode(element: JsonElement): T =
        ShepherdJson.decodeFromJsonElement(kotlinx.serialization.serializer(), element)

    private inline fun <reified T> decodeNullable(element: JsonElement): T? =
        if (element is JsonNull) null else decode(element)
}
