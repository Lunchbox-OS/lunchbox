package com.armeafamily.shepherd.companion.ble

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.util.Log
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.coroutines.resume

/**
 * Drives Android's bond (pairing) state machine for the Numeric
 * Comparison flow.
 *
 * Kable handles GATT but not bonding — bonding is an OS-level concern.
 * We call [BluetoothDevice.createBond], which surfaces the system's
 * 6-digit comparison dialog, and await the result via the
 * `ACTION_BOND_STATE_CHANGED` broadcast. The matching code on the TV is
 * rendered by the device's pairing overlay; the user compares and
 * confirms in the system dialog, not in this app.
 *
 * All methods require `BLUETOOTH_CONNECT`; callers must gate on the
 * runtime permission first.
 */
class BondManager(private val context: Context) {

    private val adapter
        get() = context.getSystemService(BluetoothManager::class.java).adapter

    @SuppressLint("MissingPermission")
    fun bondState(identifier: String): Int =
        adapter.getRemoteDevice(identifier).bondState

    fun isBonded(identifier: String): Boolean =
        bondState(identifier) == BluetoothDevice.BOND_BONDED

    /**
     * Remove the OS-level bond for [identifier] via the hidden
     * `BluetoothDevice.removeBond` API (reflection).
     *
     * Recovers from a *one-sided* bond: after the device is factory-reset
     * it calls BlueZ `remove_device`, but Android still lists the peer as
     * `BOND_BONDED`. That stale bond makes reconnects come up at GATT
     * level yet fail to encrypt, and it short-circuits [ensureBonded] so a
     * fresh pairing never starts. Clearing it lets the user re-pair
     * cleanly. Best-effort: returns `false` (logged) if the platform
     * blocks the hidden call, in which case the user must "Forget" the
     * device in system Bluetooth settings.
     */
    @SuppressLint("MissingPermission")
    fun removeBond(identifier: String): Boolean {
        val device = adapter.getRemoteDevice(identifier)
        if (device.bondState == BluetoothDevice.BOND_NONE) return true
        return runCatching {
            BluetoothDevice::class.java.getMethod("removeBond").invoke(device) as Boolean
        }.getOrElse { e ->
            Log.w(TAG, "removeBond reflection failed for $identifier", e)
            false
        }
    }

    private companion object {
        const val TAG = "ShepherdBle"

        /**
         * How long to wait for a removed bond to reach `BOND_NONE`.
         * Removal is asynchronous and `createBond` is rejected while the
         * old bond is still tearing down.
         */
        const val UNBOND_TIMEOUT_MS = 3_000L
    }

    /**
     * Bond with [identifier] from a clean slate, discarding any bond
     * Android is already holding.
     *
     * This is what the pairing flow wants, and [ensureBonded] is not.
     * `BOND_BONDED` on this side says nothing about whether the *peer*
     * still has its half: after the device is factory-reset it calls
     * BlueZ `remove_device`, and Android happily keeps listing the peer
     * as bonded. That stale bond comes up at GATT level and then fails to
     * encrypt, and because [ensureBonded] short-circuits on it, a fresh
     * pairing never starts — the user re-pairs, the app skips straight to
     * `claim` over a link that can't carry it, and pairing fails with a
     * message about something else entirely.
     *
     * Dropping and re-creating is safe here because we only get called
     * when the user is deliberately pairing, and the caller has already
     * confirmed the device reports itself Unclaimed. A device that is
     * Unclaimed has no admin, so any bond we're holding for it is either
     * stale or about to be superseded.
     *
     * If [removeBond] is blocked by the platform we carry on with the
     * existing bond: it might be fine, and failing outright would leave
     * no path forward at all.
     */
    @SuppressLint("MissingPermission")
    suspend fun ensureFreshBond(identifier: String): Boolean {
        val device = adapter.getRemoteDevice(identifier)
        if (device.bondState != BluetoothDevice.BOND_NONE) {
            Log.i(TAG, "Dropping the existing bond for $identifier before re-pairing")
            if (removeBond(identifier) && !awaitUnbonded(device)) {
                Log.w(TAG, "Bond for $identifier did not clear in time; pairing with it in place")
            }
        }
        return ensureBonded(identifier)
    }

    /**
     * Wait for [device] to reach `BOND_NONE` after a [removeBond].
     * Returns `false` on timeout — removal is asynchronous and
     * `createBond` is rejected while the old bond is still tearing down.
     */
    @SuppressLint("MissingPermission")
    private suspend fun awaitUnbonded(device: BluetoothDevice): Boolean {
        if (device.bondState == BluetoothDevice.BOND_NONE) return true
        return withTimeoutOrNull(UNBOND_TIMEOUT_MS) {
            suspendCancellableCoroutine { continuation ->
                lateinit var receiver: BroadcastReceiver
                receiver = object : BroadcastReceiver() {
                    override fun onReceive(ctx: Context, intent: Intent) {
                        if (intent.action != BluetoothDevice.ACTION_BOND_STATE_CHANGED) return
                        val changed: BluetoothDevice? =
                            intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE)
                        if (changed?.address != device.address) return
                        if (intent.getIntExtra(
                                BluetoothDevice.EXTRA_BOND_STATE,
                                BluetoothDevice.BOND_NONE,
                            ) == BluetoothDevice.BOND_NONE
                        ) {
                            runCatching { context.unregisterReceiver(receiver) }
                            if (continuation.isActive) continuation.resume(true)
                        }
                    }
                }
                context.registerReceiver(
                    receiver,
                    IntentFilter(BluetoothDevice.ACTION_BOND_STATE_CHANGED),
                )
                continuation.invokeOnCancellation {
                    runCatching { context.unregisterReceiver(receiver) }
                }
                // Removal may already have landed between the check above
                // and the receiver going live.
                if (device.bondState == BluetoothDevice.BOND_NONE) {
                    runCatching { context.unregisterReceiver(receiver) }
                    if (continuation.isActive) continuation.resume(true)
                }
            }
        } ?: false
    }

    /**
     * Ensure the device at [identifier] is bonded, initiating pairing if
     * needed. Suspends until the bond settles. Returns `true` on
     * `BOND_BONDED`, `false` if the user cancelled or pairing failed
     * (`BOND_NONE`). Cancelling the coroutine unregisters the receiver
     * but does not abort an in-flight OS pairing.
     *
     * Note this trusts an existing `BOND_BONDED` without verifying the
     * peer agrees — see [ensureFreshBond], which is what the pairing flow
     * should call.
     */
    @SuppressLint("MissingPermission")
    suspend fun ensureBonded(identifier: String): Boolean {
        val device = adapter.getRemoteDevice(identifier)
        if (device.bondState == BluetoothDevice.BOND_BONDED) return true

        return suspendCancellableCoroutine { continuation ->
            lateinit var receiver: BroadcastReceiver
            fun finish(bonded: Boolean) {
                runCatching { context.unregisterReceiver(receiver) }
                if (continuation.isActive) continuation.resume(bonded)
            }
            receiver = object : BroadcastReceiver() {
                override fun onReceive(ctx: Context, intent: Intent) {
                    if (intent.action != BluetoothDevice.ACTION_BOND_STATE_CHANGED) return
                    val changed: BluetoothDevice? =
                        intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE)
                    if (changed?.address != device.address) return
                    when (intent.getIntExtra(
                        BluetoothDevice.EXTRA_BOND_STATE,
                        BluetoothDevice.BOND_NONE,
                    )) {
                        BluetoothDevice.BOND_BONDED -> finish(true)
                        BluetoothDevice.BOND_NONE -> finish(false)
                        else -> Unit // BOND_BONDING — keep waiting
                    }
                }
            }
            context.registerReceiver(
                receiver,
                IntentFilter(BluetoothDevice.ACTION_BOND_STATE_CHANGED),
            )
            continuation.invokeOnCancellation {
                runCatching { context.unregisterReceiver(receiver) }
            }

            // createBond returns false if it could not even start; treat
            // that as immediate failure.
            if (!device.createBond()) finish(false)
        }
    }
}
