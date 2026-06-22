package com.shepherd.companion.ble

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import kotlinx.coroutines.suspendCancellableCoroutine
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
     * Ensure the device at [identifier] is bonded, initiating pairing if
     * needed. Suspends until the bond settles. Returns `true` on
     * `BOND_BONDED`, `false` if the user cancelled or pairing failed
     * (`BOND_NONE`). Cancelling the coroutine unregisters the receiver
     * but does not abort an in-flight OS pairing.
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
