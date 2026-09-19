package com.lunchboxos.companion.domain

import kotlinx.serialization.Serializable

/**
 * One bonded Lunchbox device, persisted locally (encrypted).
 *
 * Built from the [AdminRecord] returned by `claim`, plus the Android
 * scan [androidIdentifier] (MAC) used to reconnect without re-scanning,
 * and an optional user [nickname]. The [httpToken] is the secret bearer
 * token also accepted by the device's HTTP API — never log it.
 */
@Serializable
data class DeviceRecord(
    /** BlueZ-resolved identity address reported by the device. */
    val identityAddress: String,
    /** "public" | "random". */
    val addressType: String,
    /** The device's own name (from DeviceInfo / the claim record). */
    val deviceName: String,
    /** When the bond was established (ISO-8601). */
    val bondedAt: IsoTimestamp,
    /** Bearer token, also valid for the HTTP API. Secret. */
    val httpToken: String,
    /** "admin" in v1. */
    val role: String,
    /**
     * The Android platform identifier (MAC) seen at scan time. Used as
     * the handle for `Peripheral(identifier)` reconnects. Not part of the
     * wire protocol — a client-side convenience.
     */
    val androidIdentifier: String,
    /** Optional client-side label ("Kid's room"). Never sent on the wire. */
    val nickname: String? = null,
) {
    /** What to show in the device picker. */
    val displayName: String get() = nickname?.takeIf { it.isNotBlank() } ?: deviceName
}
