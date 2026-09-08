package com.armeafamily.shepherd.companion.domain

import com.armeafamily.shepherd.companion.ble.ShepherdConnection
import com.armeafamily.shepherd.companion.ble.ShepherdJson
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/**
 * Typed façade over [ShepherdConnection], mirroring the device's
 * `ManagementService` trait plus the claim-flow methods. Each call
 * serialises params, sends the RPC, and decodes the result; errors
 * surface as [com.armeafamily.shepherd.companion.ble.RpcException].
 *
 * Params come from [RpcParams], generated from the trait's own signatures,
 * so a renamed parameter fails the build here rather than at run time. Only
 * the claim-flow methods below still build their own: they live in
 * `crates/shepherd-ble/src/rpc.rs` rather than on `ManagementService`, so the
 * RPC schema does not describe them.
 */
class ManagementClient(private val connection: ShepherdConnection) {

    // --- claim flow ----------------------------------------------------

    suspend fun claim(deviceName: String): AdminRecord =
        decode(call("claim", buildJsonObject { put("device_name", JsonPrimitive(deviceName)) }))

    suspend fun factoryReset() {
        call("factory_reset", JsonObject(emptyMap()))
    }

    // --- health / state ------------------------------------------------

    suspend fun health(): HealthStatus = decode(call("health", RpcParams.health()))

    suspend fun serviceState(): ServiceStateSnapshot =
        decode(call("service_state", RpcParams.serviceState()))

    // --- entries -------------------------------------------------------

    suspend fun listEntries(at: IsoTimestamp? = null): List<EntryView> =
        decode(call("list_entries", RpcParams.listEntries(at)))

    suspend fun getEntry(id: String, at: IsoTimestamp? = null): EntryView =
        decode(call("get_entry", RpcParams.getEntry(id, at)))

    // --- groups (issue #5) ---------------------------------------------

    suspend fun listGroups(at: IsoTimestamp? = null): List<GroupView> =
        decode(call("list_groups", RpcParams.listGroups(at)))

    // --- tokens (issue #8) ---------------------------------------------

    /**
     * Grant (positive) or revoke (negative) banked time on a token gate.
     * [subject] is an entry ID, or `group:<id>` for a whole category.
     */
    suspend fun adjustTokens(subject: String, deltaSeconds: Long): TokenStatus =
        decode(call("adjust_tokens", RpcParams.adjustTokens(subject, deltaSeconds)))

    // --- sessions ------------------------------------------------------

    suspend fun currentSession(): SessionInfo? =
        decodeNullable(call("current_session", RpcParams.currentSession()))

    suspend fun launch(id: String): LaunchOutcome =
        decode(call("launch", RpcParams.launch(id)))

    suspend fun stopCurrent(mode: StopMode = StopMode.GRACEFUL) {
        call("stop_current", RpcParams.stopCurrent(mode))
    }

    suspend fun extendCurrent(seconds: Long): ExtendResult =
        decode(call("extend_current", RpcParams.extendCurrent(seconds)))

    // --- overrides -----------------------------------------------------

    suspend fun listOverrides(date: IsoDate? = null): List<DailyOverride> =
        decode(call("list_overrides", RpcParams.listOverrides(date)))

    suspend fun getOverride(id: String, date: IsoDate? = null): DailyOverride? =
        decodeNullable(call("get_override", RpcParams.getOverride(id, date)))

    suspend fun upsertOverride(
        id: String,
        date: IsoDate? = null,
        availability: Boolean? = null,
        quotaDeltaSeconds: Long? = null,
    ): DailyOverride =
        decode(call(
            "upsert_override",
            RpcParams.upsertOverride(id, date, availability, quotaDeltaSeconds),
        ))

    suspend fun deleteOverride(id: String, date: IsoDate? = null): DeleteResult =
        decode(call("delete_override", RpcParams.deleteOverride(id, date)))

    // --- usage ---------------------------------------------------------

    suspend fun usageAll(from: IsoDate, to: IsoDate): List<UsageStat> =
        decode(call("usage_all", RpcParams.usageAll(from, to)))

    suspend fun usageEntry(id: String, from: IsoDate, to: IsoDate): List<UsageStat> =
        decode(call("usage_entry", RpcParams.usageEntry(id, from, to)))

    // --- volume / brightness ------------------------------------------

    suspend fun getVolume(): VolumeInfo = decode(call("get_volume", RpcParams.getVolume()))

    suspend fun setVolume(percent: Int): VolumeInfo =
        decode(call("set_volume", RpcParams.setVolume(percent)))

    suspend fun setMute(muted: Boolean): VolumeInfo =
        decode(call("set_mute", RpcParams.setMute(muted)))

    // --- per-output volume limits (issue #124) --------------------------

    suspend fun listAudioOutputs(): List<AudioOutputRecord> =
        decode(call("list_audio_outputs", RpcParams.listAudioOutputs()))

    /** `maxVolume = null` clears the cap and lets the global limit apply. */
    suspend fun setAudioOutputLimits(outputKey: String, maxVolume: Int?): AudioOutputRecord =
        decode(call("set_audio_output_limits", RpcParams.setAudioOutputLimits(outputKey, maxVolume)))

    suspend fun forgetAudioOutput(outputKey: String): Boolean =
        decode(call("forget_audio_output", RpcParams.forgetAudioOutput(outputKey)))

    /** Move sound to this output. Returns the reading for the new one. */
    suspend fun selectAudioOutput(outputKey: String): VolumeInfo =
        decode(call("select_audio_output", RpcParams.selectAudioOutput(outputKey)))

    suspend fun getBrightness(): BrightnessInfo =
        decode(call("get_brightness", RpcParams.getBrightness()))

    suspend fun setBrightness(percent: Int): BrightnessInfo =
        decode(call("set_brightness", RpcParams.setBrightness(percent)))

    suspend fun setAutoBrightness(enabled: Boolean): BrightnessInfo =
        decode(call("set_auto_brightness", RpcParams.setAutoBrightness(enabled)))

    // --- web management authentication (issue #156) --------------------

    /**
     * Whether the device's web UI has a password yet.
     *
     * The companion is the reset path: a parent who has forgotten the password
     * taps [setWebPassword] here rather than finding an SSH client, which is
     * what the issue's "for now, the reset flow can just be over SSH" was
     * settling for.
     */
    suspend fun webAuthStatus(): WebAuthStatus =
        decode(call("web_auth_status", RpcParams.webAuthStatus()))

    /** Set or replace the web UI's password. Existing sessions survive. */
    suspend fun setWebPassword(password: String) {
        call("set_web_password", RpcParams.setWebPassword(password))
    }

    suspend fun listWebSessions(): List<WebSessionInfo> =
        decode(call("list_web_sessions", RpcParams.listWebSessions()))

    suspend fun revokeWebSession(id: String) {
        call("revoke_web_session", RpcParams.revokeWebSession(id))
    }

    /**
     * Browsers waiting to be let in, each with the six digits it is showing.
     *
     * The parent compares those digits against the screen in front of them
     * before approving — the same ritual as pairing, and for the same reason:
     * a request that is not theirs shows a different number.
     */
    suspend fun listLoginRequests(): List<LoginRequestInfo> =
        decode(call("list_login_requests", RpcParams.listLoginRequests()))

    suspend fun approveLoginRequest(id: String) {
        call("approve_login_request", RpcParams.approveLoginRequest(id))
    }

    suspend fun denyLoginRequest(id: String) {
        call("deny_login_request", RpcParams.denyLoginRequest(id))
    }

    // --- misc ----------------------------------------------------------

    suspend fun reloadConfig(): ReloadResult = decode(call("reload_config", RpcParams.reloadConfig()))

    /**
     * Re-fetch playlists, videos and sponsor segments now instead of waiting
     * out their caches (issue #165).
     *
     * Returns once the device has accepted the request. The work behind it —
     * a `yt-dlp` run per playlist, then downloads — takes far longer than the
     * RPC deadline, so what came of it arrives as a diagnostic on the device
     * health screen rather than in this reply.
     */
    suspend fun refreshMedia() {
        call("refresh_media", RpcParams.refreshMedia())
    }

    suspend fun logout() {
        call("logout", RpcParams.logout())
    }

    /**
     * Administrator-facing conditions currently true of the device (issue #143)
     * — a missing dependency, a protection that is not in effect.
     */
    suspend fun listDiagnostics(): DiagnosticSet =
        decode(call("list_diagnostics", RpcParams.listDiagnostics()))

    /**
     * Where the device is on the network, and where its web interface is
     * listening (issue #182).
     *
     * The one question this app cannot answer any other way: it reached the
     * device over BLE and has no idea what its address is.
     */
    suspend fun networkStatus(): NetworkStatusView =
        decode(call("network_status", RpcParams.networkStatus()))

    /** Relax the kiosk so the device can be set up in place (issue #154). */
    suspend fun enterAdminMode() {
        call("enter_admin_mode", RpcParams.enterAdminMode())
    }

    /**
     * Leave administrator mode. Never refused by the device, whatever is still
     * on screen — this is the escape hatch when the HUD will not offer its own
     * exit because a window refuses to close.
     *
     * The device logs its desktop session out on the way (issue #154), which is
     * what makes leaving a reset rather than a flag flip: nothing tracks what
     * the mode started. Expect the connection to drop shortly afterwards.
     */
    suspend fun exitAdminMode() {
        call("exit_admin_mode", RpcParams.exitAdminMode())
    }

    /** Cover the screen while leaving everything running (issue #154). */
    suspend fun lockDevice() {
        call("lock_device", RpcParams.lockDevice())
    }

    /**
     * Uncover it. This and the web app are the only places it can be done: the
     * device itself offers no way back in, which is what makes locking it and
     * walking away safe.
     */
    suspend fun unlockDevice() {
        call("unlock_device", RpcParams.unlockDevice())
    }

    suspend fun listWindows(): List<WindowInfo> =
        decode(call("list_windows", RpcParams.listWindows()))

    suspend fun actOnWindow(id: Long, action: WindowAction) {
        call("act_on_window", RpcParams.actOnWindow(id, action))
    }

    // --- plumbing ------------------------------------------------------

    private suspend fun call(method: String, params: JsonElement): JsonElement =
        connection.call(method, params)

    private inline fun <reified T> decode(element: JsonElement): T =
        ShepherdJson.decodeFromJsonElement(kotlinx.serialization.serializer(), element)

    private inline fun <reified T> decodeNullable(element: JsonElement): T? =
        if (element is JsonNull) null else decode(element)
}
