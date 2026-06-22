package com.shepherd.companion.ui

import android.Manifest
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner as ComposeLocalLifecycleOwner
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.compose.runtime.DisposableEffect
import com.google.accompanist.permissions.ExperimentalPermissionsApi
import com.google.accompanist.permissions.rememberMultiplePermissionsState
import com.shepherd.companion.ui.device.DeviceControlsScreen
import com.shepherd.companion.ui.entry.EntryDetailScreen
import com.shepherd.companion.ui.home.HomeScreen
import com.shepherd.companion.ui.pairing.PairingScreen
import com.shepherd.companion.ui.settings.SettingsScreen

object Routes {
    const val HOME = "home"
    const val PAIR = "pair"
    const val CONTROLS = "controls"
    const val SETTINGS = "settings"
    const val ENTRY = "entry"
    fun entry(id: String) = "$ENTRY/$id"
}

@OptIn(ExperimentalPermissionsApi::class)
@Composable
fun App() {
    val vm: ShepherdViewModel = viewModel()

    // Bind the BLE session to the foreground lifecycle: connect on START,
    // disconnect on STOP. No background connections.
    val lifecycleOwner = ComposeLocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> vm.onForeground()
                Lifecycle.Event.ON_STOP -> vm.onBackground()
                else -> Unit
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    val permissions = rememberMultiplePermissionsState(
        listOf(Manifest.permission.BLUETOOTH_SCAN, Manifest.permission.BLUETOOTH_CONNECT),
    )

    if (!permissions.allPermissionsGranted) {
        PermissionGate(onRequest = { permissions.launchMultiplePermissionRequest() })
        return
    }

    val snackbarHostState = remember { SnackbarHostState() }
    val message by vm.message.collectAsState()
    LaunchedEffect(message) {
        message?.let {
            snackbarHostState.showSnackbar(it)
            vm.consumeMessage()
        }
    }

    val navController = rememberNavController()
    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { padding ->
        NavHost(
            navController = navController,
            startDestination = Routes.HOME,
            modifier = Modifier.padding(padding),
        ) {
            composable(Routes.HOME) {
                HomeScreen(
                    vm = vm,
                    onAddDevice = { navController.navigate(Routes.PAIR) },
                    onOpenEntry = { id -> navController.navigate(Routes.entry(id)) },
                    onOpenControls = { navController.navigate(Routes.CONTROLS) },
                    onOpenSettings = { navController.navigate(Routes.SETTINGS) },
                )
            }
            composable(Routes.PAIR) {
                PairingScreen(
                    vm = vm,
                    onDone = { navController.popBackStack(Routes.HOME, inclusive = false) },
                    onBack = { navController.popBackStack() },
                )
            }
            composable("${Routes.ENTRY}/{entryId}") { backStackEntry ->
                val entryId = backStackEntry.arguments?.getString("entryId").orEmpty()
                EntryDetailScreen(
                    vm = vm,
                    entryId = entryId,
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.CONTROLS) {
                DeviceControlsScreen(vm = vm, onBack = { navController.popBackStack() })
            }
            composable(Routes.SETTINGS) {
                SettingsScreen(
                    vm = vm,
                    onBack = { navController.popBackStack() },
                    onAllForgotten = { navController.popBackStack(Routes.HOME, inclusive = false) },
                )
            }
        }
    }
}

@Composable
private fun PermissionGate(onRequest: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            "Bluetooth permission needed",
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
        )
        Text(
            "Your phone needs to talk to the shepherd device over Bluetooth. " +
                "The app only scans for shepherd devices and never uses your location.",
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
        )
        Button(onClick = onRequest) { Text("Grant Bluetooth access") }
    }
}
