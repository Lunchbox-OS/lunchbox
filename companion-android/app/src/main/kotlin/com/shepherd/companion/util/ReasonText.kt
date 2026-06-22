package com.shepherd.companion.util

import com.shepherd.companion.ble.ErrorCode
import com.shepherd.companion.ble.RpcException
import com.shepherd.companion.domain.ReasonCode

/** Human-readable one-liners for wire [ReasonCode]s and RPC errors. */
object ReasonText {

    fun describe(reason: ReasonCode): String = when (reason) {
        is ReasonCode.OutsideTimeWindow ->
            reason.nextWindowStart?.let { "Outside allowed hours — next ${Formatting.clock(it)}" }
                ?: "Outside allowed hours"

        is ReasonCode.QuotaExhausted ->
            "Daily limit reached (${Formatting.coarse(reason.quota.secs)} used up)"

        is ReasonCode.CooldownActive ->
            "Cooling down — available ${Formatting.clock(reason.availableAt)}"

        is ReasonCode.SessionActive ->
            "Another activity is running" +
                (reason.remaining?.let { " (${Formatting.coarse(it.secs)} left)" } ?: "")

        is ReasonCode.UnsupportedKind ->
            "This device can't launch ${reason.kind.name.lowercase()} activities"

        is ReasonCode.Disabled ->
            reason.reason?.let { "Disabled: $it" } ?: "Disabled by parent"

        is ReasonCode.InternetUnavailable ->
            "Needs internet" + (reason.check?.let { " ($it)" } ?: "")

        is ReasonCode.ManuallyDisabled ->
            "Blocked for the day (until ${reason.until})"
    }

    /** Maps an [RpcException] to user-facing copy, per the spec's error table. */
    fun describe(error: RpcException): String = when (error.code) {
        ErrorCode.PARSE_ERROR -> "Communication error; try again."
        ErrorCode.METHOD_NOT_FOUND -> "This device has features your app doesn't yet support."
        ErrorCode.INVALID_PARAMS -> error.message
        ErrorCode.NOT_CLAIMED -> "This device isn't set up yet."
        ErrorCode.ALREADY_CLAIMED -> "Another phone is already paired with this device."
        ErrorCode.PERMISSION_DENIED -> "This phone is not authorised to control this device."
        ErrorCode.NOT_FOUND -> error.message
        ErrorCode.BAD_REQUEST -> error.message
        ErrorCode.FORBIDDEN -> "This action is not allowed by the policy."
        ErrorCode.UNPROCESSABLE -> error.message
        ErrorCode.INTERNAL -> "Device error: ${error.message}"
        ErrorCode.INVALID_REQUEST, ErrorCode.CONFLICT -> error.message
    }
}
