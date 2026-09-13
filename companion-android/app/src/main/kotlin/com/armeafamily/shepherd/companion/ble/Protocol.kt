package com.armeafamily.shepherd.companion.ble

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

    /**
     * Protocol version the app speaks. The app refuses a device that answers
     * with a different one.
     *
     * Hand-mirrored from `crates/shepherd-ble/src/protocol.rs`, and guarded by
     * `protocol_constants_match_the_companion` in
     * `crates/shepherd-wire-codegen/tests/rpc_codegen_drift.rs` — bumping one
     * side and not the other is a wire break that compiles cleanly on both and
     * shows up only as a phone refusing to pair, which is exactly how it was
     * found the first time (issue #149).
     */
    const val PROTOCOL_VERSION: Long = 2

    /**
     * Maximum logical-frame size the app will accept from a notification
     * stream. Matches the protocol's hard `u16` length-prefix ceiling
     * (65_535 bytes).
     *
     * This is *not* the same as the device's `MAX_FRAME_BYTES` on the
     * Request characteristic (which is a 16 KiB guard against a buggy /
     * malicious client claiming a huge incoming write). For Response /
     * Events the bound is what the *server* can legitimately push — and
     * a `state_changed` snapshot with a dozen-plus configured entries
     * comfortably exceeds 16 KiB. The protocol itself cannot exceed
     * 64 KiB per frame because of the length prefix's width.
     */
    const val MAX_FRAME_BYTES: Int = 0xFFFF

    /** MTU we request right after service discovery. */
    const val DESIRED_MTU: Int = 517

    /** The device's default advertised local name. */
    const val DEFAULT_DEVICE_NAME: String = "shepherd"
}
