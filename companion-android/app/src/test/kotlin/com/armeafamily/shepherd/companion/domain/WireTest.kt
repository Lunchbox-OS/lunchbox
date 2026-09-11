package com.armeafamily.shepherd.companion.domain

import com.armeafamily.shepherd.companion.ble.ErrorCode
import com.armeafamily.shepherd.companion.ble.RpcResponse
import com.armeafamily.shepherd.companion.ble.ShepherdJson
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertFalse
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
    fun `group view decodes and derives its override subject`() {
        val group = decode<GroupView>(
            """
            {
              "group_id": "games",
              "label": "Games",
              "member_ids": ["game-a", "game-b"],
              "enabled": false,
              "reasons": [{"code":"quota_exhausted","used":{"secs":1800,"nanos":0},
                           "quota":{"secs":1800,"nanos":0}}],
              "used_today": {"secs": 1800, "nanos": 0},
              "daily_quota": {"secs": 1800, "nanos": 0},
              "max_run_if_started_now": {"secs": 900, "nanos": 0}
            }
            """.trimIndent(),
        )
        assertEquals("games", group.groupId)
        assertEquals(listOf("game-a", "game-b"), group.memberIds)
        assertEquals(1800, group.usedToday.secs)
        assertEquals(900, group.maxRunIfStartedNow?.secs)
        // The subject is what override calls address the category by.
        assertEquals("group:games", group.subject)
    }

    @Test
    fun `entry view carries its category`() {
        val entry = decode<EntryView>(
            """{"entry_id":"game-a","label":"Game A","kind_tag":"process",
                "enabled":true,"group":"games","reasons":[]}""",
        )
        assertEquals("games", entry.group)

        // Absent for an ungrouped activity, and for a device predating groups.
        val ungrouped = decode<EntryView>(
            """{"entry_id":"solo","label":"Solo","kind_tag":"process",
                "enabled":true,"reasons":[]}""",
        )
        assertNull(ungrouped.group)
    }

    @Test
    fun `token status rides along on the entry and group views`() {
        val entry = decode<EntryView>(
            """{"entry_id":"minecraft","label":"Minecraft","kind_tag":"process",
                "enabled":false,"reasons":[],
                "tokens":{"balance":{"secs":300,"nanos":0},
                          "minimum":{"secs":600,"nanos":0},
                          "unlocked":false,
                          "max_balance":{"secs":3600,"nanos":0},
                          "carry_over":false}}""",
        )
        val tokens = entry.tokens!!
        assertEquals(300, tokens.balance.secs)
        assertEquals(600, tokens.minimum.secs)
        assertEquals(false, tokens.unlocked)
        assertEquals(3600, tokens.maxBalance?.secs)

        // Absent when the activity has no gate, and on a device predating the
        // field — both must decode rather than throw.
        val ungated = decode<EntryView>(
            """{"entry_id":"solo","label":"Solo","kind_tag":"process",
                "enabled":true,"reasons":[]}""",
        )
        assertNull(ungated.tokens)

        // A category carries the gate its members share. `max_balance` is
        // nullable on the wire (0 = unlimited becomes None).
        val group = decode<GroupView>(
            """{"group_id":"games","label":"Games","member_ids":["game-a"],
                "enabled":true,"reasons":[],
                "used_today":{"secs":0,"nanos":0},
                "daily_quota":null,"max_run_if_started_now":null,
                "tokens":{"balance":{"secs":900,"nanos":0},
                          "minimum":{"secs":0,"nanos":0},
                          "unlocked":true,"carry_over":true}}""",
        )
        assertEquals(900, group.tokens?.balance?.secs)
        assertEquals(true, group.tokens?.carryOver)
        assertNull(group.tokens?.maxBalance)
    }

    @Test
    fun `every reason code the device can emit decodes`() {
        // These four were added to shepherdd after the app shipped and were
        // missing here, so any entry carrying one failed the decode of the
        // whole `list_entries` response.
        assertTrue(decode<ReasonCode>("""{"code":"not_ready","kind":"steam"}""") is ReasonCode.NotReady)

        val inputs = decode<ReasonCode>(
            """{"code":"required_input_unavailable","devices":["keyboard","mouse"]}""",
        )
        // Generated from the Rust enum, so these decode as typed values rather
        // than bare strings.
        assertEquals(
            listOf(InputDeviceType.KEYBOARD, InputDeviceType.MOUSE),
            (inputs as ReasonCode.RequiredInputUnavailable).devices,
        )

        val tokens = decode<ReasonCode>(
            """{"code":"tokens_insufficient","balance":{"secs":300,"nanos":0},"required":{"secs":1800,"nanos":0}}""",
        )
        assertEquals(1800, (tokens as ReasonCode.TokensInsufficient).required.secs)
    }

    @Test
    fun `group restricted wraps the underlying reason`() {
        val reason = decode<ReasonCode>(
            """{"code":"group_restricted","group":"games","label":"Games",
                "reason":{"code":"quota_exhausted","used":{"secs":3600,"nanos":0},
                          "quota":{"secs":3600,"nanos":0}}}""",
        )
        val group = reason as ReasonCode.GroupRestricted
        assertEquals("games", group.group)
        assertEquals("Games", group.label)
        assertTrue(group.reason is ReasonCode.QuotaExhausted)
    }

    @Test
    fun `unknown reason code degrades instead of failing the whole response`() {
        // A newer device may send a reason this build has never heard of.
        // It must not take the entry list down with it.
        val entry = decode<EntryView>(
            """
            {
              "entry_id": "steam-celeste",
              "label": "Celeste",
              "kind_tag": "steam",
              "enabled": false,
              "reasons": [
                {"code": "from_the_future", "whatever": 1},
                {"code": "quota_exhausted", "used":{"secs":60,"nanos":0}, "quota":{"secs":60,"nanos":0}}
              ]
            }
            """.trimIndent(),
        )
        assertEquals(2, entry.reasons.size)
        assertTrue(entry.reasons[0] is ReasonCode.Unknown)
        assertTrue(entry.reasons[1] is ReasonCode.QuotaExhausted)
    }

    @Test
    fun `daily override is keyed by subject`() {
        val entry = decode<DailyOverride>(
            """{"subject":"steam-celeste","date":"2026-07-19","availability":true,
                "created_at":"2026-07-19T10:00:00-04:00","updated_at":"2026-07-19T10:00:00-04:00"}""",
        )
        assertEquals("steam-celeste", entry.subject)
        assertEquals(true, entry.availability)

        // A whole category can carry an override too (issue #5).
        val group = decode<DailyOverride>(
            """{"subject":"group:games","date":"2026-07-19","availability":false,
                "created_at":"2026-07-19T10:00:00-04:00","updated_at":"2026-07-19T10:00:00-04:00"}""",
        )
        assertEquals("group:games", group.subject)
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
        // The payload above predates per-output support, so a device running an
        // older build must still decode — `output` simply stays absent.
        assertNull(volume.output)
    }

    @Test
    fun `volume info names the active output`() {
        val json = """
            {"percent":40,"muted":false,"available":true,"backend":"pipewire",
             "restrictions":{"max_volume":80,"min_volume":null,"allow_mute":true,"allow_change":true},
             "output":{"key":"alsa_card.pci-0000_00_1b.0:output:analog-output-headphones",
                       "description":"Built-in Audio Analog Stereo","kind":"headphones"}}
        """.trimIndent()
        val volume = decode<VolumeInfo>(json)
        val output = volume.output
        assertEquals(
            "alsa_card.pci-0000_00_1b.0:output:analog-output-headphones",
            output?.key,
        )
        assertEquals(AudioOutputKind.HEADPHONES, output?.kind)
    }

    @Test
    fun `audio output record carries its limit and active flag`() {
        val json = """
            {"output":{"key":"alsa_card.usb-Focusrite_Scarlett_2i2_USB-00:output:analog-output",
                       "description":"Focusrite Scarlett 2i2 Analog Stereo","kind":"unknown"},
             "max_volume":50,"min_volume":null,
             "last_seen":"2026-08-21T23:45:19.599464680-04:00","active":true}
        """.trimIndent()
        val record = decode<AudioOutputRecord>(json)
        assertEquals(50L, record.maxVolume)
        assertNull(record.minVolume)
        assertTrue(record.active)
        // A generic USB interface classifies as nothing, which is routine and
        // must not be mistaken for a decode failure.
        assertEquals(AudioOutputKind.UNKNOWN, record.output.kind)
    }

    @Test
    fun `an uncapped audio output decodes with no limit`() {
        val json = """
            {"output":{"key":"k","description":"Speakers","kind":"speakers"},
             "max_volume":null,"min_volume":null,
             "last_seen":"2026-08-21T23:45:19-04:00","active":false}
        """.trimIndent()
        val record = decode<AudioOutputRecord>(json)
        assertNull(record.maxVolume)
        assertEquals(AudioOutputKind.SPEAKERS, record.output.kind)
    }

    @Test
    fun `an audio output row from a daemon without the available field is selectable`() {
        // Older daemon, newer phone: `available` was added with the output
        // picker. Defaulting it to false would grey out the button for every
        // device the parent could actually switch to, so the default is true and
        // a stale daemon simply refuses the call out loud.
        val json = """
            {"output":{"key":"k","description":"Speakers","kind":"speakers"},
             "max_volume":null,"min_volume":null,
             "last_seen":"2026-08-21T23:45:19-04:00","active":false}
        """.trimIndent()
        assertTrue(decode<AudioOutputRecord>(json).available)
    }

    @Test
    fun `an audio output row can say the device is gone`() {
        val json = """
            {"output":{"key":"k","description":"Headphones","kind":"headphones"},
             "max_volume":50,"min_volume":null,
             "last_seen":"2026-08-21T23:45:19-04:00","active":false,"available":false}
        """.trimIndent()
        val record = decode<AudioOutputRecord>(json)
        // The limit outlives the hardware; only the ability to switch to it goes.
        assertEquals(50L, record.maxVolume)
        assertFalse(record.available)
    }

    @Test
    fun `window list decodes both wayland and xwayland shapes`() {
        // Straight from `swaymsg -t get_tree` as the daemon flattens it:
        // a Wayland window names itself with app_id, an XWayland one only
        // has a class, and the scratchpad entry sits on __i3_scratch.
        val json = """
            [
              {"id":10,"name":"Firefox","app_id":"firefox","window_class":null,"pid":1234,
               "in_scratchpad":false,"workspace":"1","visible":true,"focused":true,
               "owner":"activity"},
              {"id":20,"name":"Steam","app_id":null,"window_class":"Steam","pid":5678,
               "in_scratchpad":true,"workspace":"__i3_scratch","visible":false,"focused":false,
               "owner":"shepherd"}
            ]
        """.trimIndent()
        val windows = decode<List<WindowInfo>>(json)
        assertEquals(2, windows.size)
        assertEquals("firefox", windows[0].appId)
        assertNull(windows[0].windowClass)
        assertTrue(windows[0].focused)
        assertEquals(WindowOwner.ACTIVITY, windows[0].owner)
        assertEquals("Steam", windows[1].windowClass)
        assertTrue(windows[1].inScratchpad)
        assertEquals(5678, windows[1].pid)
        assertEquals(WindowOwner.SHEPHERD, windows[1].owner)
    }

    /**
     * The two owners the windows screen exists for. Their spellings are what
     * separates "the child is playing a game" from "something is on screen
     * that no session owns", so a rename on the daemon side has to fail here
     * rather than quietly downgrade every orphan to an ordinary row.
     */
    @Test
    fun `window owner decodes the unsupervised spellings`() {
        val json = """
            [
              {"id":30,"name":"Stubborn","app_id":"org.example.Stubborn","window_class":null,
               "pid":9001,"in_scratchpad":false,"workspace":"1","visible":true,"focused":false,
               "owner":"escaped"},
              {"id":40,"name":"Victim","app_id":"org.example.Victim","window_class":null,
               "pid":9002,"in_scratchpad":false,"workspace":"1","visible":true,"focused":false,
               "owner":"unowned"}
            ]
        """.trimIndent()
        val windows = decode<List<WindowInfo>>(json)
        assertEquals(WindowOwner.ESCAPED, windows[0].owner)
        assertEquals(WindowOwner.UNOWNED, windows[1].owner)
    }

    @Test
    fun `window action serialises to the wire spelling act_on_window expects`() {
        assertEquals("\"close\"", ShepherdJson.encodeToString(WindowAction.serializer(), WindowAction.CLOSE))
        assertEquals("\"hide\"", ShepherdJson.encodeToString(WindowAction.serializer(), WindowAction.HIDE))
        assertEquals("\"show\"", ShepherdJson.encodeToString(WindowAction.serializer(), WindowAction.SHOW))
        assertEquals("\"focus\"", ShepherdJson.encodeToString(WindowAction.serializer(), WindowAction.FOCUS))
    }

    @Test
    fun `a diagnostic code this build predates does not fail the decode`() {
        // The device is the thing that gains variants, and it gains them in
        // exactly the releases a phone has not been updated for. kotlinx's
        // default enum serializer throws on an unrecognised value, and the
        // exception takes down the decode of the whole enclosing response --
        // so before this, one new `DiagnosticCode` made a newer device
        // unreadable to an older companion. Worst possible timing: a
        // diagnostic is reported when something is already wrong.
        //
        // `ignoreUnknownKeys` does not cover it. That forgives an unknown
        // *key*; this is a known key with an unknown *value*.
        val json = """
            {"code":"a_code_from_a_newer_device","message":"something is wrong",
             "remedy":null,"severity":"critical",
             "since":"2026-09-03T00:00:00.000000000-04:00",
             "subject":{"type":"service"}}
        """.trimIndent()

        val diagnostic = decode<Diagnostic>(json)

        assertEquals(DiagnosticCode.UNKNOWN, diagnostic.code)
        // The rest of the payload has to survive, which is the entire point:
        // the message and remedy are written for a person and are readable
        // even when the code is not.
        assertEquals("something is wrong", diagnostic.message)
        assertEquals(DiagnosticSeverity.CRITICAL, diagnostic.severity)
    }

    @Test
    fun `a known enum value still round-trips by its wire name`() {
        // The tolerant serializer replaces the one kotlinx derives from
        // `@SerialName`, so the ordinary case needs holding down too: a
        // mistake there would rename every value on the wire at once.
        val json = """{"code":"state_not_protected","message":"m","remedy":null,
                       "severity":"warning",
                       "since":"2026-09-03T00:00:00.000000000-04:00",
                       "subject":{"type":"service"}}""".trimIndent()

        val diagnostic = decode<Diagnostic>(json)
        assertEquals(DiagnosticCode.STATE_NOT_PROTECTED, diagnostic.code)
        assertEquals(
            "state_not_protected",
            ShepherdJson.encodeToString(DiagnosticCode.serializer(), diagnostic.code)
                .trim('"'),
        )
    }

}
