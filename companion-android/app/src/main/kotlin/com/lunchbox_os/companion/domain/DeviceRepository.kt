package com.lunchbox_os.companion.domain

import com.lunchbox_os.companion.persistence.AdminRecordStore
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * The set of bonded devices and which one is currently selected.
 *
 * Switching the active device only changes which BLE peer the app talks
 * to; there is no shared state across devices. Mutations persist through
 * [AdminRecordStore] and publish to [records] / [activeId].
 */
class DeviceRepository(private val store: AdminRecordStore) {

    private val mutex = Mutex()

    private val _records = MutableStateFlow<List<DeviceRecord>>(emptyList())
    val records: StateFlow<List<DeviceRecord>> = _records.asStateFlow()

    private val _activeId = MutableStateFlow<String?>(null)

    /**
     * The [DeviceRecord.androidIdentifier] of the selected device.
     *
     * Devices are keyed by that — the peer's BLE MAC — and *not* by
     * `identityAddress`. The latter is whatever address the daemon
     * recorded at claim time, which mid-pairing is the phone's
     * resolvable private address: it differs on every re-pair, so keying
     * on it made [upsert] append a second record for a device already in
     * the list. Re-pairing one device three times left three entries, all
     * for the same box, each with a token only one of them could use.
     */
    val activeId: StateFlow<String?> = _activeId.asStateFlow()

    val active: DeviceRecord?
        get() = _records.value.firstOrNull { it.androidIdentifier == _activeId.value }

    suspend fun load() = mutex.withLock {
        // Collapse any duplicates an older build already persisted,
        // keeping the newest record per device (the last one written has
        // the token that matches the live bond).
        val loaded = store.load()
        val deduped = loaded.associateBy { it.androidIdentifier }.values.toList()
        if (deduped.size != loaded.size) store.save(deduped)
        _records.value = deduped
        if (_activeId.value == null || deduped.none { it.androidIdentifier == _activeId.value }) {
            _activeId.value = deduped.firstOrNull()?.androidIdentifier
        }
    }

    fun setActive(androidIdentifier: String) {
        _activeId.value = androidIdentifier
    }

    /** Insert or replace the record for a device, then select it. */
    suspend fun upsert(record: DeviceRecord) = mutex.withLock {
        val next = _records.value.filter { it.androidIdentifier != record.androidIdentifier } + record
        _records.value = next
        _activeId.value = record.androidIdentifier
        store.save(next)
    }

    suspend fun updateNickname(androidIdentifier: String, nickname: String?) = mutex.withLock {
        val next = _records.value.map {
            if (it.androidIdentifier == androidIdentifier) it.copy(nickname = nickname) else it
        }
        _records.value = next
        store.save(next)
    }

    /** Forget one device locally. Does not call `factory_reset` on it. */
    suspend fun remove(androidIdentifier: String) = mutex.withLock {
        val next = _records.value.filter { it.androidIdentifier != androidIdentifier }
        _records.value = next
        if (_activeId.value == androidIdentifier) {
            _activeId.value = next.firstOrNull()?.androidIdentifier
        }
        store.save(next)
    }

    /** Forget every device locally (the "I lost my devices" recovery). */
    suspend fun clearAll() = mutex.withLock {
        _records.value = emptyList()
        _activeId.value = null
        store.save(emptyList())
    }
}
