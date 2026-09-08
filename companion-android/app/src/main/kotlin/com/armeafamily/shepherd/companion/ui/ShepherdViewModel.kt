package com.armeafamily.shepherd.companion.ui

import android.app.Application
import android.os.Build
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.armeafamily.shepherd.companion.appContainer
import com.armeafamily.shepherd.companion.ble.ConnectTimeoutException
import com.armeafamily.shepherd.companion.ble.RpcException
import com.armeafamily.shepherd.companion.ble.ShepherdConnection
import com.armeafamily.shepherd.companion.domain.AdminRecord
import com.armeafamily.shepherd.companion.domain.AudioOutputRecord
import com.armeafamily.shepherd.companion.domain.AdminSummary
import com.armeafamily.shepherd.companion.domain.BrightnessInfo
import com.armeafamily.shepherd.companion.domain.ClaimOutcome
import com.armeafamily.shepherd.companion.domain.EnrolmentRequestInfo
import com.armeafamily.shepherd.companion.domain.ClaimStateTag
import com.armeafamily.shepherd.companion.domain.DailyOverride
import com.armeafamily.shepherd.companion.domain.Diagnostic
import com.armeafamily.shepherd.companion.domain.DiagnosticSet
import com.armeafamily.shepherd.companion.domain.DiagnosticSeverity
import com.armeafamily.shepherd.companion.domain.DiagnosticSubject
import com.armeafamily.shepherd.companion.domain.EntryView
import com.armeafamily.shepherd.companion.domain.EventPayload
import com.armeafamily.shepherd.companion.domain.GroupView
import com.armeafamily.shepherd.companion.domain.LoginRequestInfo
import com.armeafamily.shepherd.companion.domain.ManagementClient
import com.armeafamily.shepherd.companion.domain.NetworkInterfaceView
import com.armeafamily.shepherd.companion.domain.NetworkStatusView
import com.armeafamily.shepherd.companion.domain.ServiceStateSnapshot
import com.armeafamily.shepherd.companion.domain.SessionInfo
import com.armeafamily.shepherd.companion.domain.ShepherdRecord
import com.armeafamily.shepherd.companion.domain.UsageStat
import com.armeafamily.shepherd.companion.domain.VolumeInfo
import com.armeafamily.shepherd.companion.domain.WebAuthStatus
import com.armeafamily.shepherd.companion.domain.WindowAction
import com.armeafamily.shepherd.companion.domain.WindowInfo
import com.armeafamily.shepherd.companion.ui.windows.WindowPresentation
import com.armeafamily.shepherd.companion.ble.Protocol
import com.armeafamily.shepherd.companion.util.Formatting
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
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** Coarse state of the BLE link to the active device. */
enum class LinkStatus {
    Idle,
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,

    /**
     * Retries are exhausted and the device is still advertising, so the
     * bond *may* be one-sided — but the same symptoms come from a
     * congested radio or a daemon restart. The bond is left intact and
     * re-pairing is offered, not forced; see [ShepherdViewModel.dropBondAndRepair].
     */
    RepairSuggested,

    /** The OS bond is provably gone. Re-pairing is the only way forward. */
    NeedsRepair,
}

/** Everything the device screens render. */
data class DeviceUiState(
    val record: ShepherdRecord? = null,
    val link: LinkStatus = LinkStatus.Idle,
    val snapshot: ServiceStateSnapshot? = null,
    val currentSession: SessionInfo? = null,
    val volume: VolumeInfo? = null,
    val brightness: BrightnessInfo? = null,
    /**
     * Audio outputs the device has seen, with any per-output volume limit
     * (issue #124). Populated by discovery on the device, so this is the list
     * of real hardware rather than anything configured ahead of time.
     */
    val audioOutputs: List<AudioOutputRecord> = emptyList(),
    /**
     * A per-output limit or forget is in flight. Disables the whole card while
     * it lands, so a second drag cannot race the refresh that follows the
     * first — the same gate the web UI applies.
     */
    val audioBusy: Boolean = false,
    /**
     * Categories sharing a schedule and budget (issue #5). Fetched separately
     * from the snapshot, which only carries entries.
     */
    val groups: List<GroupView> = emptyList(),
) {
    val entries: List<EntryView> get() = snapshot?.entries.orEmpty()

    /**
     * Whether the device is in administrator mode (issue #154). Rides the
     * snapshot, so it needs no RPC of its own and is correct the moment the
     * link comes up.
     */
    val adminMode: Boolean get() = snapshot?.adminMode ?: false

    /** Whether the device's screen is currently locked (issue #154). */
    val locked: Boolean get() = snapshot?.locked ?: false

    /** The category an activity belongs to, for labelling its row. */
    fun groupOf(entry: EntryView): GroupView? =
        entry.group?.let { id -> groups.firstOrNull { it.groupId == id } }
}

/**
 * The compositor's window list, as the windows screen renders it.
 *
 * Kept out of [DeviceUiState] because nothing fetches it unless that
 * screen is open: it is a maintenance view, and a list of Sway
 * containers is not worth an RPC on every connect.
 */
/**
 * Administrator-facing conditions on the device (issue #143).
 *
 * Kept apart from the entry list even though some diagnostics name an entry:
 * an activity can be perfectly available while something about it is
 * misconfigured, and the two answer different questions.
 */
/**
 * Browsers waiting to be signed in, and the web UI's password state (issue
 * #156).
 *
 * Polled rather than pushed, like the diagnostics above and for a stronger
 * reason: a request lives two minutes and a parent looking at this screen is
 * looking at it *because* they just clicked something on a laptop. A poll
 * every few seconds while the screen is open is the whole requirement.
 */
data class WebAuthUiState(
    val status: WebAuthStatus? = null,
    val requests: List<LoginRequestInfo> = emptyList(),
    val loading: Boolean = false,
    val error: String? = null,
    /** Set after an approval or denial lands, for a one-line confirmation. */
    val lastAction: String? = null,
)

/**
 * The device's administrators, and the phones waiting to become one
 * (issue #149).
 */
data class AdminsUiState(
    val admins: List<AdminSummary> = emptyList(),
    val requests: List<EnrolmentRequestInfo> = emptyList(),
    val loading: Boolean = false,
    /** True once a roster has arrived, so "empty" is not shown before asking. */
    val loaded: Boolean = false,
    val error: String? = null,
    /** Set after an approval, denial or revocation lands, for a one-liner. */
    val lastAction: String? = null,
)

data class DiagnosticsUiState(
    val set: DiagnosticSet = DiagnosticSet(items = emptyList(), truncated = false),
    val loading: Boolean = false,
    /** True once a set has arrived — distinguishes "healthy" from "not asked yet". */
    val loaded: Boolean = false,
    val error: String? = null,
) {
    val items: List<Diagnostic> get() = set.items

    /** Conditions where the config promises something the device is not doing. */
    val critical: List<Diagnostic>
        get() = set.items.filter { it.severity == DiagnosticSeverity.CRITICAL }

    /** Problems belonging to one activity, for that activity's own screen. */
    fun forEntry(entryId: String): List<Diagnostic> =
        set.items.filter { (it.subject as? DiagnosticSubject.Entry)?.entryId == entryId }
}

/**
 * Where the device is on the network (issue #182).
 *
 * Kept out of [DeviceUiState] for the same reason the window list is: nothing
 * needs it unless somebody has the network screen open, and the addresses on a
 * device are not worth an RPC on every connect.
 */
data class NetworkUiState(
    val status: NetworkStatusView? = null,
    val loading: Boolean = false,
    /** Last refresh failure, shown alongside whatever status we still hold. */
    val error: String? = null,
) {
    /** Interfaces another machine could reach this device at, most useful first. */
    val reachable: List<NetworkInterfaceView>
        get() = status?.interfaces.orEmpty().filter { it.reachable }

    /** Everything else — loopback, container bridges, interfaces with no address. */
    val other: List<NetworkInterfaceView>
        get() = status?.interfaces.orEmpty().filter { !it.reachable }
}

data class WindowsUiState(
    val windows: List<WindowInfo> = emptyList(),
    val loading: Boolean = false,
    /** True once a list has arrived — distinguishes "empty" from "not asked yet". */
    val loaded: Boolean = false,
    /** Last refresh failure, shown alongside whatever list we still hold. */
    val error: String? = null,
    /** Window with an action in flight; its row's buttons are disabled. */
    val busyId: Long? = null,
    /**
     * Whether the device was in administrator mode as of the last refresh.
     * Carried here rather than read from the device state at render time so
     * the three groupings below stay a pure function of this object, and stay
     * testable without a device.
     */
    val adminMode: Boolean = false,
) {
    /**
     * Windows on the child's screen that nothing is supervising.
     *
     * Split out and rendered first because they are the only rows on this
     * screen that are a problem rather than a fact. An orphan stashed on the
     * scratchpad stays under the scratchpad heading: it is hidden rather than
     * loose, which is the same line the device's own reconciliation sweep
     * draws before it warns about one.
     */
    val orphaned: List<WindowInfo>
        get() = windows.filter { !it.inScratchpad && WindowPresentation.isOrphan(it, adminMode) }
    val onScreen: List<WindowInfo>
        get() = windows.filter { !it.inScratchpad && !WindowPresentation.isOrphan(it, adminMode) }
    val scratchpad: List<WindowInfo> get() = windows.filter { it.inScratchpad }
}

/** Phases of the pairing flow surfaced to the pairing screen. */
sealed interface PairingPhase {
    data object Idle : PairingPhase
    data class Connecting(val deviceName: String?) : PairingPhase

    /** Bond in progress; show the "compare the digits" guidance + [mac]. */
    data class Comparing(val deviceName: String?, val mac: String) : PairingPhase
    data class Claiming(val deviceName: String?) : PairingPhase

    /**
     * The device already has an administrator, so this phone is queued
     * (issue #149). It shows [code]; whoever holds the other phone compares
     * those digits against the row in their Administrators list before
     * approving.
     *
     * The code can change while this is on screen: a request expires after a
     * few minutes and the next poll starts a fresh one, so the phone always
     * displays the digits the device would currently list.
     */
    data class AwaitingApproval(
        val deviceName: String?,
        val code: String,
        val requestId: String,
    ) : PairingPhase

    data class Success(val record: ShepherdRecord) : PairingPhase

    /**
     * [needsApproval] marks the refusal a parent can act on — an
     * administrator on another phone turned this request down — as opposed to
     * a transport failure they can only retry.
     */
    data class Failed(val reason: String, val needsApproval: Boolean = false) : PairingPhase
}

/**
 * Owns the live BLE session with the active device and drives pairing.
 *
 * A connection exists only while the UI is bound ([onForeground]) — going
 * to the background tears it down, honouring the project's "no background
 * BLE" constraint. Reconnects are attempted silently a few times with
 * backoff before the link is surfaced as dropped — and once it is, a slow
 * retry keeps running behind the banner so a fault that outlasts the
 * backoff ladder (a suspended box, a radio reset) still heals itself.
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

    private val _windows = MutableStateFlow(WindowsUiState())
    val windows: StateFlow<WindowsUiState> = _windows

    private val _diagnostics = MutableStateFlow(DiagnosticsUiState())
    val diagnostics: StateFlow<DiagnosticsUiState> = _diagnostics

    private val _network = MutableStateFlow(NetworkUiState())
    val network: StateFlow<NetworkUiState> = _network
    private val _webAuth = MutableStateFlow(WebAuthUiState())
    val webAuth: StateFlow<WebAuthUiState> = _webAuth

    private val _admins = MutableStateFlow(AdminsUiState())
    val admins: StateFlow<AdminsUiState> = _admins

    /** Default name to claim under — the phone's model. */
    val defaultPhoneName: String = Build.MODEL ?: "Android phone"

    private var connection: ShepherdConnection? = null
    private var client: ManagementClient? = null
    private var sessionJob: Job? = null
    private var eventsJob: Job? = null
    private var pairingJob: Job? = null
    private var windowsJob: Job? = null
    private var diagnosticsJob: Job? = null
    private var networkJob: Job? = null
    private var webAuthJob: Job? = null
    private var adminsJob: Job? = null
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
        // Don't start a session connection while a pairing flow owns a
        // (separate) connection — that would run two links at once and
        // could orphan one when pairing calls adopt().
        if (pairingJob?.isActive == true) return
        val record = repository.active ?: return
        if (connection == null) connectTo(record)
    }

    fun onBackground() {
        bound = false
        teardown()
        _state.update { it.copy(link = LinkStatus.Idle) }
    }

    fun selectDevice(androidIdentifier: String) {
        if (androidIdentifier == repository.activeId.value && connection != null) return
        repository.setActive(androidIdentifier)
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
        // Keep what we already know about *this* device across a
        // reconnect. The link status is rendered alongside it, so the
        // screen reads "here is the last known state, and the link is
        // down" rather than blanking to "No activities yet." — which is
        // what it did on every attempt once the give-up path started
        // retrying once a minute. Switching devices still starts clean:
        // showing one box's activities under another box's name would be
        // worse than showing nothing.
        val sameDevice = _state.value.record?.androidIdentifier == record.androidIdentifier
        _state.update { prev ->
            if (sameDevice) {
                prev.copy(record = record, link = LinkStatus.Connecting)
            } else {
                DeviceUiState(record = record, link = LinkStatus.Connecting)
            }
        }
        // Window ids are per-compositor: acting on another box's id would
        // hit whatever container happens to hold it there.
        if (!sameDevice) _windows.value = WindowsUiState()
        if (!sameDevice) _diagnostics.value = DiagnosticsUiState()
        // Another box's addresses are actively misleading: they would send
        // somebody to SSH into the device they just switched away from.
        if (!sameDevice) _network.value = NetworkUiState()
        if (!sameDevice) _webAuth.value = WebAuthUiState()
        if (!sameDevice) _admins.value = AdminsUiState()
        conn.start()
        eventsJob = viewModelScope.launch {
            conn.events.collect { event -> applyEvent(event.payload) }
        }
        sessionJob = viewModelScope.launch { runConnectionLoop(record, conn) }
    }

    private suspend fun runConnectionLoop(record: ShepherdRecord, conn: ShepherdConnection) {
        // Long enough to ride out a daemon restart (~38s of retries).
        // The old 1+2+5s ladder gave up in 8s — less than a session
        // restart takes — so a routine shepherdd restart exhausted the
        // retries while the device was still coming back up, and the
        // reachability probe below then read "advertising but unusable"
        // and offered to re-pair. Suggesting a trip to the TV for what
        // is a self-healing event is worse than waiting.
        val backoffs = longArrayOf(1_000, 2_000, 5_000, 10_000, 20_000)
        var failures = 0
        while (viewModelScope.isActive) {
            _state.update {
                it.copy(link = if (failures == 0) LinkStatus.Connecting else LinkStatus.Reconnecting)
            }
            try {
                connectWithin(conn)
                failures = 0
                _state.update { it.copy(link = LinkStatus.Connected) }
                refreshAll()
                // Suspend until the session ends, then loop to reconnect.
                // Not `state.first { it is Disconnected }`: a daemon
                // restart leaves the ACL link up and only removes the
                // GATT service, so that never fires and the loop parks
                // here forever behind a screen of stale data. See
                // ShepherdConnection.awaitSessionEnd.
                conn.awaitSessionEnd()
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                // Force the link down before retrying. When the peer's GATT
                // server restarts, the ACL link survives it and Android goes
                // on serving the service list it discovered before — which no
                // longer contains ours. `peripheral.connect()` on an already
                // -connected peripheral is a no-op, so it never re-discovers
                // and every retry fails "Service … not found" in perpetuity,
                // even long after the daemon is back. Only a real disconnect
                // makes the next attempt rediscover.
                runCatching { conn.disconnect() }
                // Any connect/drain failure lands here — out of range, the
                // box powered off, shepherd not running (bond fine but the
                // GATT service is absent), a genuinely one-sided bond, or a
                // revoked BLUETOOTH_CONNECT permission (hence the guarded
                // isBonded). We deliberately do NOT treat an encrypted-read
                // failure on its own as a lost bond: a running-but-serviceless
                // peer fails identically, and wiping the bond there would
                // force a needless re-pair. The scan-probe below is the only
                // thing that removes a bond, and only on positive evidence.
                val stillBonded =
                    runCatching { container.bondManager.isBonded(record.androidIdentifier) }.getOrDefault(true)
                if (!stillBonded) {
                    // The OS bond itself vanished — factory-reset or claimed
                    // elsewhere. Stop and prompt re-pair.
                    releaseConnection(conn)
                    _state.update { it.copy(link = LinkStatus.NeedsRepair) }
                    return
                }
                failures++
                if (failures > backoffs.size) {
                    // Exhausted retries while still OS-bonded. Distinguish a
                    // stale one-sided bond from a device that's simply
                    // unreachable (off, out of range, or shepherd not
                    // running) by scanning for the *shepherd service*: if the
                    // device is still advertising it — i.e. shepherd is up and
                    // in range — yet we can't hold an encrypted link, a stale
                    // bond is the leading explanation.
                    //
                    // Leading, but not proven. A congested 2.4 GHz band, a
                    // daemon restart mid-connect, and an outbox backlog that
                    // outruns the connect drain all present identically here,
                    // and all of them clear up on their own. Removing the bond
                    // automatically turned each of those into a mandatory trip
                    // to the TV to re-pair. So we surface the suggestion and
                    // leave the bond alone — `dropBondAndRepair` removes it
                    // only when the user takes us up on it.
                    releaseConnection(conn)
                    val link = if (deviceIsReachable(record)) {
                        LinkStatus.RepairSuggested
                    } else {
                        LinkStatus.Disconnected
                    }
                    _state.update { it.copy(link = link) }
                    // Give up on this *connection*, not on the device. The
                    // faults that land here outlive the ladder routinely — a
                    // box asleep on the couch outlasts it by an hour — and
                    // this used to `return`, so a foregrounded app that had
                    // given up made no further attempt for as long as it
                    // stayed foregrounded. On 2026-08-16 that turned one
                    // power-key suspend into a companion that stayed dead
                    // across the resume, a logout/login, and a whole fresh
                    // shepherdd: the device-side journal shows it advertising
                    // and answering, with the phone never going on air again
                    // (docs/ai/history/2026-08-16 001
                    // ble-connect-fails-after-long-session.md). The banner
                    // stays — the user still gets Retry/Re-pair — but a slow
                    // retry keeps running underneath it so the link heals
                    // itself the moment the fault clears.
                    scheduleRetryAfterGiveUp(record)
                    return
                }
                delay(backoffs[failures - 1])
            }
        }
    }

    /**
     * Run [ShepherdConnection.connect] under a wall-clock budget.
     *
     * `connect()` is the one phase of the session no timeout used to
     * cover: [ShepherdConnection.REQUEST_TIMEOUT_MS] applies to `call()`
     * only, so a connect that stalled — most often in the post-connect
     * outbox drain — parked [runConnectionLoop] indefinitely on
     * `Connecting`, never reaching the backoff ladder below it. A symptom
     * that outlives the 15 s RPC timeout by minutes is the signature of
     * this path, not of a slow RPC.
     */
    private suspend fun connectWithin(
        conn: ShepherdConnection,
        probeEncryptedLink: Boolean = true,
    ) {
        val connected = withTimeoutOrNull(CONNECT_TIMEOUT_MS) {
            conn.connect(probeEncryptedLink)
            true
        }
        if (connected == null) {
            // Don't leave a half-open link behind. The disconnect also
            // makes the daemon clear its outboxes, which is exactly the
            // state the next attempt wants to find.
            runCatching { conn.disconnect() }
            throw ConnectTimeoutException(CONNECT_TIMEOUT_MS)
        }
    }

    /**
     * Forget the OS bond and hand off to the pairing flow.
     *
     * Only ever reached by the user accepting the re-pair offer on a
     * [LinkStatus.RepairSuggested] or [LinkStatus.NeedsRepair] banner —
     * the connect loop no longer does this on its own, because the
     * evidence it has (device advertising, link unusable) is consistent
     * with several transient faults that fix themselves.
     */
    fun dropBondAndRepair() {
        val record = _state.value.record ?: return
        teardown()
        runCatching { container.bondManager.removeBond(record.androidIdentifier) }
        _state.update { it.copy(link = LinkStatus.NeedsRepair) }
    }

    /**
     * Tear down a connection the loop is giving up on: close the
     * peripheral and cancel its pollers (leaked otherwise), and drop the
     * session fields if this is still the active connection.
     */
    private fun releaseConnection(conn: ShepherdConnection) {
        eventsJob?.cancel(); eventsJob = null
        conn.close()
        if (connection === conn) {
            connection = null
            client = null
        }
    }

    /**
     * Briefly scan for the device's advertisement to decide whether a
     * repeated connect failure is "unreachable" vs. "reachable but the
     * bond is one-sided". Returns true if the device is advertising
     * within [SCAN_PROBE_MS]. Requires BLUETOOTH_SCAN + BT on; any failure
     * (permission, adapter off) is treated as not-reachable.
     */
    private suspend fun deviceIsReachable(record: ShepherdRecord): Boolean =
        runCatching {
            withTimeoutOrNull(SCAN_PROBE_MS) {
                container.scanner.scan().first { it.identifier == record.androidIdentifier }
                true
            } ?: false
        }.getOrDefault(false)

    private fun teardown() {
        pairingJob?.cancel(); pairingJob = null
        sessionJob?.cancel(); sessionJob = null
        eventsJob?.cancel(); eventsJob = null
        windowsJob?.cancel(); windowsJob = null
        connection?.close(); connection = null
        client = null
    }

    /**
     * Keep trying, slowly, after [runConnectionLoop] has given up.
     *
     * Parked in `sessionJob` so `teardown()`/[onBackground] cancel it like
     * any other session work — the app must not hold BLE work in the
     * background — and so [onForeground] (which rebuilds when
     * `connection == null`) still gets the user an *immediate* attempt on
     * return rather than waiting out the interval.
     *
     * The interval is deliberately far longer than the ladder: this runs
     * behind a banner that already tells the user something is wrong, so
     * it only has to beat "never", not be quick. Each firing rebuilds the
     * connection and re-runs the whole ladder, so a device that comes
     * back is picked up within a minute of doing so.
     */
    private fun scheduleRetryAfterGiveUp(record: ShepherdRecord) {
        sessionJob = viewModelScope.launch {
            while (isActive) {
                delay(RETRY_AFTER_GIVE_UP_MS)
                // `connectTo` would tear this job down mid-flight; hand the
                // rebuild to the scope instead and stop looping here.
                if (bound && connection == null) {
                    viewModelScope.launch { connectTo(record) }
                    return@launch
                }
            }
        }
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
        runCatching { c.listGroups() }.onSuccess { g -> _state.update { it.copy(groups = g) } }
        runCatching { c.getVolume() }.onSuccess { v -> _state.update { it.copy(volume = v) } }
        runCatching { c.getBrightness() }.onSuccess { b -> _state.update { it.copy(brightness = b) } }
        runCatching { c.listAudioOutputs() }
            .onSuccess { o -> _state.update { it.copy(audioOutputs = o) } }
    }

    private fun applyEvent(payload: EventPayload) {
        when (payload) {
            is EventPayload.StateChanged -> {
                val snap = payload.toSnapshot()
                _state.update { it.copy(snapshot = snap, currentSession = snap.currentSession) }
                // The pushed snapshot carries entries but not categories, and
                // a member's session spends the category's shared budget.
                refreshGroups()
            }
            is EventPayload.SessionStarted,
            is EventPayload.SessionExpiring,
            is EventPayload.WarningIssued,
            -> refreshSession()
            // A finished session has just spent shared budget and may have
            // started a category-wide cooldown.
            is EventPayload.SessionEnded -> {
                refreshSession()
                refreshGroups()
            }
            is EventPayload.PolicyReloaded,
            is EventPayload.EntryAvailabilityChanged,
            is EventPayload.InternetStatusChanged,
            -> refreshSnapshot()
            is EventPayload.VolumeChanged -> {
                refreshVolume()
                // A VolumeChanged can mean the active output changed, which
                // moves the "In use now" marker and can surface a device the
                // list has never seen.
                refreshAudioOutputs()
            }
            is EventPayload.BrightnessChanged -> refreshBrightness()
            else -> Unit
        }
    }

    private fun refreshSession() = viewModelScope.launch {
        client?.let { c -> runCatching { c.currentSession() }.onSuccess { s -> _state.update { it.copy(currentSession = s) } } }
    }

    private fun refreshSnapshot() = viewModelScope.launch {
        client?.let { c -> runCatching { c.serviceState() }.onSuccess { s -> _state.update { it.copy(snapshot = s, currentSession = s.currentSession) } } }
        // Group state moves with entry state — a member's session spends the
        // category's shared budget — so refresh both together.
        refreshGroups()
    }

    private fun refreshGroups() = viewModelScope.launch {
        client?.let { c -> runCatching { c.listGroups() }.onSuccess { g -> _state.update { it.copy(groups = g) } } }
    }

    private fun refreshVolume() = viewModelScope.launch {
        client?.let { c -> runCatching { c.getVolume() }.onSuccess { v -> _state.update { it.copy(volume = v) } } }
    }

    private fun refreshAudioOutputs() = viewModelScope.launch {
        client?.let { c ->
            runCatching { c.listAudioOutputs() }
                .onSuccess { o -> _state.update { it.copy(audioOutputs = o) } }
        }
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

    /** `maxVolume = null` clears this output's cap. */
    fun setAudioOutputLimit(outputKey: String, maxVolume: Int?) = action { c ->
        _state.update { it.copy(audioBusy = true) }
        try {
            c.setAudioOutputLimits(outputKey, maxVolume)
            // The device may have turned the volume down to obey a new cap, so
            // the volume card has to be refetched alongside the row list.
            //
            // The busy window spans the refreshes too, not just the write: a
            // row re-syncs its slider from the record that comes back, so a
            // second drag begun after the write resolved but before the list
            // arrived would be snapped out from under the finger.
            joinAll(refreshAudioOutputs(), refreshVolume())
        } finally {
            _state.update { it.copy(audioBusy = false) }
        }
    }

    /** Move sound to another output (issue #124). */
    fun selectAudioOutput(outputKey: String) = action { c ->
        _state.update { it.copy(audioBusy = true) }
        try {
            val volume = c.selectAudioOutput(outputKey)
            // Name the device: the daemon refuses one it can no longer see, so a
            // silent success would be indistinguishable from nothing happening.
            _message.value = volume.output?.description
                ?.let { "Now playing through $it." } ?: "Switched output."
            joinAll(refreshAudioOutputs(), refreshVolume())
        } finally {
            _state.update { it.copy(audioBusy = false) }
        }
    }

    fun forgetAudioOutput(outputKey: String) = action { c ->
        _state.update { it.copy(audioBusy = true) }
        try {
            c.forgetAudioOutput(outputKey)
            joinAll(refreshAudioOutputs(), refreshVolume())
        } finally {
            _state.update { it.copy(audioBusy = false) }
        }
    }

    fun setBrightness(percent: Int) = action { c -> _state.update { it.copy(brightness = c.setBrightness(percent)) } }

    fun setAutoBrightness(enabled: Boolean) =
        action { c -> _state.update { it.copy(brightness = c.setAutoBrightness(enabled)) } }

    fun reloadConfig() = action { c ->
        val result = c.reloadConfig()
        _message.value = "Config reloaded — ${result.entryCount} entries."
        refreshSnapshot()
    }

    /**
     * Ask the device to re-fetch its media libraries (issue #165).
     *
     * The message deliberately says "started", not "done": the device answers
     * as soon as it accepts the request, and the fetches outlive the reply by
     * minutes. A refresh that could not reach what it went for raises a
     * diagnostic, which the health screen shows.
     */
    fun refreshMedia() = action { c ->
        c.refreshMedia()
        _message.value = "Refreshing media libraries…"
    }

    fun logoutDevice() = action { c ->
        c.logout()
        _message.value = "Logged out the device session."
    }

    // --- windows (issue #140) ------------------------------------------

    /**
     * Re-read the compositor's window list.
     *
     * The device pushes no event when a window opens, closes, or is
     * stashed on the scratchpad, so the windows screen polls this while
     * it is open. Concurrent calls collapse onto the in-flight one: the
     * poll ticks faster than a BLE round trip on a busy link, and
     * queueing them would just spend the link on a list nobody is
     * waiting for any more.
     *
     * A failure keeps the previous list and rides alongside it. Blanking
     * the screen on a refresh that lands mid-reconnect would drop the
     * rows out from under a finger already reaching for "Close".
     */
    /**
     * Re-read what is currently wrong with the device (issue #143).
     *
     * A plain refresh rather than a subscription: the daemon does emit a
     * `DiagnosticsChanged` event, but these are conditions somebody has to go
     * and fix, so the phone showing them a few seconds late costs nothing and a
     * poll needs no reconnect handling.
     */
    fun refreshDiagnostics() {
        if (diagnosticsJob?.isActive == true) return
        val c = client ?: run {
            _diagnostics.update { it.copy(loading = false, error = "Not connected.") }
            return
        }
        _diagnostics.update { it.copy(loading = true) }
        diagnosticsJob = viewModelScope.launch {
            try {
                val set = c.listDiagnostics()
                _diagnostics.update {
                    it.copy(set = set, loading = false, loaded = true, error = null)
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _diagnostics.update {
                    it.copy(loading = false, error = why ?: "Couldn't read device health.")
                }
            }
        }
    }

    /**
     * Re-read where the device is on the network (issue #182).
     *
     * Polled while the screen is open rather than pushed: an address changes
     * when a cable is plugged in or a VPN comes up, and neither is worth an
     * event on a link this app shares with everything else it does.
     */
    fun refreshNetwork() {
        if (networkJob?.isActive == true) return
        val c = client ?: run {
            _network.update { it.copy(loading = false, error = "Not connected.") }
            return
        }
        _network.update { it.copy(loading = true) }
        networkJob = viewModelScope.launch {
            try {
                val status = c.networkStatus()
                _network.update { it.copy(status = status, loading = false, error = null) }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _network.update {
                    it.copy(loading = false, error = why ?: "Couldn't read the network status.")
                }
            }
        }
    }

    /**
     * Re-read the device's administrators and anyone waiting to become one
     * (issue #149).
     *
     * Safe to call on a timer, like [refreshWebAuth], and for the same reason:
     * a pending enrolment has no event behind it, so the screen that shows it
     * polls while it is open and nothing polls while it is not.
     */
    fun refreshAdmins() {
        if (adminsJob?.isActive == true) return
        val c = client ?: run {
            _admins.update { it.copy(loading = false, error = "Not connected.") }
            return
        }
        _admins.update { it.copy(loading = true) }
        adminsJob = viewModelScope.launch {
            try {
                val roster = c.listAdmins()
                val waiting = c.listEnrolmentRequests()
                _admins.update {
                    it.copy(
                        admins = roster,
                        requests = waiting,
                        loading = false,
                        loaded = true,
                        error = null,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _admins.update {
                    it.copy(loading = false, error = why ?: "Couldn't read the administrators.")
                }
            }
        }
    }

    /**
     * Let a waiting phone administer this device, or turn it away.
     *
     * The parent has already compared the six digits against the other phone's
     * screen; this is the tap that enrols it.
     */
    fun decideEnrolment(id: String, approve: Boolean) {
        val c = client ?: return
        viewModelScope.launch {
            try {
                val enrolled = if (approve) c.approveEnrolmentRequest(id) else null
                if (!approve) c.denyEnrolmentRequest(id)
                _admins.update {
                    it.copy(
                        requests = it.requests.filterNot { r -> r.id == id },
                        admins = if (enrolled != null) it.admins + enrolled else it.admins,
                        lastAction = if (approve) "Added as an administrator." else "Turned away.",
                        error = null,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _admins.update { it.copy(error = why ?: "Couldn't answer the request.") }
            }
        }
    }

    /**
     * Remove an administrator, which also tells the device to forget that
     * phone's Bluetooth bond.
     *
     * The device refuses to remove the last one — that would leave it with
     * nobody able to reach it and a phone still bonded to it — and says so.
     */
    fun revokeAdmin(id: String) {
        val c = client ?: return
        viewModelScope.launch {
            try {
                c.revokeAdmin(id)
                _admins.update {
                    it.copy(
                        admins = it.admins.filterNot { a -> a.id == id },
                        lastAction = "Removed.",
                        error = null,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _admins.update { it.copy(error = why ?: "Couldn't remove that administrator.") }
            }
        }
    }

    fun consumeAdminsAction() {
        _admins.update { it.copy(lastAction = null) }
    }

    /**
     * Re-read the web UI's password state and any browsers waiting for a tap
     * (issue #156).
     *
     * Safe to call on a timer: it returns immediately if a read is already in
     * flight, and the screen that shows this polls every few seconds while it
     * is open.
     */
    fun refreshWebAuth() {
        if (webAuthJob?.isActive == true) return
        val c = client ?: run {
            _webAuth.update { it.copy(loading = false, error = "Not connected.") }
            return
        }
        _webAuth.update { it.copy(loading = true) }
        webAuthJob = viewModelScope.launch {
            try {
                val status = c.webAuthStatus()
                val requests = c.listLoginRequests()
                _webAuth.update {
                    it.copy(status = status, requests = requests, loading = false, error = null)
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _webAuth.update {
                    it.copy(loading = false, error = why ?: "Couldn't read the sign-in requests.")
                }
            }
        }
    }

    /**
     * Let a waiting browser in, or turn it away.
     *
     * The parent has already compared the six digits against the screen they
     * are sitting at; this is the tap that mints the session.
     */
    fun decideLoginRequest(id: String, approve: Boolean) {
        val c = client ?: return
        viewModelScope.launch {
            try {
                if (approve) c.approveLoginRequest(id) else c.denyLoginRequest(id)
                _webAuth.update {
                    it.copy(
                        requests = it.requests.filterNot { r -> r.id == id },
                        lastAction = if (approve) "Signed in." else "Turned away.",
                        error = null,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _webAuth.update { it.copy(error = why ?: "Couldn't answer the request.") }
            }
        }
    }

    /**
     * Set the web UI's password from here.
     *
     * The reset path that means a parent who has forgotten it does not need an
     * SSH client. No old password is asked for: reaching this at all required
     * a bonded, authenticated BLE link to a device this phone is the admin of.
     */
    fun setWebPassword(password: String) {
        val c = client ?: return
        viewModelScope.launch {
            try {
                c.setWebPassword(password)
                _webAuth.update {
                    it.copy(
                        status = it.status?.copy(configured = true),
                        lastAction = "Password set.",
                        error = null,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _webAuth.update { it.copy(error = why ?: "Couldn't set the password.") }
            }
        }
    }

    /** Clear the one-line confirmation after the UI has shown it. */
    fun clearWebAuthAction() = _webAuth.update { it.copy(lastAction = null) }

    fun refreshWindows() {
        if (windowsJob?.isActive == true) return
        val c = client ?: run {
            _windows.update { it.copy(loading = false, error = "Not connected.") }
            return
        }
        _windows.update { it.copy(loading = true) }
        windowsJob = viewModelScope.launch {
            try {
                val list = c.listWindows()
                val adminMode = _state.value.adminMode
                _windows.update {
                    it.copy(
                        windows = list,
                        loading = false,
                        loaded = true,
                        error = null,
                        adminMode = adminMode,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val why = if (e is RpcException) ReasonText.describe(e) else e.message
                _windows.update {
                    it.copy(loading = false, error = why ?: "Couldn't list the windows.")
                }
            }
        }
    }

    /**
     * Close, hide, or show one window.
     *
     * [act] is spelled short because `action` is the private RPC-error
     * wrapper this delegates to.
     */
    fun actOnWindow(id: Long, act: WindowAction) = action { c ->
        _windows.update { it.copy(busyId = id) }
        try {
            c.actOnWindow(id, act)
            _message.value = when (act) {
                WindowAction.CLOSE -> "Asked the window to close."
                WindowAction.HIDE -> "Moved to the scratchpad."
                WindowAction.SHOW -> "Pulled off the scratchpad."
                WindowAction.FOCUS -> "Switched to the window."
                // Unreachable in practice: `act` comes from this app's own
                // buttons, not off the wire. It exists because the enum has to
                // tolerate a value a newer device might send, and the honest
                // thing for an action we cannot name is to confirm nothing.
                WindowAction.UNKNOWN -> "Asked the device to act on the window."
            }
        } finally {
            _windows.update { it.copy(busyId = null) }
        }
        // Sway applies the action out of band — an app can even refuse to
        // close — so re-read the tree rather than predicting it. Cancel
        // any refresh already running so this one isn't dropped as a
        // duplicate and leaves the list a beat stale.
        windowsJob?.cancel()
        refreshWindows()
    }

    /**
     * Enter administrator mode (issue #154). Refused by the device while an
     * activity is running; the message says which, so it is shown as-is.
     */
    fun enterAdminMode() = action { c ->
        c.enterAdminMode()
        _message.value = "Administrator mode on."
        refreshSnapshot()
    }

    /**
     * Leave administrator mode. Never refused, whatever is still on screen.
     *
     * No `refreshSnapshot()` afterwards, unlike every other control here:
     * leaving logs the device's session out (issue #154), so the snapshot this
     * would ask for is one the device is in no position to answer. Same
     * reasoning as [logoutDevice], which has never refreshed either.
     */
    fun exitAdminMode() = action { c ->
        c.exitAdminMode()
        _message.value = "Administrator mode off; the device is logging out."
    }

    /** Lock the device's screen. Only meaningful inside administrator mode. */
    fun lockDevice() = action { c ->
        c.lockDevice()
        _message.value = "Screen locked."
        refreshSnapshot()
    }

    /** Unlock it. There is no way to do this from the device itself. */
    fun unlockDevice() = action { c ->
        c.unlockDevice()
        _message.value = "Screen unlocked."
        refreshSnapshot()
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

    /**
     * Grant or revoke banked time on a token gate (issue #8).
     *
     * Refreshes entries and categories afterwards: the grant may have opened
     * or closed the gate, which changes what every other screen shows.
     */
    fun adjustTokens(subject: String, deltaSeconds: Long) = action { c ->
        val status = c.adjustTokens(subject, deltaSeconds)
        val sign = if (deltaSeconds >= 0) "+" else "−"
        val amount = Formatting.coarse(kotlin.math.abs(deltaSeconds))
        _message.value =
            "Earned time $sign$amount (now ${Formatting.coarse(status.balance.secs)})."
        refreshSnapshot()
    }

    fun deleteOverride(id: String, date: String?, onDone: () -> Unit) = action { c ->
        c.deleteOverride(id, date)
        _message.value = "Override cleared."
        onDone()
    }

    /**
     * Load today's override for a limit subject (an entry ID, or `group:<id>`).
     *
     * Returns `success(null)` when no override is set, and `failure` when the
     * lookup itself failed. Those two cases must stay distinguishable: a
     * failure previously collapsed to `null`, which rendered the editor as
     * "no override set" and let a caregiver silently overwrite a real one.
     */
    suspend fun loadOverride(id: String, date: String?): Result<DailyOverride?> {
        val c = client ?: return Result.failure(IllegalStateException("Not connected."))
        return runCatching { c.getOverride(id, date) }
            .onFailure { _message.value = "Couldn't load today's override: ${it.message ?: "unknown error"}" }
    }

    suspend fun loadUsage(id: String, from: String, to: String): List<UsageStat> =
        client?.let { runCatching { it.usageEntry(id, from, to) }.getOrDefault(emptyList()) } ?: emptyList()

    // --- factory reset / forget ---------------------------------------

    fun factoryReset(onDone: () -> Unit) {
        val record = _state.value.record ?: run { _message.value = "Not connected."; return }
        viewModelScope.launch {
            // Best-effort RPC: the device removes its BlueZ bond and
            // disconnects us while handling this, so the response may never
            // arrive (a dropped link now fails the call fast rather than
            // after the 15s timeout). Either way the token is dead
            // server-side, so proceed to clean up regardless.
            client?.let { runCatching { it.factoryReset() } }
            // Drop the now one-sided OS bond too, so a later re-pair starts
            // a fresh pairing instead of short-circuiting on a stale bond.
            container.bondManager.removeBond(record.androidIdentifier)
            teardown()
            repository.remove(record.androidIdentifier)
            _message.value = "Device unpaired."
            onDone()
        }
    }

    fun forgetDevice(androidIdentifier: String) {
        viewModelScope.launch {
            if (androidIdentifier == _state.value.record?.androidIdentifier) teardown()
            repository.remove(androidIdentifier)
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

    fun updateNickname(androidIdentifier: String, nickname: String?) {
        viewModelScope.launch { repository.updateNickname(androidIdentifier, nickname?.trim()?.ifBlank { null }) }
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
        // The pairing connection becomes the live session on success, so
        // tear down anything currently active first (also cancels a prior
        // pairing attempt).
        teardown()
        _pairing.value = PairingPhase.Connecting(null)
        // Track the pairing coroutine so teardown()/onBackground() can
        // cancel it — otherwise backgrounding mid-pairing leaks a live BLE
        // connection and keeps doing BLE work in the background. The
        // cancellation handler below closes `conn`.
        // Sample the bond *before* the peripheral is touched. Bonding is
        // also triggered implicitly by any encrypt-authenticated read, so
        // a bond seen later in this flow may be the one this flow just
        // created — and dropping that discards the pairing the user has
        // already confirmed on both screens. Only a bond that predates
        // the attempt can be assumed stale.
        val hadPriorBond = runCatching { container.bondManager.isBonded(identifier) }
            .getOrDefault(false)
        pairingJob = viewModelScope.launch {
            var conn = ShepherdConnection.fromIdentifier(identifier, viewModelScope)
            try {
                conn.start()
                // Pre-bond: connect() skips the encrypted drain entirely
                // here, so nothing in this step can start bonding behind
                // the pairing screen's back. Same wall-clock cap as a
                // session connect, so a stall can't park the spinner with
                // no way out but backing out of the flow.
                connectWithin(conn, probeEncryptedLink = false)
                val info = conn.readDeviceInfo()

                if (info.protocolVersion != Protocol.PROTOCOL_VERSION) {
                    fail(conn, "This device speaks protocol v${info.protocolVersion}; update the app.")
                    return@launch
                }
                // A claimed device is no longer a dead end (issue #149): this
                // phone bonds anyway and then asks, and an administrator on
                // another phone decides. Only the *first* phone gets in
                // without being asked about.
                val alreadyClaimed = info.claimState == ClaimStateTag.CLAIMED

                _pairing.value = PairingPhase.Comparing(info.deviceName, identifier)
                // Drop the bond only if this phone already had one when the
                // flow started *and* the device says it is unclaimed. That
                // combination is the only one where the bond is provably
                // stale: an unclaimed device has forgotten every bond, so
                // trusting ours would skip straight to claim over a link that
                // can never encrypt.
                //
                // A claimed device is explicitly *not* that case, and getting
                // this wrong cost a pairing during #149. A second phone that
                // bonded on a previous attempt and has not been approved yet
                // holds a bond the device also holds; dropping it leaves the
                // device with a key the phone no longer has, and the next
                // connect is torn down mid-handshake ("Disconnect detected")
                // with nothing on either side explaining why. Keeping it means
                // a genuinely stale bond surfaces as a pairing error instead —
                // recoverable, and far rarer.
                //
                // A bond that appeared *during* the flow is this pairing's own
                // and must be kept either way — which is why the decision uses
                // the sample taken before we touched the peripheral.
                //
                // Removing a bond also drops the GATT link it belongs to, so
                // this cannot happen underneath a connection we still need:
                // doing it inline killed the very link the bond and claim
                // were about to run over, and pairing failed with a
                // "cancelled or failed" that had nothing to do with the
                // user. Retire this connection first, then build a fresh one
                // on the other side of the removal.
                if (hadPriorBond && !alreadyClaimed) {
                    conn.close()
                    container.bondManager.dropBond(identifier)
                    conn = ShepherdConnection.fromIdentifier(identifier, viewModelScope)
                    conn.start()
                    connectWithin(conn, probeEncryptedLink = false)
                }
                val bonded = container.bondManager.ensureBonded(identifier)
                if (!bonded) {
                    fail(conn, "Pairing was cancelled or failed. Try again.")
                    return@launch
                }

                _pairing.value = PairingPhase.Claiming(info.deviceName)
                val client = ManagementClient(conn)
                // On a claimed device this may come back pending, in which
                // case the phone waits here — showing the digits — until an
                // administrator decides.
                val admin: AdminRecord = when (val outcome = client.claim(phoneName)) {
                    is ClaimOutcome.Claimed -> outcome.admin
                    is ClaimOutcome.Pending ->
                        awaitApproval(client, info.deviceName, phoneName, outcome.request)
                            ?: return@launch
                    is ClaimOutcome.Unknown -> {
                        fail(conn, "This device answered in a way this app doesn't understand.")
                        return@launch
                    }
                }
                val record = admin.toShepherdRecord(
                    androidIdentifier = identifier,
                    deviceName = info.deviceName,
                )
                repository.upsert(record)

                // Adopt this connection as the active session.
                adopt(record, conn)
                _pairing.value = PairingPhase.Success(record)
            } catch (e: RpcException) {
                if (e.code == com.armeafamily.shepherd.companion.ble.ErrorCode.ENROLMENT_DENIED) {
                    conn.close()
                    _pairing.value = PairingPhase.Failed(ReasonText.describe(e), needsApproval = true)
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

    /**
     * Sit on the approval screen until an administrator decides.
     *
     * Returns the minted record once approved, or `null` when the phase has
     * already been moved to a terminal state (denial is thrown as an
     * [RpcException] by `claim` and handled by the caller).
     *
     * Polling `claim` is the whole protocol: the device answers with the same
     * request while it is pending, the record once it is approved, and an
     * `enrolment_denied` error if it was turned down. Nothing else is needed —
     * and because the bond is the requester's identity, there is no polling
     * secret to hold on to.
     *
     * If the request expires the device starts a fresh one with new digits, so
     * the displayed code is refreshed from every answer rather than being
     * captured once. That keeps this screen and the approver's list showing
     * the same number no matter how long the walk between them takes.
     */
    private suspend fun awaitApproval(
        client: ManagementClient,
        deviceName: String?,
        phoneName: String,
        first: EnrolmentRequestInfo,
    ): AdminRecord? {
        var request = first
        while (true) {
            _pairing.value = PairingPhase.AwaitingApproval(deviceName, request.code, request.id)
            delay(ENROLMENT_POLL)
            when (val outcome = client.claim(phoneName)) {
                is ClaimOutcome.Claimed -> return outcome.admin
                is ClaimOutcome.Pending -> request = outcome.request
                is ClaimOutcome.Unknown -> {
                    _pairing.value =
                        PairingPhase.Failed("This device answered in a way this app doesn't understand.")
                    return null
                }
            }
        }
    }

    private fun fail(conn: ShepherdConnection, reason: String) {
        conn.close()
        _pairing.value = PairingPhase.Failed(reason)
    }

    /** Take over an already-connected, claimed connection as the session. */
    private fun adopt(record: ShepherdRecord, conn: ShepherdConnection) {
        // Drop any session that raced in while pairing was in flight, but
        // never close the connection we're adopting.
        if (connection != null && connection !== conn) connection?.close()
        sessionJob?.cancel()
        eventsJob?.cancel()
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

    private companion object {
        /** How long to scan for the device before concluding it's unreachable. */
        const val SCAN_PROBE_MS = 5_000L

        /**
         * Ceiling on one [ShepherdConnection.connect] attempt: GATT
         * connect + discovery + MTU + link-encryption settle + the
         * bounded post-connect drain.
         *
         * A healthy reconnect is well under two seconds. This is sized
         * to sit *above* the sum of connect()'s own internal budgets, so
         * a stall surfaces as the specific failure that caused it
         * (settle exhausted, drain stalled) rather than being masked by
         * a generic timeout here. It's the backstop of last resort for
         * something connect() doesn't bound at all.
         */
        const val CONNECT_TIMEOUT_MS = 30_000L

        /**
         * Pause between give-up and the next full attempt, while the app
         * stays foregrounded. Long on purpose: the retry runs behind a
         * banner the user can already act on, so its job is to beat
         * "never", not to be fast. One rebuilt connection per minute is
         * also cheap enough to leave running for as long as the screen
         * is up.
         */
        const val RETRY_AFTER_GIVE_UP_MS = 60_000L

        /**
         * How often a phone waiting to be enrolled asks again (issue #149).
         *
         * Each poll is one BLE round trip on an otherwise idle link, and the
         * thing it is waiting for is a human walking to another phone, so
         * there is nothing to gain from being quicker. Slow enough to be
         * unnoticeable, fast enough that the approval feels immediate.
         */
        const val ENROLMENT_POLL = 3_000L

        /**
         * How often the administrators screen re-reads the roster and the
         * queue while it is open. Same shape as the web-access screen's poll,
         * and for the same reason: there is no event for either, and a screen
         * nobody is looking at polls nothing.
         */
        const val ADMINS_POLL_MS = 3_000L
    }
}

/**
 * Build a persisted record from the claim result.
 *
 * [deviceName] comes from `DeviceInfo` — the shepherd device's own name —
 * and deliberately *not* from [AdminRecord.deviceName], which is the
 * admin phone's name (`claim(phoneName)` sets it, correctly, to identify
 * who claimed the device). Using the claim record's value labelled every
 * device in the picker with the phone's name, so a user with two boxes
 * saw two identical entries.
 */
private fun AdminRecord.toShepherdRecord(
    androidIdentifier: String,
    deviceName: String,
) = ShepherdRecord(
    identityAddress = identityAddress,
    addressType = addressType,
    deviceName = deviceName,
    bondedAt = bondedAt,
    httpToken = httpToken,
    role = role.name.lowercase(),
    androidIdentifier = androidIdentifier,
)
