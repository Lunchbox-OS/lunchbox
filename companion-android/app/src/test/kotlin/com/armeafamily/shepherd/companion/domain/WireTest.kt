package com.armeafamily.shepherd.companion.domain

import com.armeafamily.shepherd.companion.ble.ErrorCode
import com.armeafamily.shepherd.companion.ble.RpcResponse
import com.armeafamily.shepherd.companion.ble.ShepherdJson
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Decodes the exact sample payloads from the companion spec to lock the
 * snake_case naming strategy and tagged-enum mappings against the wire.
 */
class WireTest {

    private inline fun <reified T> decode(json: String): T =
        ShepherdJson.decodeFromString(json)

    @Test
    fun `entry view with empty reasons`() {
        val json = """
            {
              "entry_id": "steam-celeste",
              "label": "Celeste",
              "icon_ref": "steam://celeste/icon",
              "kind_tag": "steam",
              "enabled": true,
              "reasons": [],
              "max_run_if_started_now": { "secs": 1800, "nanos": 0 }
            }
        """.trimIndent()
        val entry = decode<EntryView>(json)
        assertEquals("steam-celeste", entry.entryId)
        assertEquals(EntryKindTag.STEAM, entry.kindTag)
        assertEquals(1800, entry.maxRunIfStartedNow?.secs)
        assertTrue(entry.reasons.isEmpty())
    }

    @Test
    fun `reason codes decode by discriminator`() {
        val quota = decode<ReasonCode>(
            """{"code":"quota_exhausted","used":{"secs":3600,"nanos":0},"quota":{"secs":3600,"nanos":0}}""",
        )
        assertTrue(quota is ReasonCode.QuotaExhausted)

        val window = decode<ReasonCode>(
            """{"code":"outside_time_window","next_window_start":"2026-06-21T17:00:00-04:00"}""",
        )
        assertEquals("2026-06-21T17:00:00-04:00", (window as ReasonCode.OutsideTimeWindow).nextWindowStart)

        val session = decode<ReasonCode>(
            """{"code":"session_active","entry_id":"steam-celeste","remaining":{"secs":600,"nanos":0}}""",
        )
        assertEquals("steam-celeste", (session as ReasonCode.SessionActive).entryId)
    }

    @Test
    fun `launch outcome approved`() {
        val outcome = decode<LaunchOutcome>(
            """{"Approved":{"session_id":"uuid-1","deadline":"2026-06-21T18:35:00-04:00"}}""",
        )
        assertTrue(outcome.isApproved)
        assertEquals("uuid-1", outcome.approved?.sessionId)
        assertNull(outcome.denied)
    }

    @Test
    fun `launch outcome denied carries reasons`() {
        val outcome = decode<LaunchOutcome>(
            """{"Denied":{"reasons":[{"code":"disabled","reason":"manually disabled by parent"}]}}""",
        )
        assertEquals(false, outcome.isApproved)
        assertEquals(1, outcome.denied?.reasons?.size)
    }

    @Test
    fun `device info`() {
        val info = decode<DeviceInfo>(
            """{"protocol_version":1,"firmware_version":"0.1.0","claim_state":"unclaimed","device_name":"shepherd"}""",
        )
        assertEquals(1, info.protocolVersion)
        assertEquals(ClaimStateTag.UNCLAIMED, info.claimState)
    }

    @Test
    fun `admin record from claim`() {
        val record = decode<AdminRecord>(
            """
            {
              "identity_address": "AA:BB:CC:DD:EE:FF",
              "address_type": "public",
              "device_name": "Pixel 8",
              "bonded_at": "2026-06-20T22:30:00-04:00",
              "http_token": "9b2e",
              "role": "admin"
            }
            """.trimIndent(),
        )
        assertEquals("AA:BB:CC:DD:EE:FF", record.identityAddress)
        assertEquals("9b2e", record.httpToken)
    }

    @Test
    fun `state_changed event inlines the snapshot`() {
        val json = """
            {
              "api_version": 1,
              "timestamp": "2026-06-21T18:05:00-04:00",
              "payload": {
                "type": "state_changed",
                "api_version": 1,
                "policy_loaded": true,
                "current_session": null,
                "entry_count": 2,
                "entries": [],
                "internet_status": []
              }
            }
        """.trimIndent()
        val event = decode<Event>(json)
        val payload = event.payload
        assertTrue(payload is EventPayload.StateChanged)
        assertEquals(2, (payload as EventPayload.StateChanged).toSnapshot().entryCount)
    }

    @Test
    fun `session_ended event has nested tagged reason`() {
        val json = """
            {
              "api_version": 1,
              "timestamp": "2026-06-21T18:35:00-04:00",
              "payload": {
                "type": "session_ended",
                "session_id": "uuid-1",
                "entry_id": "steam-celeste",
                "reason": { "type": "process_exited", "exit_code": 0 },
                "duration": { "secs": 1740, "nanos": 0 }
              }
            }
        """.trimIndent()
        val ended = decode<Event>(json).payload as EventPayload.SessionEnded
        assertEquals(0, (ended.reason as SessionEndReason.ProcessExited).exitCode)
        assertEquals(1740, ended.duration.secs)
    }

    @Test
    fun `error response maps code`() {
        val resp = decode<RpcResponse>(
            """{"id":7,"error":{"code":"not_found","message":"No entry with id 'missing'"}}""",
        )
        assertEquals(ErrorCode.NOT_FOUND, resp.error?.code)
        assertNull(resp.result)
    }

    @Test
    fun `volume info round trips with restrictions`() {
        val json = """
            {"percent":42,"muted":false,"available":true,"backend":"pipewire",
             "restrictions":{"max_volume":80,"min_volume":null,"allow_mute":true,"allow_change":true}}
        """.trimIndent()
        val volume = decode<VolumeInfo>(json)
        assertEquals(42, volume.percent)
        assertEquals(80, volume.restrictions.maxVolume)
        assertNull(volume.restrictions.minVolume)
    }
}
