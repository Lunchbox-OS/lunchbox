@file:OptIn(ExperimentalUuidApi::class)

package com.armeafamily.shepherd.companion.ble

import android.util.Log
import com.juul.kable.Advertisement
import com.juul.kable.Characteristic
import com.juul.kable.Peripheral
import com.juul.kable.State
import com.juul.kable.WriteType
import com.juul.kable.characteristicOf
import com.armeafamily.shepherd.companion.domain.DeviceInfo
import com.armeafamily.shepherd.companion.domain.Event
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.uuid.ExperimentalUuidApi

/**
 * A live BLE session with one shepherd device.
 *
 * Wraps a Kable [Peripheral] and layers the management protocol on top:
 * MTU-aware chunked writes to the Request characteristic, **read-poll
 * draining** of the Response and Events characteristics, frame
 * reassembly, request/response correlation by `id`, and a hot
 * [events] stream.
 *
 * Why read-poll instead of notify: notify was the original design but
 * proved unreliable across bonded reconnects. BlueZ caches CCCD state
 * at the bond, Android short-circuits subsequent CCCD writes from a
 * bonded peer, and the server-side notify task spawned for the *first*
 * session is the only one the wire ever reaches — every later reopen
 * of the companion silently loses responses (manifesting as the 15s
 * `service_state` timeout). Read-poll has no CCCD machinery to get
 * stuck on: the client just reads the characteristic and the server
 * drains its byte queue (see `crates/shepherd-ble/src/outbox.rs`).
 *
 * Construct via [fromAdvertisement] (pairing / first contact) or
 * [fromIdentifier] (reconnect to a bonded device by MAC). Call [start]
 * once to wire up the polling loops, then [connect]. The pollers live
 * on the [scope] passed in, so they outlive transient disconnects and
 * resume on reconnect.
 */
/**
 * The GATT link connected but the bonded, encrypted channel is not
 * usable — the peer likely forgot its side of the bond (e.g. after a
 * device factory reset that called BlueZ `remove_device`) while Android
 * still lists us as bonded. Distinct from an ordinary connect failure so
 * the reconnect loop can drop the stale bond and prompt a re-pair rather
 * than spin on a link that will never carry an RPC.
 */
class LinkUnauthenticatedException(cause: Throwable) :
    Exception("bonded link is not usable; the peer may have forgotten the bond", cause)

/** An in-flight [ShepherdConnection.call] failed because the link dropped. */
class ConnectionDroppedException : Exception("BLE link dropped before the response arrived")

/**
 * [ShepherdConnection.connect]'s post-connect drain ran out of budget
 * before the device's outbox went quiet.
 *
 * The drain used to be unbounded — read until two consecutive empties,
 * no timeout, no read cap, no delay. Because the daemon queues events
 * whether or not anyone is connected, a reconnect after a long
 * heavy-activity session had to chew through a full outbox at 512 bytes
 * per GATT round trip; and once events arrived faster than the drain
 * consumed them, "two consecutive empties" never happened and `connect()`
 * never returned. The UI sat on `Connecting` forever with nothing logged
 * on either side. See
 * `docs/ai/history/2026-08-01 001 ble-connect-drain-unbounded.md`.
 *
 * Recovery is to drop the link rather than press on: the daemon clears
 * both outboxes on peer disconnect, so the caller's next attempt starts
 * from an empty queue.
 */
class DrainStalledException(label: String, bytes: Int, reads: Int) : Exception(
    "$label outbox did not go quiet within the connect budget " +
        "($bytes stale bytes over $reads reads)",
)

/**
 * [ShepherdConnection.connect] exceeded its caller's wall-clock budget.
 *
 * `connect()` has several unbounded-by-nature steps (GATT connect,
 * discovery, MTU negotiation, the post-connect drain), and the 15 s
 * per-RPC timeout covers none of them — so a stall here used to be
 * *unbounded*, not merely slow. Callers wrap it and surface this instead,
 * which routes into the ordinary connect-retry backoff.
 */
class ConnectTimeoutException(budgetMs: Long) :
    Exception("connect did not complete within ${budgetMs}ms")

/**
 * A poll read that never came back.
 *
 * Distinct from a read that *failed*: `BluetoothGatt.readCharacteristic`
 * returning false (the characteristic is gone after the peer's GATT
 * server restarted) delivers no callback at all, so kable's `read()`
 * suspends forever rather than throwing. Nothing above notices, because
 * a coroutine parked mid-read raises no error and changes no state.
 */
class ReadTimeoutException(budgetMs: Long) :
    Exception("characteristic read did not complete within ${budgetMs}ms")

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

    // Conflated wake channel for the response poller. `call()` sends
    // here right before awaiting a response so the poller drops back
    // to the fast interval immediately instead of finishing its
    // current backoff sleep.
    private val responseWake = Channel<Unit>(Channel.CONFLATED)

    // True after `connect()` has finished its post-connect drain;
    // false again on every disconnect. Both pollers and `call()` gate
    // on this — without it, the post-connect drain races the very
    // first RPC's response and silently eats it (the 1108-byte
    // `service_state` snapshot that drove the original "list blank
    // on reopen" symptom).
    private val ready = MutableStateFlow(false)

    // replay = 1 so a collector that attaches a tick after an event
    // arrives still sees that event — e.g. ShepherdViewModel.eventsJob
    // is launched immediately after `connect()`, but the events poller
    // may dispatch a snapshot in the same instant.
    private val _events = MutableSharedFlow<Event>(replay = 1, extraBufferCapacity = 64)
    val events: SharedFlow<Event> = _events

    val state: StateFlow<State> get() = peripheral.state

    /**
     * Suspend until this session stops being usable, however that happens.
     *
     * A full [State.Disconnected] is *not* the only way a session ends,
     * and on the failure that matters most it never arrives. When the
     * daemon restarts, BlueZ keeps the ACL link up and only the GATT
     * application goes away: the peripheral stays `Connected`, service
     * re-discovery finds the characteristics gone ("failed to find
     * characteristic"), and the state mirror above clears [ready] on the
     * transient blip. Nothing sets `ready` true again except [connect],
     * so the pollers park, no read is ever attempted, and a caller
     * waiting on `Disconnected` waits forever — the app sits on stale
     * data indefinitely with no error and no retry.
     *
     * A cleared `ready` under a live link therefore *is* the end of the
     * session: it means the pollers have stopped and only a fresh
     * `connect()` can restart them.
     */
    suspend fun awaitSessionEnd() {
        merge(
            peripheral.state.filter { it is State.Disconnected },
            ready.filter { !it },
        ).first()
    }

    /** The peripheral's platform identifier (MAC address on Android). */
    val identifier: String get() = peripheral.identifier.toString()

    private var pollers: List<Job> = emptyList()

    // close() is idempotent — teardown paths (give-up, concurrent
    // pairing/session ownership) can call it more than once.
    @Volatile
    private var closed = false

    /** Wire up Response/Events poll loops. Idempotent; call before [connect]. */
    fun start() {
        if (pollers.isNotEmpty()) return
        pollers = listOf(
            // Mirror peripheral state into `ready`. Any non-Connected
            // state flips it false so a stale `ready=true` from a
            // previous session can't let the pollers (or call()) skip
            // the next post-connect drain.
            scope.launch {
                peripheral.state.collect { s ->
                    if (s !is State.Connected) {
                        ready.value = false
                        // Fail any RPC awaiting a response now instead of
                        // letting it hang for the full REQUEST_TIMEOUT_MS.
                        // A transient reconnect reuses this instance and
                        // never calls close(), so this is the only place
                        // in-flight calls get released on a drop.
                        failPending()
                    }
                }
            },
            scope.launch {
                pollLoop(
                    label = "response",
                    char = responseChar,
                    wake = responseWake,
                    dispatch = ::dispatchResponse,
                )
            },
            scope.launch {
                pollLoop(
                    label = "events",
                    char = eventsChar,
                    wake = null,
                    dispatch = ::dispatchEvent,
                )
            },
        )
    }

    /**
     * Read-poll loop. Waits for [State.Connected], drains any leftover
     * bytes from a prior session (server-side queue persists across
     * BLE reconnects), then reads until disconnect with adaptive
     * back-off. Loops back to waiting for Connected on disconnect.
     *
     * @param wake optional conflated channel that lets a caller
     *   short-circuit the current backoff sleep — used to drop back
     *   to the fast interval the instant an RPC is dispatched.
     */
    private suspend fun pollLoop(
        label: String,
        char: Characteristic,
        wake: Channel<Unit>?,
        dispatch: (ByteArray) -> Unit,
    ) {
        val assembler = FrameAssembler()
        Log.i(TAG, "$label poller starting; awaiting ready")
        while (currentCoroutineContext().isActive) {
            // Wait for `ready` — that's connect()'s signal that the
            // post-connect drain finished and the outbox is now
            // aligned to a frame boundary. Polling before this point
            // would race with the drain (the original "ate the
            // service_state response" bug).
            try {
                ready.first { it }
            } catch (e: CancellationException) {
                throw e
            }
            // Drain assembler / wake state from any prior session
            // before re-entering the main loop; on the very first
            // pass these are already empty.
            assembler.reset()
            while (wake?.tryReceive()?.isSuccess == true) { /* drain */ }
            Log.i(TAG, "$label poller entering main loop")

            var currentDelayMs = INITIAL_POLL_DELAY_MS
            var totalReads = 0
            var emptyReads = 0
            var consecutiveFailures = 0
            while (currentCoroutineContext().isActive && ready.value) {
                // Bound the read. `peripheral.read` can never return: when
                // the daemon restarts, BlueZ keeps the ACL link up and only
                // unregisters the GATT application, so Android re-discovers,
                // finds the characteristic gone, and `readCharacteristic()`
                // returns false — no callback is ever delivered and kable
                // awaits it forever. The poller then blocks mid-read with
                // `ready` still true, so nothing throws, nothing logs,
                // no state changes, and the session loop's wait for the
                // link to end never fires. The app sits on stale data
                // indefinitely. A read that outlives this budget is dead,
                // not slow; treat it as a failure and let the consecutive
                // -failure rule below force the reconnect.
                var failure: Throwable? = null
                val bytes = try {
                    withTimeoutOrNull(POLL_READ_TIMEOUT_MS) { peripheral.read(char) }
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    failure = e
                    null
                }
                // null == the read threw (failure set) or outran its budget.
                if (bytes == null) {
                    val e = failure ?: ReadTimeoutException(POLL_READ_TIMEOUT_MS)
                    // Disconnect, MTU change mid-read, etc. A bare `break`
                    // here used to fall straight back to the outer
                    // `ready.first { it }`, which returns *instantly* while
                    // `ready` is still true — so a Connected-but-unreadable
                    // link (kable reports Connected while every read throws)
                    // spun this loop at millions of iterations/sec, pegging
                    // the (main-thread) dispatcher and starving the very
                    // state collector that would flip `ready` false. Never
                    // hot-loop: back off between failures (which also yields
                    // the dispatcher), and after enough consecutive failures
                    // force a real disconnect so the reconnect loop rebuilds
                    // the session instead of retrying a dead link forever.
                    consecutiveFailures++
                    Log.w(
                        TAG,
                        "$label read failed after $totalReads reads " +
                            "($emptyReads empty; $consecutiveFailures consecutive)",
                        e,
                    )
                    if (consecutiveFailures >= MAX_CONSECUTIVE_READ_FAILURES) {
                        Log.w(TAG, "$label: forcing reconnect after $consecutiveFailures consecutive read failures")
                        scope.launch { runCatching { peripheral.disconnect() } }
                        break
                    }
                    delay(currentDelayMs.toLong())
                    currentDelayMs = (currentDelayMs * 2).coerceAtMost(MAX_POLL_DELAY_MS)
                    continue
                }
                consecutiveFailures = 0
                totalReads++

                if (bytes.isNotEmpty()) {
                    Log.d(TAG, "$label read returned ${bytes.size}B")
                    runCatching { assembler.push(bytes) }
                        .onSuccess { frames -> frames.forEach(dispatch) }
                        .onFailure(::handleFramingFailure)
                    currentDelayMs = INITIAL_POLL_DELAY_MS
                    // No delay — keep draining while data is flowing.
                    continue
                }
                emptyReads++

                // Empty read: idle. Wait for the next backoff tick or
                // a wake signal, whichever comes first.
                val woken = if (wake != null) {
                    withTimeoutOrNull(currentDelayMs.toLong()) { wake.receive() }
                } else {
                    delay(currentDelayMs.toLong())
                    null
                }
                currentDelayMs = if (woken != null) {
                    INITIAL_POLL_DELAY_MS
                } else {
                    (currentDelayMs * 2).coerceAtMost(MAX_POLL_DELAY_MS)
                }
            }
        }
    }

    /**
     * Read the characteristic to quiescence, discarding whatever comes
     * back, so the pollers start on a frame boundary.
     *
     * Bounded on two axes — [DRAIN_BUDGET_MS] and [MAX_DRAIN_READS] —
     * because "read until two consecutive empties" is not a terminating
     * condition when the device is queueing events faster than a
     * 512-byte-per-round-trip drain consumes them. Exceeding either
     * budget throws [DrainStalledException]; see that class for why
     * failing is better than proceeding.
     */
    private suspend fun drainAndDiscard(label: String, char: Characteristic) {
        var consecutiveEmpty = 0
        var totalDropped = 0
        var reads = 0

        // The first read gets its own, much longer budget — see
        // [readAfterSettle]. Timing it against DRAIN_BUDGET_MS conflates
        // "the device won't stop talking" with "the link isn't encrypted
        // yet", and the latter is the normal state of affairs one read
        // into a fresh session with a bonded peer.
        val firstBytes = readAfterSettle(label, char)
        reads++
        if (firstBytes.isEmpty()) consecutiveEmpty++ else totalDropped += firstBytes.size

        val wentQuiet = withTimeoutOrNull(DRAIN_BUDGET_MS) {
            while (consecutiveEmpty < 2 && reads < MAX_DRAIN_READS) {
                // Read failures propagate to connect(): the Response/Events
                // characteristics require an authenticated-encrypted link, so
                // a *throw* here (as opposed to an empty read) is the signal
                // that the bonded link isn't actually usable. connect() turns
                // it into a LinkUnauthenticatedException. withTimeoutOrNull
                // only swallows its own timeout, so that path is unchanged.
                val bytes = peripheral.read(char)
                reads++
                if (bytes.isEmpty()) {
                    consecutiveEmpty++
                } else {
                    consecutiveEmpty = 0
                    totalDropped += bytes.size
                }
            }
            consecutiveEmpty >= 2
        }
        if (totalDropped > 0) {
            Log.i(TAG, "$label drained $totalDropped stale bytes over $reads reads on (re)connect")
        }
        if (wentQuiet != true) throw DrainStalledException(label, totalDropped, reads)
    }

    /**
     * The session's first read on an encrypted characteristic, given room
     * to warm the link up.
     *
     * Reconnecting to a bonded peer produces a GATT link that is *not yet
     * encrypted*. Android establishes encryption from the stored LTK
     * lazily — triggered by the first ATT request that needs it, which is
     * precisely this read. So this one read can take seconds, and can
     * fail outright (insufficient authentication/encryption) while the
     * handshake is still in flight, even though the bond is perfectly
     * healthy and the very next attempt will succeed.
     *
     * Retrying here is what makes reconnect-after-restart work. Without
     * it, a first read that's merely slow reads as a stalled drain, and
     * one that fails reads as a broken bond — and the caller's recovery
     * for those two is disconnect-and-retry and "your bond is dead",
     * neither of which is what a warming link needs.
     */
    private suspend fun readAfterSettle(label: String, char: Characteristic): ByteArray {
        var lastError: Throwable? = null
        repeat(LINK_SETTLE_ATTEMPTS) { attempt ->
            val bytes = try {
                withTimeoutOrNull(LINK_SETTLE_TIMEOUT_MS) { peripheral.read(char) }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                lastError = e
                null
            }
            if (bytes != null) {
                if (attempt > 0) Log.i(TAG, "$label: link settled after ${attempt + 1} attempts")
                return bytes
            }
            Log.i(
                TAG,
                "$label: first read did not land while the link warms up " +
                    "(attempt ${attempt + 1}/$LINK_SETTLE_ATTEMPTS): ${lastError?.message ?: "timed out"}",
            )
            delay(LINK_SETTLE_RETRY_MS)
        }
        // Out of attempts. A recorded throwable is the honest signal —
        // connect() maps it to LinkUnauthenticatedException, which is the
        // right reading once retries are exhausted. Pure timeouts with no
        // error surface as a stall instead.
        throw lastError ?: DrainStalledException(label, 0, LINK_SETTLE_ATTEMPTS)
    }

    /**
     * A framing-level error means the stream is desynchronised
     * (corrupt length prefix or a server frame larger than [Protocol.MAX_FRAME_BYTES]).
     * Disconnect so the [ShepherdViewModel] reconnect loop can rebuild
     * a clean session.
     */
    private fun handleFramingFailure(error: Throwable) {
        if (error is FramingException) {
            scope.launch { runCatching { peripheral.disconnect() } }
        } else {
            throw error
        }
    }

    /**
     * Connect, discover services, negotiate MTU, then synchronously drain
     * any stale bytes the server may have buffered before flipping
     * [ready].
     *
     * The drain is the key sequencing point: the server-side outbox
     * persists across BLE disconnects, so a reopen can find leftover bytes
     * (a half-delivered response, stale events) at the head of the queue.
     * Discarding them here, before [ready] flips true and the pollers (or
     * any user-side [call]) start consuming, guarantees the first frame the
     * assembler sees starts at a real frame boundary.
     *
     * @param probeEncryptedLink when true (reconnect to a bonded device), a
     *   failure draining the encrypted Response/Events characteristics
     *   fails the whole connect (as [LinkUnauthenticatedException]) rather
     *   than declaring a dead link "ready". The caller treats this like any
     *   other connect failure — it is NOT on its own proof of a lost bond
     *   (a running-but-serviceless or one-sided peer both fail here), so
     *   recovery is left to the caller's scan-based give-up logic. When
     *   false (initial pairing, before the bond exists) the encrypted chars
     *   aren't reachable yet, so drain failures are swallowed and the caller
     *   proceeds to bond + claim.
     */
    suspend fun connect(probeEncryptedLink: Boolean = true) {
        ready.value = false
        peripheral.connect()
        chunkSize = runCatching {
            peripheral.maximumWriteValueLengthForType(WriteType.WithoutResponse)
        }.getOrDefault(20).coerceAtLeast(20)
        if (!probeEncryptedLink) {
            // Pre-bond (pairing). Don't touch the encrypted characteristics
            // at all: there is nothing queued to drain on a link that has
            // never carried a session, and reading them here is actively
            // harmful. On Android an ATT read of an encrypt-authenticated
            // characteristic *initiates bonding*, so draining here starts
            // the OS numeric-comparison flow before the pairing screen has
            // shown the user what to compare — and with the settle retries
            // it can sit through the entire handshake, spending most of
            // the caller's connect budget. Anything stale that somehow
            // survives is cleared server-side by the `id == 1` sentinel on
            // the claim RPC, which is the first thing pairing sends.
            ready.value = true
            Log.i(TAG, "connect: ready (pre-bond; skipped the encrypted drain)")
            return
        }
        // A successful GATT connect does NOT prove the encrypted service is
        // usable (bond intact + serving). On reconnect a throw here fails
        // the connect so the loop retries / gives up cleanly, instead of
        // flipping ready on a link that can't actually carry RPCs.
        try {
            drainAndDiscard("response", responseChar)
            drainAndDiscard("events", eventsChar)
        } catch (e: CancellationException) {
            throw e
        } catch (e: DrainStalledException) {
            // Don't flip `ready` on a stream we're stranded mid-frame in:
            // the pollers would hand the assembler a desynced byte stream
            // and we'd trade a bounded retry for a framing-error loop.
            // Dropping the link makes the daemon clear both outboxes
            // (see `disconnect_monitor` in shepherd-ble), so the caller's
            // retry starts clean. Deliberately NOT wrapped as
            // LinkUnauthenticatedException: the link is fine, it's the
            // backlog that isn't, and conflating the two would point
            // recovery at the bond.
            Log.w(TAG, "connect: ${e.message}; dropping the link so the device clears its outboxes")
            runCatching { peripheral.disconnect() }
            throw e
        } catch (e: Throwable) {
            if (probeEncryptedLink) throw LinkUnauthenticatedException(e)
            Log.i(TAG, "connect: pre-bond drain failed (expected during pairing): ${e.message}")
        }
        ready.value = true
        Log.i(TAG, "connect: ready")
    }

    suspend fun disconnect() = peripheral.disconnect()

    fun close() {
        if (closed) return
        closed = true
        pollers.forEach(Job::cancel)
        pollers = emptyList()
        pending.values.forEach { it.cancel() }
        pending.clear()
        peripheral.close()
    }

    /**
     * Complete every in-flight [call] with [ConnectionDroppedException].
     * Invoked on any disconnect so a pending RPC fails promptly rather
     * than blocking on `deferred.await()` until [REQUEST_TIMEOUT_MS].
     */
    private fun failPending() {
        if (pending.isEmpty()) return
        val dropped = ConnectionDroppedException()
        for (id in pending.keys.toList()) {
            pending.remove(id)?.completeExceptionally(dropped)
        }
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
            // Kick the response poller out of any current backoff so
            // the reply lands at the fast interval, not after the
            // backoff completes.
            responseWake.trySend(Unit)
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
        }.getOrElse {
            Log.w(TAG, "dispatch: failed to parse response frame (${frame.size}B): ${frame.decodeToString().take(200)}")
            return
        }
        val deferred = pending.remove(response.id)
        Log.i(
            TAG,
            "dispatch: response id=${response.id} ok=${response.error == null} matched=${deferred != null}",
        )
        deferred?.complete(response)
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

        private const val TAG = "ShepherdBle"

        /**
         * Fast-path polling interval: how long the poll loop waits
         * between reads after the last one was empty (and we don't
         * have a fresh wake signal). Picked to land just above the
         * typical BLE connection interval so we don't burn battery
         * polling faster than the link can actually deliver new
         * bytes.
         */
        private const val INITIAL_POLL_DELAY_MS: Int = 25

        /**
         * Backoff ceiling for the idle poll. Bounds worst-case event
         * latency to ~this many ms when the user is just sitting on
         * the screen and nothing is happening server-side.
         */
        private const val MAX_POLL_DELAY_MS: Int = 300

        /**
         * Budget for a single poll read. A healthy read is a few tens of
         * ms, so this is generous by two orders of magnitude — it exists
         * to catch reads that will *never* complete (see
         * [ReadTimeoutException]), not slow ones, and is deliberately far
         * above any plausible congested-radio latency so a busy link is
         * never mistaken for a dead one. With
         * [MAX_CONSECUTIVE_READ_FAILURES] this bounds how long a silently
         * dead session can masquerade as live at roughly 25 s.
         */
        private const val POLL_READ_TIMEOUT_MS: Long = 5_000

        /**
         * Consecutive failed reads before the poll loop stops retrying
         * and forces a real disconnect so the reconnect loop can rebuild
         * the session. Guards against a *Connected-but-unreadable* link —
         * kable reporting [State.Connected] while every `read` throws
         * `NotConnectedException` — which the reader must not sit on
         * forever. With the adaptive back-off this is ~1.5 s of retries
         * before giving up, comfortably longer than any transient
         * mid-read hiccup (an MTU renegotiation, a single dropped PDU).
         */
        private const val MAX_CONSECUTIVE_READ_FAILURES: Int = 5

        /**
         * Wall-clock budget for draining one characteristic in
         * [connect], measured from *after* the first read lands (see
         * [readAfterSettle]) so link setup isn't charged against it.
         * Reads cost a GATT round trip and return at most 512 bytes, so
         * this covers tens of KiB of steady-state drain — comfortably
         * more than the device's whole events outbox now that snapshots
         * coalesce, and short enough that a stall surfaces as a retry
         * instead of a hang.
         */
        private const val DRAIN_BUDGET_MS: Long = 5_000

        /**
         * Budget for a single attempt at the session's first read, which
         * is what drives Android to encrypt a freshly reconnected link
         * from the stored LTK. Generous on purpose: this is a handshake,
         * not a queue drain.
         */
        private const val LINK_SETTLE_TIMEOUT_MS: Long = 5_000

        /**
         * Attempts at that first read before concluding the link really
         * is unusable. Encryption setup routinely fails the request
         * that triggered it and succeeds on the retry.
         */
        private const val LINK_SETTLE_ATTEMPTS: Int = 3

        /** Pause between link-settle attempts. */
        private const val LINK_SETTLE_RETRY_MS: Long = 250

        /**
         * Hard read cap for one drain, in case reads return fast enough
         * to keep the time budget alive indefinitely. 192 × 512 B = 96
         * KiB, comfortably above the device's 64 KiB events outbox cap.
         */
        private const val MAX_DRAIN_READS: Int = 192

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
