package com.armeafamily.shepherd.companion.domain

import com.armeafamily.shepherd.companion.persistence.AdminRecordStore
import io.mockk.coEvery
import io.mockk.coJustRun
import io.mockk.coVerify
import io.mockk.mockk
import io.mockk.slot
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * Devices are keyed by [ShepherdRecord.androidIdentifier] — the peer's
 * BLE MAC — and never by `identityAddress`.
 *
 * `identityAddress` is whatever address the daemon recorded at claim
 * time, and mid-pairing that is the phone's *resolvable private
 * address*: a different value on every pairing. Keying on it made the
 * device list grow by one entry per re-pair, each carrying a token only
 * one of them could still use, all rendered with the same name. These
 * tests pin the stable key so that can't come back.
 */
class ShepherdRepositoryTest {

    private fun record(
        androidIdentifier: String,
        identityAddress: String,
        deviceName: String = "shepherd",
        nickname: String? = null,
    ) = ShepherdRecord(
        identityAddress = identityAddress,
        addressType = "public",
        deviceName = deviceName,
        bondedAt = "2026-08-10T00:00:00Z",
        httpToken = "token-$identityAddress",
        role = "admin",
        androidIdentifier = androidIdentifier,
        nickname = nickname,
    )

    private fun storeWith(initial: List<ShepherdRecord>): AdminRecordStore =
        mockk<AdminRecordStore>().also {
            coEvery { it.load() } returns initial
            coJustRun { it.save(any()) }
        }

    @Test
    fun `re-pairing the same device replaces its record even though the RPA changed`() = runTest {
        val store = storeWith(listOf(record("AA:BB:CC:DD:EE:FF", "44:64:95:72:FC:B7")))
        val repo = ShepherdRepository(store)
        repo.load()

        // Same physical device (same MAC), new claim-time address.
        repo.upsert(record("AA:BB:CC:DD:EE:FF", "4B:D8:E3:93:8B:05"))

        assertEquals(1, repo.records.value.size)
        assertEquals("4B:D8:E3:93:8B:05", repo.records.value.single().identityAddress)
        assertEquals("AA:BB:CC:DD:EE:FF", repo.activeId.value)
    }

    @Test
    fun `a genuinely different device is added rather than replacing`() = runTest {
        val store = storeWith(listOf(record("AA:BB:CC:DD:EE:FF", "44:64:95:72:FC:B7")))
        val repo = ShepherdRepository(store)
        repo.load()

        repo.upsert(record("11:22:33:44:55:66", "4B:D8:E3:93:8B:05"))

        assertEquals(2, repo.records.value.size)
        assertEquals("11:22:33:44:55:66", repo.activeId.value)
    }

    @Test
    fun `load collapses duplicates an older build persisted, keeping the newest`() = runTest {
        // What a pre-fix install looks like: one device, four re-pairs.
        val dupes = listOf(
            record("AA:BB:CC:DD:EE:FF", "58:AE:E5:03:C5:48"),
            record("AA:BB:CC:DD:EE:FF", "46:C2:B5:1E:48:E1"),
            record("AA:BB:CC:DD:EE:FF", "4B:D8:E3:93:8B:05"),
            record("AA:BB:CC:DD:EE:FF", "44:64:95:72:FC:B7"),
        )
        val store = storeWith(dupes)
        val repo = ShepherdRepository(store)

        repo.load()

        assertEquals(1, repo.records.value.size)
        // The last one written is the one whose token matches the live bond.
        assertEquals("44:64:95:72:FC:B7", repo.records.value.single().identityAddress)
        // and the collapse is persisted, not just applied in memory
        val saved = slot<List<ShepherdRecord>>()
        coVerify { store.save(capture(saved)) }
        assertEquals(1, saved.captured.size)
    }

    @Test
    fun `load leaves a clean list untouched`() = runTest {
        val clean = listOf(
            record("AA:BB:CC:DD:EE:FF", "58:AE:E5:03:C5:48"),
            record("11:22:33:44:55:66", "46:C2:B5:1E:48:E1"),
        )
        val store = storeWith(clean)
        val repo = ShepherdRepository(store)

        repo.load()

        assertEquals(2, repo.records.value.size)
        // No rewrite when there was nothing to collapse.
        coVerify(exactly = 0) { store.save(any()) }
    }

    @Test
    fun `nickname and removal key on the device, not the claim-time address`() = runTest {
        val store = storeWith(
            listOf(
                record("AA:BB:CC:DD:EE:FF", "58:AE:E5:03:C5:48"),
                record("11:22:33:44:55:66", "46:C2:B5:1E:48:E1"),
            ),
        )
        val repo = ShepherdRepository(store)
        repo.load()

        repo.updateNickname("AA:BB:CC:DD:EE:FF", "Kid's room")
        assertEquals("Kid's room", repo.records.value.first { it.androidIdentifier == "AA:BB:CC:DD:EE:FF" }.nickname)
        assertEquals("Kid's room", repo.records.value.first { it.androidIdentifier == "AA:BB:CC:DD:EE:FF" }.displayName)

        repo.remove("AA:BB:CC:DD:EE:FF")
        assertEquals(1, repo.records.value.size)
        assertEquals("11:22:33:44:55:66", repo.records.value.single().androidIdentifier)
        // the active selection follows the removal
        assertEquals("11:22:33:44:55:66", repo.activeId.value)
    }

    @Test
    fun `displayName falls back to the device name, which is the device's own`() {
        // Regression guard for records labelled with the admin phone's
        // name: `deviceName` must come from DeviceInfo, so the picker
        // shows the box, not the phone that claimed it.
        val r = record("AA:BB:CC:DD:EE:FF", "58:AE:E5:03:C5:48", deviceName = "shepherd")
        assertEquals("shepherd", r.displayName)
        assertEquals("Playroom", r.copy(nickname = "Playroom").displayName)
        assertEquals("shepherd", r.copy(nickname = "  ").displayName)
    }
}
