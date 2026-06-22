@file:OptIn(ExperimentalUuidApi::class)

package com.shepherd.companion.ble

import com.juul.kable.Scanner
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlin.uuid.ExperimentalUuidApi

/** One advertisement from a shepherd device during onboarding. */
data class DiscoveredDevice(
    /** Platform identifier — the MAC address on Android. */
    val identifier: String,
    /** Advertised local name, if present (default "shepherd"). */
    val name: String?,
    /** Received signal strength, in dBm. */
    val rssi: Int,
)

/**
 * Scans for shepherd devices, filtered natively to the management
 * service UUID. The returned [Flow] is cold — collection starts the
 * system scanner and cancellation stops it, so scanning never runs in
 * the background (a project constraint).
 */
class ShepherdScanner {
    fun scan(): Flow<DiscoveredDevice> =
        Scanner {
            filters {
                match { services = listOf(Protocol.MANAGEMENT_SERVICE) }
            }
        }.advertisements.map { advertisement ->
            DiscoveredDevice(
                identifier = advertisement.identifier,
                name = advertisement.name ?: advertisement.peripheralName,
                rssi = advertisement.rssi,
            )
        }
}
