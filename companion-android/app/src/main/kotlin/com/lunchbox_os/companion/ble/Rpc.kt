package com.lunchbox_os.companion.ble

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import com.lunchbox_os.companion.domain.LunchboxWireModule
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNamingStrategy

/**
 * Shared JSON codec for the BLE wire protocol.
 *
 * The device serialises Rust structs with `#[serde(rename_all =
 * "snake_case")]`; [JsonNamingStrategy.SnakeCase] converts our camelCase
 * Kotlin property names to match without per-field annotations. Enum
 * constants and a few externally-tagged shapes still use explicit
 * `@SerialName`, since the naming strategy only rewrites class
 * properties.
 *
 * - `ignoreUnknownKeys`: forward-compatible with newer firmware fields.
 * - `explicitNulls = false`: omit `null` fields (matches serde's
 *   `skip_serializing_if = "Option::is_none"`) and tolerate absent ones.
 * - `classDiscriminator = "type"`: default for our internally-tagged
 *   enums; [com.lunchbox_os.companion.domain.ReasonCode] overrides it to
 *   `"code"`.
 *
 * `ignoreUnknownKeys` covers unknown *fields*, but not unknown polymorphic
 * discriminator *values* — those throw. Since `reasons` is nested inside
 * `EntryView`, a single unrecognised reason code from a newer device would
 * otherwise fail the decode of the entire entry list, so
 * [ReasonCode.Unknown] is registered as the default below.
 */
@OptIn(ExperimentalSerializationApi::class)
val LunchboxJson: Json = Json {
    namingStrategy = JsonNamingStrategy.SnakeCase
    ignoreUnknownKeys = true
    explicitNulls = false
    encodeDefaults = true
    classDiscriminator = "type"
    // Generated: one polymorphic default per tagged enum, so an unrecognised
    // discriminator from a newer device degrades to that enum's `Unknown`
    // instead of failing the decode of the whole response.
    serializersModule = LunchboxWireModule
}

/** Request written to the Request characteristic. */
@Serializable
data class RpcRequest(
    val id: Long,
    val method: String,
    val params: JsonElement,
)

/** Response notified on the Response characteristic. */
@Serializable
data class RpcResponse(
    val id: Long,
    val result: JsonElement? = null,
    val error: RpcError? = null,
)

@Serializable
data class RpcError(
    val code: ErrorCode,
    val message: String,
)

/**
 * Wire error codes, serialised as snake_case strings. Mirrors
 * `ErrorCode` in `crates/lunchbox-ble/src/protocol.rs`.
 */
@Serializable
enum class ErrorCode {
    @SerialName("parse_error") PARSE_ERROR,
    @SerialName("invalid_request") INVALID_REQUEST,
    @SerialName("method_not_found") METHOD_NOT_FOUND,
    @SerialName("invalid_params") INVALID_PARAMS,
    @SerialName("not_claimed") NOT_CLAIMED,
    /**
     * No longer sent by a device speaking protocol v2: a second phone now gets
     * a pending enrolment instead of a refusal (issue #149). Kept so the enum
     * still decodes a v1 device's answer.
     */
    @SerialName("already_claimed") ALREADY_CLAIMED,
    @SerialName("permission_denied") PERMISSION_DENIED,
    @SerialName("enrolment_denied") ENROLMENT_DENIED,
    @SerialName("not_found") NOT_FOUND,
    @SerialName("bad_request") BAD_REQUEST,
    @SerialName("forbidden") FORBIDDEN,
    @SerialName("conflict") CONFLICT,
    @SerialName("unprocessable") UNPROCESSABLE,
    @SerialName("internal") INTERNAL,
}

/**
 * Thrown when the device returns an `error` response. Carries the
 * structured [code] so the UI can branch without string-matching the
 * human [message].
 */
class RpcException(
    val code: ErrorCode,
    override val message: String,
) : Exception(message)
