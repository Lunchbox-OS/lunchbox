package com.lunchboxos.companion

import android.app.Application
import android.content.Context
import com.lunchboxos.companion.ble.BondManager
import com.lunchboxos.companion.ble.DeviceScanner
import com.lunchboxos.companion.domain.DeviceRepository
import com.lunchboxos.companion.persistence.AdminRecordStore

/**
 * Manual dependency container. The surface is small enough that a DI
 * framework would be overhead; everything is wired here and reached via
 * [Application]. No singletons hold a BLE connection — connections are
 * owned by the ViewModel and torn down when a screen leaves the
 * foreground.
 */
class AppContainer(context: Context) {
    val recordStore = AdminRecordStore(context)
    val repository = DeviceRepository(recordStore)
    val scanner = DeviceScanner()
    val bondManager = BondManager(context)
}

class CompanionApp : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
    }
}

/** Reach the [AppContainer] from a [Context]. */
val Context.appContainer: AppContainer
    get() = (applicationContext as CompanionApp).container
