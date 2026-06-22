package com.shepherd.companion.ble

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
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
 *   enums; [com.shepherd.companion.domain.ReasonCode] overrides it to
 *   `"code"`.
 */
@OptIn(ExperimentalSerializationApi::class)
val ShepherdJson: Json = Json {
    namingStrategy = JsonNamingStrategy.SnakeCase
    ignoreUnknownKeys = true
    explicitNulls = false
    encodeDefaults = true
    classDiscriminator = "type"
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
 * `ErrorCode` in `crates/shepherd-ble/src/protocol.rs`.
 */
@Serializable
enum class ErrorCode {
    @SerialName("parse_error") PARSE_ERROR,
    @SerialName("invalid_request") INVALID_REQUEST,
    @SerialName("method_not_found") METHOD_NOT_FOUND,
    @SerialName("invalid_params") INVALID_PARAMS,
    @SerialName("not_claimed") NOT_CLAIMED,
    @SerialName("already_claimed") ALREADY_CLAIMED,
    @SerialName("permission_denied") PERMISSION_DENIED,
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
