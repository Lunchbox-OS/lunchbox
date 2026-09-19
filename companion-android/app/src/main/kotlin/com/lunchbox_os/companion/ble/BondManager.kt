package com.lunchbox_os.companion.ble

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

    /**
     * Remove the bond for [identifier] and wait for it to clear.
     *
     * **Drops any GATT connection to that peer as a side effect**, so the
     * caller must not be holding a link it still needs — close it first
     * and reconnect afterwards. Doing this underneath a live pairing
     * connection killed the link that the subsequent bond and claim ran
     * over, surfacing as "pairing was cancelled or failed" with nothing
     * having been cancelled.
     *
     * Best-effort, like [removeBond]: a platform that blocks the hidden
     * call, or a removal that doesn't settle in time, is logged and the
     * caller carries on with the bond in place — it might still work, and
     * failing outright would leave no path forward.
     */
    @SuppressLint("MissingPermission")
    suspend fun dropBond(identifier: String) {
        val device = adapter.getRemoteDevice(identifier)
        if (device.bondState == BluetoothDevice.BOND_NONE) return
        Log.i(TAG, "Dropping the pre-existing bond for $identifier before re-pairing")
        if (removeBond(identifier) && !awaitUnbonded(device)) {
            Log.w(TAG, "Bond for $identifier did not clear in time; pairing with it in place")
        }
    }

    private companion object {
        const val TAG = "LunchboxBle"

        /**
         * How long to wait for a removed bond to reach `BOND_NONE`.
         * Removal is asynchronous and `createBond` is rejected while the
         * old bond is still tearing down.
         */
        const val UNBOND_TIMEOUT_MS = 3_000L
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
     * Trusts an existing `BOND_BONDED` and returns immediately. That is
     * only correct when the peer is known to still hold its half; when it
     * may not — a device that was factory-reset keeps Android listing a
     * bond it has itself forgotten — the caller must [dropBond] first.
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
