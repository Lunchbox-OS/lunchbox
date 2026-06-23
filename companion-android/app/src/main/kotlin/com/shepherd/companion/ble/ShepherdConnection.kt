@file:OptIn(ExperimentalUuidApi::class)

package com.shepherd.companion.ble

import com.juul.kable.Advertisement
import com.juul.kable.Peripheral
import com.juul.kable.State
import com.juul.kable.WriteType
import com.juul.kable.characteristicOf
import com.shepherd.companion.domain.DeviceInfo
import com.shepherd.companion.domain.Event
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.uuid.ExperimentalUuidApi

/**
 * A live BLE session with one shepherd device.
 *
 * Wraps a Kable [Peripheral] and layers the management protocol on top:
 * MTU-aware chunked writes to the Request characteristic, frame
 * reassembly of Response/Events notifications, request/response
 * correlation by `id`, and a hot [events] stream.
 *
 * Construct via [fromAdvertisement] (pairing / first contact) or
 * [fromIdentifier] (reconnect to a bonded device by MAC). Call [start]
 * once to wire up the notification collectors, then [connect]. The
 * collectors live on the [scope] passed in, so they outlive transient
 * disconnects and resume on reconnect.
 */
class ShepherdConnection private constructor(
    private val peripheral: Peripheral,
    private val scope: CoroutineScope,
) {
    private val requestChar = characteristicOf(Protocol.MANAGEMENT_SERVICE, Protocol.REQUEST_CHAR)
    private val responseChar = characteristicOf(Protocol.MANAGEMENT_SERVICE, Protocol.RESPONSE_CHAR)
    private val eventsChar = characteristicOf(Protocol.MANAGEMENT_SERVICE, Protocol.EVENTS_CHAR)
    private val deviceInfoChar = characteristicOf(Protocol.MANAGEMENT_SERVICE, Protocol.DEVICE_INFO_CHAR)

    private val nextId = AtomicLong(1)
    private val pending = ConcurrentHashMap<Long, CompletableDeferred<RpcResponse>>()

    // Guards the multi-chunk write of a single logical frame so two
    // concurrent calls never interleave their fragments on the wire.
    private val writeMutex = Mutex()

    // Negotiated `MTU - 3`, established after connect. 20 is the BLE 4.0
    // floor and a safe default before negotiation.
    @Volatile
    private var chunkSize: Int = 20

    // replay = 1 so a collector that attaches a tick after a notification
    // arrives still sees that event. Important for the device's
    // "initial StateChanged on every Events subscribe" push (see
    // crates/shepherd-ble/src/server.rs::events_characteristic): the
    // CCCD-enable + first notify can arrive on the BLE thread before
    // ShepherdViewModel.eventsJob has attached, and without replay
    // that snapshot — the one the UI uses to populate its list —
    // would be silently dropped.
    private val _events = MutableSharedFlow<Event>(replay = 1, extraBufferCapacity = 64)
    val events: SharedFlow<Event> = _events

    val state: StateFlow<State> get() = peripheral.state

    /** The peripheral's platform identifier (MAC address on Android). */
    val identifier: String get() = peripheral.identifier.toString()

    private var collectors: List<Job> = emptyList()

    /** Wire up Response/Events collectors. Idempotent; call before [connect]. */
    fun start() {
        if (collectors.isNotEmpty()) return
        val responseAssembler = FrameAssembler()
        val eventsAssembler = FrameAssembler()
        collectors = listOf(
            scope.launch {
                // onSubscription resets the buffer at the start of every
                // (re)connection, dropping any partial frame from before.
                peripheral.observe(responseChar) { responseAssembler.reset() }
                    .collect { bytes ->
                        responseAssembler.push(bytes).forEach(::dispatchResponse)
                    }
            },
            scope.launch {
                peripheral.observe(eventsChar) { eventsAssembler.reset() }
                    .collect { bytes ->
                        eventsAssembler.push(bytes).forEach(::dispatchEvent)
                    }
            },
        )
    }

    /** Connect, discover services, negotiate MTU. */
    suspend fun connect() {
        peripheral.connect()
        chunkSize = runCatching {
            peripheral.maximumWriteValueLengthForType(WriteType.WithoutResponse)
        }.getOrDefault(20).coerceAtLeast(20)
    }

    suspend fun disconnect() = peripheral.disconnect()

    fun close() {
        collectors.forEach(Job::cancel)
        collectors = emptyList()
        pending.values.forEach { it.cancel() }
        pending.clear()
        peripheral.close()
    }

    /** Read the unencrypted DeviceInfo characteristic (raw JSON, unframed). */
    suspend fun readDeviceInfo(): DeviceInfo {
        val bytes = peripheral.read(deviceInfoChar)
        return ShepherdJson.decodeFromString(DeviceInfo.serializer(), bytes.decodeToString())
    }

    /**
     * Send an RPC and await its response. Throws [RpcException] on an
     * error response and [TimeoutCancellationException] if the device
     * does not answer within [REQUEST_TIMEOUT_MS].
     */
    suspend fun call(method: String, params: JsonElement): JsonElement {
        val id = nextId.getAndIncrement()
        val deferred = CompletableDeferred<RpcResponse>()
        pending[id] = deferred
        try {
            val request = RpcRequest(id, method, params)
            val json = ShepherdJson.encodeToString(RpcRequest.serializer(), request)
            val framed = Framing.encode(json.encodeToByteArray())
            val chunks = Framing.chunk(framed, chunkSize)
            writeMutex.withLock {
                for (chunk in chunks) {
                    peripheral.write(requestChar, chunk, WriteType.WithoutResponse)
                }
            }
            val response = withTimeout(REQUEST_TIMEOUT_MS) { deferred.await() }
            response.error?.let { throw RpcException(it.code, it.message) }
            return response.result ?: JsonNull
        } finally {
            pending.remove(id)
        }
    }

    private fun dispatchResponse(frame: ByteArray) {
        val response = runCatching {
            ShepherdJson.decodeFromString(RpcResponse.serializer(), frame.decodeToString())
        }.getOrNull() ?: return
        pending.remove(response.id)?.complete(response)
    }

    private fun dispatchEvent(frame: ByteArray) {
        val event = runCatching {
            ShepherdJson.decodeFromString(Event.serializer(), frame.decodeToString())
        }.getOrNull() ?: return
        _events.tryEmit(event)
    }

    companion object {
        /** Default per-request timeout; BLE round-trips are well under this. */
        const val REQUEST_TIMEOUT_MS: Long = 15_000

        fun fromAdvertisement(advertisement: Advertisement, scope: CoroutineScope): ShepherdConnection =
            ShepherdConnection(
                Peripheral(advertisement) {
                    onServicesDiscovered { requestMtu(Protocol.DESIRED_MTU) }
                },
                scope,
            )

        fun fromIdentifier(identifier: String, scope: CoroutineScope): ShepherdConnection =
            ShepherdConnection(
                Peripheral(identifier) {
                    onServicesDiscovered { requestMtu(Protocol.DESIRED_MTU) }
                },
                scope,
            )
    }
}
