@file:OptIn(ExperimentalSerializationApi::class)

package com.lunchbox_os.companion.domain

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * The hand-written remainder of the wire mirror.
 *
 * Everything with a plain serde shape is generated into
 * `WireTypes.generated.kt` from the Rust types — see
 * `crates/lunchbox-management/src/bin/kotlin_types.rs`. What stays here is
 * what a generator cannot faithfully produce:
 *
 * - [LaunchOutcome] is externally tagged (`{"Approved": {…}}`) and needs a
 *   bespoke serializer.
 * - [Event] / [EventPayload]: the `state_changed` variant flattens a `$ref`
 *   beside its tag, which kotlinx cannot express as a sealed subclass.
 * - Convenience affordances on generated types, as extensions.
 */

// --- convenience on generated types -----------------------------------

/** Build a [DurationSecs] from whole seconds. */
fun durationOfSeconds(s: Long) = DurationSecs(s, 0)

/** The limit subject addressing a category in override calls (issue #5). */
val GroupView.subject: String get() = "group:" + groupId

/** Wrapper results: the RPC layer unwraps `{"<field>": ...}` responses. */
@Serializable
data class ExtendResult(val newDeadline: IsoTimestamp? = null)

@Serializable
data class DeleteResult(val deleted: Boolean)

@Serializable
data class ReloadResult(val entryCount: Long)
/**
 * `launch` result. Serde serialises `LaunchOutcome` externally tagged:
 * `{"Approved": {..}}` or `{"Denied": {..}}`. The PascalCase outer keys are
 * not snake_case, so a custom serializer reads them directly rather than
 * relying on property names (which the global naming strategy would
 * otherwise rewrite).
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

// --- events -----------------------------------------------------------


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
        val apiVersion: Long,
        val policyLoaded: Boolean,
        val currentSession: SessionInfo? = null,
        val entryCount: Long,
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
