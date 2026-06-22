package com.shepherd.companion.domain

import com.shepherd.companion.persistence.AdminRecordStore
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
 * [AdminRecordStore] and publish to [records] / [activeAddress].
 */
class ShepherdRepository(private val store: AdminRecordStore) {

    private val mutex = Mutex()

    private val _records = MutableStateFlow<List<ShepherdRecord>>(emptyList())
    val records: StateFlow<List<ShepherdRecord>> = _records.asStateFlow()

    private val _activeAddress = MutableStateFlow<String?>(null)
    val activeAddress: StateFlow<String?> = _activeAddress.asStateFlow()

    val active: ShepherdRecord?
        get() = _records.value.firstOrNull { it.identityAddress == _activeAddress.value }

    suspend fun load() = mutex.withLock {
        val loaded = store.load()
        _records.value = loaded
        if (_activeAddress.value == null || loaded.none { it.identityAddress == _activeAddress.value }) {
            _activeAddress.value = loaded.firstOrNull()?.identityAddress
        }
    }

    fun setActive(identityAddress: String) {
        _activeAddress.value = identityAddress
    }

    /** Insert or replace the record for a device, then select it. */
    suspend fun upsert(record: ShepherdRecord) = mutex.withLock {
        val next = _records.value.filter { it.identityAddress != record.identityAddress } + record
        _records.value = next
        _activeAddress.value = record.identityAddress
        store.save(next)
    }

    suspend fun updateNickname(identityAddress: String, nickname: String?) = mutex.withLock {
        val next = _records.value.map {
            if (it.identityAddress == identityAddress) it.copy(nickname = nickname) else it
        }
        _records.value = next
        store.save(next)
    }

    /** Forget one device locally. Does not call `factory_reset` on it. */
    suspend fun remove(identityAddress: String) = mutex.withLock {
        val next = _records.value.filter { it.identityAddress != identityAddress }
        _records.value = next
        if (_activeAddress.value == identityAddress) {
            _activeAddress.value = next.firstOrNull()?.identityAddress
        }
        store.save(next)
    }

    /** Forget every device locally (the "I lost my devices" recovery). */
    suspend fun clearAll() = mutex.withLock {
        _records.value = emptyList()
        _activeAddress.value = null
        store.save(emptyList())
    }
}
