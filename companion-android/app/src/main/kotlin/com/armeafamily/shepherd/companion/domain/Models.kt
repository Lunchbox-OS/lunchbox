@file:OptIn(ExperimentalSerializationApi::class)

package com.armeafamily.shepherd.companion.domain

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonClassDiscriminator
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * Kotlin mirrors of the wire types in `crates/shepherd-api/src/types.rs`
 * and `events.rs`. Field names are camelCase; the snake_case wire form is
 * produced by [com.armeafamily.shepherd.companion.ble.ShepherdJson]'s naming
 * strategy. Enum constants and the few tagged/externally-tagged shapes
 * carry explicit `@SerialName`s.
 */

// --- primitives -------------------------------------------------------

/** ISO-8601 timestamp with offset, e.g. "2026-06-21T18:05:00-04:00". */
typealias IsoTimestamp = String

/** ISO-8601 local date, e.g. "2026-06-21". */
typealias IsoDate = String

/**
 * `serde`-serialised `std::time::Duration`: `{ "secs": .., "nanos": .. }`.
 * The UI only cares about whole seconds; [nanos] is carried for fidelity
 * but otherwise ignored.
 */
@Serializable
data class DurationSecs(
    val secs: Long,
    val nanos: Long = 0,
) {
    companion object {
        fun ofSeconds(s: Long) = DurationSecs(s, 0)
    }
}

// --- entries ----------------------------------------------------------

@Serializable
enum class EntryKindTag {
    @SerialName("process") PROCESS,
    @SerialName("snap") SNAP,
    @SerialName("steam") STEAM,
    @SerialName("flatpak") FLATPAK,
    @SerialName("vm") VM,
    @SerialName("media") MEDIA,
    @SerialName("custom") CUSTOM,
}

@Serializable
data class EntryView(
    val entryId: String,
    val label: String,
    val iconRef: String? = null,
    val kindTag: EntryKindTag,
    val enabled: Boolean,
    /** Category this activity shares a schedule and budget with (issue #5). */
    val group: String? = null,
    val reasons: List<ReasonCode> = emptyList(),
    val maxRunIfStartedNow: DurationSecs? = null,
)

/**
 * A category of activities sharing one schedule and one combined budget
 * (issue #5).
 *
 * `usedToday` is the *combined* usage of every member, and `reasons` are the
 * group's own restrictions — the same ones members carry wrapped in
 * [ReasonCode.GroupRestricted].
 */
@Serializable
data class GroupView(
    val groupId: String,
    val label: String,
    val memberIds: List<String> = emptyList(),
    val enabled: Boolean,
    val reasons: List<ReasonCode> = emptyList(),
    val usedToday: DurationSecs,
    val dailyQuota: DurationSecs? = null,
    val maxRunIfStartedNow: DurationSecs? = null,
) {
    /** The limit subject addressing this category in override calls. */
    val subject: String get() = "group:" + groupId
}

/** Tagged enum; discriminator is `code` (not the global `type`). */
@Serializable
@JsonClassDiscriminator("code")
sealed interface ReasonCode {
    @Serializable
    @SerialName("outside_time_window")
    data class OutsideTimeWindow(val nextWindowStart: IsoTimestamp? = null) : ReasonCode

    @Serializable
    @SerialName("quota_exhausted")
    data class QuotaExhausted(val used: DurationSecs, val quota: DurationSecs) : ReasonCode

    @Serializable
    @SerialName("cooldown_active")
    data class CooldownActive(val availableAt: IsoTimestamp) : ReasonCode

    @Serializable
    @SerialName("session_active")
    data class SessionActive(val entryId: String, val remaining: DurationSecs? = null) : ReasonCode

    @Serializable
    @SerialName("unsupported_kind")
    data class UnsupportedKind(val kind: EntryKindTag) : ReasonCode

    @Serializable
    @SerialName("disabled")
    data class Disabled(val reason: String? = null) : ReasonCode

    @Serializable
    @SerialName("internet_unavailable")
    data class InternetUnavailable(val check: String? = null) : ReasonCode

    @Serializable
    @SerialName("manually_disabled")
    data class ManuallyDisabled(val until: IsoDate) : ReasonCode

    @Serializable
    @SerialName("not_ready")
    data class NotReady(val kind: EntryKindTag) : ReasonCode

    @Serializable
    @SerialName("required_input_unavailable")
    data class RequiredInputUnavailable(val devices: List<String> = emptyList()) : ReasonCode

    @Serializable
    @SerialName("tokens_insufficient")
    data class TokensInsufficient(
        val balance: DurationSecs,
        val required: DurationSecs,
    ) : ReasonCode

    /** A limit that comes from the entry's group rather than the entry (issue #5). */
    @Serializable
    @SerialName("group_restricted")
    data class GroupRestricted(
        val group: String,
        val label: String,
        val reason: ReasonCode,
    ) : ReasonCode

    /**
     * A reason this build of the app doesn't know about.
     *
     * Registered as the polymorphic default (see
     * [com.armeafamily.shepherd.companion.ble.ShepherdJson]) so a device
     * running a newer shepherdd degrades to "unavailable for some reason"
     * instead of failing the decode of the whole response. Without this, one
     * unrecognised reason code takes down the entire entry list — `reasons`
     * is nested inside [EntryView], so the failure is not contained to the
     * entry that carries it.
     */
    @Serializable
    @SerialName("__unknown")
    data class Unknown(val code: String? = null) : ReasonCode
}

// --- sessions ---------------------------------------------------------

@Serializable
enum class SessionState {
    @SerialName("launching") LAUNCHING,
    @SerialName("running") RUNNING,
    @SerialName("warned") WARNED,
    @SerialName("expiring") EXPIRING,
    @SerialName("ended") ENDED,
}

@Serializable
data class SessionInfo(
    val sessionId: String,
    val entryId: String,
    val label: String,
    val state: SessionState,
    val startedAt: IsoTimestamp,
    val deadline: IsoTimestamp? = null,
    val timeRemaining: DurationSecs? = null,
    val warningsIssued: List<Long> = emptyList(),
)

/**
 * `launch` result. Serde serialises `LaunchOutcome` externally tagged:
 * `{"Approved": {..}}` or `{"Denied": {..}}`. The PascalCase outer keys
 * are not snake_case, so a custom serializer reads them directly rather
 * than relying on property names (which the global naming strategy would
 * otherwise rewrite). The inner objects decode normally — their fields
 * (`session_id`, `reasons`) follow the snake_case strategy.
 */
@Serializable(with = LaunchOutcomeSerializer::class)
data class LaunchOutcome(
    val approved: Approved? = null,
    val denied: Denied? = null,
) {
    @Serializable
    data class Approved(val sessionId: String, val deadline: IsoTimestamp? = null)

    @Serializable
    data class Denied(val reasons: List<ReasonCode> = emptyList())

    val isApproved: Boolean get() = approved != null
}

object LaunchOutcomeSerializer : kotlinx.serialization.KSerializer<LaunchOutcome> {
    override val descriptor =
        kotlinx.serialization.descriptors.buildClassSerialDescriptor("LaunchOutcome")

    override fun deserialize(decoder: kotlinx.serialization.encoding.Decoder): LaunchOutcome {
        val input = decoder as? kotlinx.serialization.json.JsonDecoder
            ?: error("LaunchOutcome is JSON-only")
        val obj = input.decodeJsonElement() as JsonObject
        obj["Approved"]?.let {
            return LaunchOutcome(approved = input.json.decodeFromJsonElement(LaunchOutcome.Approved.serializer(), it))
        }
        obj["Denied"]?.let {
            return LaunchOutcome(denied = input.json.decodeFromJsonElement(LaunchOutcome.Denied.serializer(), it))
        }
        return LaunchOutcome()
    }

    override fun serialize(encoder: kotlinx.serialization.encoding.Encoder, value: LaunchOutcome) {
        val output = encoder as? kotlinx.serialization.json.JsonEncoder
            ?: error("LaunchOutcome is JSON-only")
        val element = when {
            value.approved != null -> kotlinx.serialization.json.buildJsonObject {
                put("Approved", output.json.encodeToJsonElement(LaunchOutcome.Approved.serializer(), value.approved))
            }
            value.denied != null -> kotlinx.serialization.json.buildJsonObject {
                put("Denied", output.json.encodeToJsonElement(LaunchOutcome.Denied.serializer(), value.denied))
            }
            else -> JsonObject(emptyMap())
        }
        output.encodeJsonElement(element)
    }
}

@Serializable
enum class StopMode {
    @SerialName("graceful") GRACEFUL,
    @SerialName("force") FORCE,
}

@Serializable
data class ExtendResult(val newDeadline: IsoTimestamp? = null)

// --- overrides --------------------------------------------------------

@Serializable
data class DailyOverride(
    /**
     * What the override applies to: a bare entry ID, or `group:<id>` for a
     * whole category (issue #5). Renamed from `entry_id` when limits gained
     * group-level subjects.
     */
    val subject: String,
    val date: IsoDate,
    val availability: Boolean? = null,
    val quotaDeltaSeconds: Long? = null,
    val createdAt: IsoTimestamp,
    val updatedAt: IsoTimestamp,
)

@Serializable
data class DeleteResult(val deleted: Boolean)

// --- usage ------------------------------------------------------------

@Serializable
data class UsageStat(
    val entryId: String,
    val label: String,
    val date: IsoDate,
    val durationSeconds: Long,
)

// --- volume / brightness ---------------------------------------------

@Serializable
data class VolumeRestrictions(
    val maxVolume: Int? = null,
    val minVolume: Int? = null,
    val allowMute: Boolean = true,
    val allowChange: Boolean = true,
)

@Serializable
data class VolumeInfo(
    val percent: Int,
    val muted: Boolean,
    val available: Boolean,
    val backend: String? = null,
    val restrictions: VolumeRestrictions = VolumeRestrictions(),
)

@Serializable
data class BrightnessRestrictions(
    val maxBrightness: Int? = null,
    val minBrightness: Int? = null,
    val allowChange: Boolean = true,
)

@Serializable
data class BrightnessInfo(
    val percent: Int,
    val available: Boolean,
    val backend: String? = null,
    val device: String? = null,
    val restrictions: BrightnessRestrictions = BrightnessRestrictions(),
    val autoAvailable: Boolean = false,
    val autoEnabled: Boolean = false,
)

// --- health / state ---------------------------------------------------

@Serializable
data class HealthStatus(
    val live: Boolean,
    val ready: Boolean,
    val policyLoaded: Boolean,
    val hostAdapterOk: Boolean,
    val storeOk: Boolean,
)

@Serializable
data class InternetStatusView(
    val target: String,
    val available: Boolean,
)

@Serializable
data class ServiceStateSnapshot(
    val apiVersion: Int,
    val policyLoaded: Boolean,
    val currentSession: SessionInfo? = null,
    val entryCount: Int,
    val entries: List<EntryView> = emptyList(),
    val internetStatus: List<InternetStatusView> = emptyList(),
)

@Serializable
data class ReloadResult(val entryCount: Int)

// --- windows (debug) --------------------------------------------------

@Serializable
enum class WindowAction {
    @SerialName("close") CLOSE,
    @SerialName("hide") HIDE,
    @SerialName("show") SHOW,
}

@Serializable
data class WindowInfo(
    val id: Long,
    val name: String? = null,
    val appId: String? = null,
    val windowClass: String? = null,
    val pid: Long? = null,
    val workspace: String? = null,
    val inScratchpad: Boolean,
    val visible: Boolean,
    val focused: Boolean,
)

// --- device info / claim ---------------------------------------------

@Serializable
enum class ClaimStateTag {
    @SerialName("unclaimed") UNCLAIMED,
    @SerialName("claimed") CLAIMED,
}

/** Payload of the unencrypted DeviceInfo characteristic. */
@Serializable
data class DeviceInfo(
    val protocolVersion: Int,
    val firmwareVersion: String,
    val claimState: ClaimStateTag,
    val deviceName: String,
)

/** Result of a successful `claim`. The `http_token` is secret. */
@Serializable
data class AdminRecord(
    val identityAddress: String,
    val addressType: String,
    val deviceName: String,
    val bondedAt: IsoTimestamp,
    val httpToken: String,
    val role: String,
)

// --- events -----------------------------------------------------------

@Serializable
enum class WarningSeverity {
    @SerialName("info") INFO,
    @SerialName("warn") WARN,
    @SerialName("critical") CRITICAL,
}

@Serializable
sealed interface SessionEndReason {
    @Serializable @SerialName("expired") data object Expired : SessionEndReason

    @Serializable @SerialName("user_stop") data object UserStop : SessionEndReason

    @Serializable @SerialName("admin_stop") data object AdminStop : SessionEndReason

    @Serializable
    @SerialName("process_exited")
    data class ProcessExited(val exitCode: Int? = null) : SessionEndReason

    @Serializable @SerialName("policy_stop") data object PolicyStop : SessionEndReason

    @Serializable @SerialName("service_shutdown") data object ServiceShutdown : SessionEndReason

    @Serializable
    @SerialName("launch_failed")
    data class LaunchFailed(val error: String) : SessionEndReason
}

@Serializable
data class Event(
    val apiVersion: Int,
    val timestamp: IsoTimestamp,
    val payload: EventPayload,
)

/**
 * Tagged on `type`. The `state_changed` variant inlines all
 * [ServiceStateSnapshot] fields alongside the tag, matching serde's
 * newtype-variant flattening; [StateChanged.toSnapshot] rebuilds the
 * struct.
 */
@Serializable
sealed interface EventPayload {
    @Serializable
    @SerialName("state_changed")
    data class StateChanged(
        val apiVersion: Int,
        val policyLoaded: Boolean,
        val currentSession: SessionInfo? = null,
        val entryCount: Int,
        val entries: List<EntryView> = emptyList(),
        val internetStatus: List<InternetStatusView> = emptyList(),
    ) : EventPayload {
        fun toSnapshot() = ServiceStateSnapshot(
            apiVersion = apiVersion,
            policyLoaded = policyLoaded,
            currentSession = currentSession,
            entryCount = entryCount,
            entries = entries,
            internetStatus = internetStatus,
        )
    }

    @Serializable
    @SerialName("session_started")
    data class SessionStarted(
        val sessionId: String,
        val entryId: String,
        val label: String,
        val deadline: IsoTimestamp? = null,
    ) : EventPayload

    @Serializable
    @SerialName("warning_issued")
    data class WarningIssued(
        val sessionId: String,
        val thresholdSeconds: Long,
        val timeRemaining: DurationSecs,
        val severity: WarningSeverity,
        val message: String? = null,
    ) : EventPayload

    @Serializable
    @SerialName("session_expiring")
    data class SessionExpiring(val sessionId: String) : EventPayload

    @Serializable
    @SerialName("session_ended")
    data class SessionEnded(
        val sessionId: String,
        val entryId: String,
        val reason: SessionEndReason,
        val duration: DurationSecs,
    ) : EventPayload

    @Serializable
    @SerialName("policy_reloaded")
    data class PolicyReloaded(val entryCount: Int) : EventPayload

    @Serializable
    @SerialName("entry_availability_changed")
    data class EntryAvailabilityChanged(val entryId: String, val enabled: Boolean) : EventPayload

    @Serializable
    @SerialName("volume_changed")
    data class VolumeChanged(val percent: Int, val muted: Boolean) : EventPayload

    @Serializable
    @SerialName("brightness_changed")
    data class BrightnessChanged(val percent: Int, val autoEnabled: Boolean = false) : EventPayload

    @Serializable
    @SerialName("hud_scale_changed")
    data class HudScaleChanged(val factor: Double) : EventPayload

    @Serializable
    @SerialName("internet_status_changed")
    data class InternetStatusChanged(val target: String, val available: Boolean) : EventPayload

    @Serializable @SerialName("shutdown") data object Shutdown : EventPayload

    @Serializable
    @SerialName("audit_entry")
    data class AuditEntry(val eventType: String, val details: JsonElement) : EventPayload
}
