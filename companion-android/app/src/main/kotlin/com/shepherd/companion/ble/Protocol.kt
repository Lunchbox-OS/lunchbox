package com.shepherd.companion.ble

import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid

/**
 * GATT identifiers and constants for the Shepherd Management Service.
 *
 * These mirror `crates/shepherd-ble/src/protocol.rs` in the device
 * firmware verbatim; they are a stable wire contract, not implementation
 * detail. Do not change them without changing the device side.
 */
@OptIn(ExperimentalUuidApi::class)
object Protocol {
    val MANAGEMENT_SERVICE: Uuid = Uuid.parse("8c0c0001-3b21-4abc-9e3f-0a9c1f2e3d40")
    val DEVICE_INFO_CHAR: Uuid = Uuid.parse("8c0c0002-3b21-4abc-9e3f-0a9c1f2e3d40")
    val REQUEST_CHAR: Uuid = Uuid.parse("8c0c0003-3b21-4abc-9e3f-0a9c1f2e3d40")
    val RESPONSE_CHAR: Uuid = Uuid.parse("8c0c0004-3b21-4abc-9e3f-0a9c1f2e3d40")
    val EVENTS_CHAR: Uuid = Uuid.parse("8c0c0005-3b21-4abc-9e3f-0a9c1f2e3d40")

    /** Protocol version the app speaks. The device rejects mismatches. */
    const val PROTOCOL_VERSION: Int = 1

    /** Server-side per-frame cap. The app never approaches it. */
    const val MAX_FRAME_BYTES: Int = 16 * 1024

    /** MTU we request right after service discovery. */
    const val DESIRED_MTU: Int = 517

    /** The device's default advertised local name. */
    const val DEFAULT_DEVICE_NAME: String = "shepherd"
}
