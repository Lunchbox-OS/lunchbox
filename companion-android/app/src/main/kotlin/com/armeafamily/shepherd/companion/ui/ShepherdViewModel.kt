package com.armeafamily.shepherd.companion.ui

import android.app.Application
import android.os.Build
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.armeafamily.shepherd.companion.appContainer
import com.armeafamily.shepherd.companion.ble.RpcException
import com.armeafamily.shepherd.companion.ble.ShepherdConnection
import com.armeafamily.shepherd.companion.domain.AdminRecord
import com.armeafamily.shepherd.companion.domain.BrightnessInfo
import com.armeafamily.shepherd.companion.domain.ClaimStateTag
import com.armeafamily.shepherd.companion.domain.DailyOverride
import com.armeafamily.shepherd.companion.domain.EntryView
import com.armeafamily.shepherd.companion.domain.EventPayload
import com.armeafamily.shepherd.companion.domain.ManagementClient
import com.armeafamily.shepherd.companion.domain.ServiceStateSnapshot
import com.armeafamily.shepherd.companion.domain.SessionInfo
import com.armeafamily.shepherd.companion.domain.ShepherdRecord
import com.armeafamily.shepherd.companion.domain.UsageStat
import com.armeafamily.shepherd.companion.domain.VolumeInfo
import com.armeafamily.shepherd.companion.ble.Protocol
import com.armeafamily.shepherd.companion.util.ReasonText
import com.juul.kable.State
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** Coarse state of the BLE link to the active device. */
enum class LinkStatus { Idle, Connecting, Connected, Reconnecting, Disconnected, NeedsRepair }

/** Everything the device screens render. */
data class DeviceUiState(
    val record: ShepherdRecord? = null,
    val link: LinkStatus = LinkStatus.Idle,
    val snapshot: ServiceStateSnapshot? = null,
    val currentSession: SessionInfo? = null,
    val volume: VolumeInfo? = null,
    val brightness: BrightnessInfo? = null,
) {
    val entries: List<EntryView> get() = snapshot?.entries.orEmpty()
}

/** Phases of the pairing flow surfaced to the pairing screen. */
sealed interface PairingPhase {
    data object Idle : PairingPhase
    data class Connecting(val deviceName: String?) : PairingPhase

    /** Bond in progress; show the "compare the digits" guidance + [mac]. */
    data class Comparing(val deviceName: String?, val mac: String) : PairingPhase
    data class Claiming(val deviceName: String?) : PairingPhase
    data class Success(val record: ShepherdRecord) : PairingPhase
    data class Failed(val reason: String, val alreadyClaimed: Boolean = false) : PairingPhase
}

/**
 * Owns the live BLE session with the active device and drives pairing.
 *
 * A connection exists only while the UI is bound ([onForeground]) — going
 * to the background tears it down, honouring the project's "no background
 * BLE" constraint. Reconnects are attempted silently a few times with
 * backoff before the link is surfaced as dropped.
 */
class ShepherdViewModel(app: Application) : AndroidViewModel(app) {

    private val container = app.appContainer
    val repository = container.repository

    private val _state = MutableStateFlow(DeviceUiState())
    val state: StateFlow<DeviceUiState> = _state

    private val _pairing = MutableStateFlow<PairingPhase>(PairingPhase.Idle)
    val pairing: StateFlow<PairingPhase> = _pairing

    private val _message = MutableStateFlow<String?>(null)
    val message: StateFlow<String?> = _message

    /** Default name to claim under — the phone's model. */
    val defaultPhoneName: String = Build.MODEL ?: "Android phone"

    private var connection: ShepherdConnection? = null
    private var client: ManagementClient? = null
    private var sessionJob: Job? = null
    private var eventsJob: Job? = null
    private var bound = false

    init {
        viewModelScope.launch {
            repository.load()
            // On a cold start the lifecycle's ON_START (onForeground) can
            // fire before records finish loading, leaving `active` null at
            // that moment. Connect here once the list is in hand.
            if (bound && connection == null) repository.active?.let { connectTo(it) }
        }
    }

    // --- foreground binding -------------------------------------------

    fun onForeground() {
        bound = true
        val record = repository.active ?: return
        if (connection == null) connectTo(record)
    }

    fun onBackground() {
        bound = false
        teardown()
        _state.update { it.copy(link = LinkStatus.Idle) }
    }

    fun selectDevice(identityAddress: String) {
        if (identityAddress == repository.activeAddress.value && connection != null) return
        repository.setActive(identityAddress)
        val record = repository.active ?: return
        if (bound) connectTo(record)
    }

    fun consumeMessage() {
        _message.value = null
    }

    // --- connection lifecycle -----------------------------------------

    private fun connectTo(record: ShepherdRecord) {
        teardown()
        val conn = ShepherdConnection.fromIdentifier(record.androidIdentifier, viewModelScope)
        connection = conn
        client = ManagementClient(conn)
        _state.value = DeviceUiState(record = record, link = LinkStatus.Connecting)
        conn.start()
        eventsJob = viewModelScope.launch {
            conn.events.collect { event -> applyEvent(event.payload) }
        }
        sessionJob = viewModelScope.launch { runConnectionLoop(record, conn) }
    }

    private suspend fun runConnectionLoop(record: ShepherdRecord, conn: ShepherdConnection) {
        val backoffs = longArrayOf(1_000, 2_000, 5_000)
        var failures = 0
        while (viewModelScope.isActive) {
            _state.update { it.copy(link = if (failures == 0) LinkStatus.Connecting else LinkStatus.Reconnecting) }
            try {
                conn.connect()
                failures = 0
                _state.update { it.copy(link = LinkStatus.Connected) }
                refreshAll()
                // Suspend here until the link drops, then loop to reconnect.
                conn.state.first { it is State.Disconnected }
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                // A vanished bond means the device was factory-reset (or
                // claimed by someone else). Don't keep retrying — the user
                // must re-pair.
                if (!container.bondManager.isBonded(record.androidIdentifier)) {
                    _state.update { it.copy(link = LinkStatus.NeedsRepair) }
                    return
                }
                failures++
                if (failures >= backoffs.size) {
                    _state.update { it.copy(link = LinkStatus.Disconnected) }
                    return
                }
                delay(backoffs[failures - 1])
            }
        }
    }

    private fun teardown() {
        sessionJob?.cancel(); sessionJob = null
        eventsJob?.cancel(); eventsJob = null
        connection?.close(); connection = null
        client = null
    }

    /** User-initiated reconnect after the link was surfaced as dropped. */
    fun retryConnection() {
        val record = repository.active ?: return
        connectTo(record)
    }

    // --- refresh + events ---------------------------------------------

    private suspend fun refreshAll() {
        val c = client ?: return
        // service_state populates the entry list — if it fails we used
        // to swallow the error silently, which is exactly what made
        // "the list doesn't appear on reopen" so hard to spot. Surface
        // the message; the device's initial StateChanged push (see
        // shepherd-ble's events_characteristic) is the redundant
        // backup that usually fills the UI in regardless.
        runCatching { c.serviceState() }.fold(
            onSuccess = { snap ->
                _state.update { it.copy(snapshot = snap, currentSession = snap.currentSession) }
            },
            onFailure = { e ->
                _message.value = "Couldn't fetch device state: ${e.message ?: e::class.simpleName}"
            },
        )
        runCatching { c.getVolume() }.onSuccess { v -> _state.update { it.copy(volume = v) } }
        runCatching { c.getBrightness() }.onSuccess { b -> _state.update { it.copy(brightness = b) } }
    }

    private fun applyEvent(payload: EventPayload) {
        when (payload) {
            is EventPayload.StateChanged -> {
                val snap = payload.toSnapshot()
                _state.update { it.copy(snapshot = snap, currentSession = snap.currentSession) }
            }
            is EventPayload.SessionStarted,
            is EventPayload.SessionEnded,
            is EventPayload.SessionExpiring,
            is EventPayload.WarningIssued,
            -> refreshSession()
            is EventPayload.PolicyReloaded,
            is EventPayload.EntryAvailabilityChanged,
            is EventPayload.InternetStatusChanged,
            -> refreshSnapshot()
            is EventPayload.VolumeChanged -> refreshVolume()
            is EventPayload.BrightnessChanged -> refreshBrightness()
            else -> Unit
        }
    }

    private fun refreshSession() = viewModelScope.launch {
        client?.let { c -> runCatching { c.currentSession() }.onSuccess { s -> _state.update { it.copy(currentSession = s) } } }
    }

    private fun refreshSnapshot() = viewModelScope.launch {
        client?.let { c -> runCatching { c.serviceState() }.onSuccess { s -> _state.update { it.copy(snapshot = s, currentSession = s.currentSession) } } }
    }

    private fun refreshVolume() = viewModelScope.launch {
        client?.let { c -> runCatching { c.getVolume() }.onSuccess { v -> _state.update { it.copy(volume = v) } } }
    }

    private fun refreshBrightness() = viewModelScope.launch {
        client?.let { c -> runCatching { c.getBrightness() }.onSuccess { b -> _state.update { it.copy(brightness = b) } } }
    }

    // --- actions -------------------------------------------------------

    private inline fun action(crossinline block: suspend (ManagementClient) -> Unit) {
        val c = client ?: run { _message.value = "Not connected."; return }
        viewModelScope.launch {
            try {
                block(c)
            } catch (e: RpcException) {
                _message.value = ReasonText.describe(e)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                _message.value = e.message ?: "Something went wrong."
            }
        }
    }

    fun launchEntry(id: String) = action { c ->
        val outcome = c.launch(id)
        if (outcome.isApproved) {
            refreshSession()
        } else {
            val reasons = outcome.denied?.reasons.orEmpty()
            _message.value = reasons.firstOrNull()?.let(ReasonText::describe) ?: "Launch denied."
        }
    }

    fun stopCurrent() = action { c -> c.stopCurrent(); refreshSession() }

    fun extendCurrent(seconds: Long) = action { c -> c.extendCurrent(seconds); refreshSession() }

    fun setVolume(percent: Int) = action { c -> _state.update { it.copy(volume = c.setVolume(percent)) } }

    fun setMute(muted: Boolean) = action { c -> _state.update { it.copy(volume = c.setMute(muted)) } }

    fun setBrightness(percent: Int) = action { c -> _state.update { it.copy(brightness = c.setBrightness(percent)) } }

    fun setAutoBrightness(enabled: Boolean) =
        action { c -> _state.update { it.copy(brightness = c.setAutoBrightness(enabled)) } }

    fun reloadConfig() = action { c ->
        val result = c.reloadConfig()
        _message.value = "Config reloaded — ${result.entryCount} entries."
        refreshSnapshot()
    }

    fun logoutDevice() = action { c ->
        c.logout()
        _message.value = "Logged out the device session."
    }

    fun upsertOverride(
        id: String,
        date: String?,
        availability: Boolean?,
        quotaDeltaSeconds: Long?,
        onDone: () -> Unit,
    ) = action { c ->
        c.upsertOverride(id, date, availability, quotaDeltaSeconds)
        _message.value = "Override saved."
        onDone()
    }

    fun deleteOverride(id: String, date: String?, onDone: () -> Unit) = action { c ->
        c.deleteOverride(id, date)
        _message.value = "Override cleared."
        onDone()
    }

    suspend fun loadOverride(id: String, date: String?): DailyOverride? =
        client?.let { runCatching { it.getOverride(id, date) }.getOrNull() }

    suspend fun loadUsage(id: String, from: String, to: String): List<UsageStat> =
        client?.let { runCatching { it.usageEntry(id, from, to) }.getOrDefault(emptyList()) } ?: emptyList()

    // --- factory reset / forget ---------------------------------------

    fun factoryReset(onDone: () -> Unit) = action { c ->
        // Wipe the bond on the device, then drop our local record even if
        // anything downstream fails — the token is dead server-side.
        c.factoryReset()
        val address = _state.value.record?.identityAddress
        teardown()
        if (address != null) repository.remove(address)
        _message.value = "Device unpaired."
        onDone()
    }

    fun forgetDevice(identityAddress: String) {
        viewModelScope.launch {
            if (identityAddress == _state.value.record?.identityAddress) teardown()
            repository.remove(identityAddress)
        }
    }

    fun forgetAll(onDone: () -> Unit) {
        viewModelScope.launch {
            teardown()
            repository.clearAll()
            _state.value = DeviceUiState()
            onDone()
        }
    }

    fun updateNickname(identityAddress: String, nickname: String?) {
        viewModelScope.launch { repository.updateNickname(identityAddress, nickname?.trim()?.ifBlank { null }) }
    }

    // --- pairing -------------------------------------------------------

    fun scan() = container.scanner.scan()

    fun resetPairing() {
        _pairing.value = PairingPhase.Idle
    }

    /**
     * Run the full Numeric Comparison + claim flow against [identifier].
     * On success the new connection is adopted as the active session.
     */
    fun pair(identifier: String, phoneName: String) {
        viewModelScope.launch {
            // The pairing connection becomes the live session on success,
            // so tear down anything currently active first.
            teardown()
            _pairing.value = PairingPhase.Connecting(null)

            val conn = ShepherdConnection.fromIdentifier(identifier, viewModelScope)
            try {
                conn.start()
                conn.connect()
                val info = conn.readDeviceInfo()

                if (info.protocolVersion != Protocol.PROTOCOL_VERSION) {
                    fail(conn, "This device speaks protocol v${info.protocolVersion}; update the app.")
                    return@launch
                }
                if (info.claimState == ClaimStateTag.CLAIMED) {
                    conn.close()
                    _pairing.value = PairingPhase.Failed(
                        "This device is already paired with another phone.",
                        alreadyClaimed = true,
                    )
                    return@launch
                }

                _pairing.value = PairingPhase.Comparing(info.deviceName, identifier)
                val bonded = container.bondManager.ensureBonded(identifier)
                if (!bonded) {
                    fail(conn, "Pairing was cancelled or failed. Try again.")
                    return@launch
                }

                _pairing.value = PairingPhase.Claiming(info.deviceName)
                val admin: AdminRecord = ManagementClient(conn).claim(phoneName)
                val record = admin.toShepherdRecord(androidIdentifier = identifier)
                repository.upsert(record)

                // Adopt this connection as the active session.
                adopt(record, conn)
                _pairing.value = PairingPhase.Success(record)
            } catch (e: RpcException) {
                if (e.code == com.armeafamily.shepherd.companion.ble.ErrorCode.ALREADY_CLAIMED) {
                    conn.close()
                    _pairing.value = PairingPhase.Failed(ReasonText.describe(e), alreadyClaimed = true)
                } else {
                    fail(conn, ReasonText.describe(e))
                }
            } catch (e: CancellationException) {
                conn.close(); throw e
            } catch (e: Exception) {
                fail(conn, e.message ?: "Pairing failed.")
            }
        }
    }

    private fun fail(conn: ShepherdConnection, reason: String) {
        conn.close()
        _pairing.value = PairingPhase.Failed(reason)
    }

    /** Take over an already-connected, claimed connection as the session. */
    private fun adopt(record: ShepherdRecord, conn: ShepherdConnection) {
        connection = conn
        client = ManagementClient(conn)
        _state.value = DeviceUiState(record = record, link = LinkStatus.Connected)
        eventsJob = viewModelScope.launch { conn.events.collect { applyEvent(it.payload) } }
        sessionJob = viewModelScope.launch {
            runCatching { refreshAll() }
            // Watch for drops and reconnect like a normal session.
            runConnectionLoopAfterConnected(record, conn)
        }
    }

    private suspend fun runConnectionLoopAfterConnected(record: ShepherdRecord, conn: ShepherdConnection) {
        conn.state.first { it is State.Disconnected }
        runConnectionLoop(record, conn)
    }

    override fun onCleared() {
        teardown()
        super.onCleared()
    }
}

/** Build a persisted record from the claim result. */
private fun AdminRecord.toShepherdRecord(androidIdentifier: String) = ShepherdRecord(
    identityAddress = identityAddress,
    addressType = addressType,
    deviceName = deviceName,
    bondedAt = bondedAt,
    httpToken = httpToken,
    role = role,
    androidIdentifier = androidIdentifier,
)
