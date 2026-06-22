package com.shepherd.companion.ui.pairing

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Bluetooth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import com.shepherd.companion.ble.DiscoveredDevice
import com.shepherd.companion.ui.PairingPhase
import com.shepherd.companion.ui.ShepherdViewModel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingScreen(
    vm: ShepherdViewModel,
    onDone: () -> Unit,
    onBack: () -> Unit,
) {
    val phase by vm.pairing.collectAsState()
    val discovered = remember { mutableStateMapOf<String, DiscoveredDevice>() }
    var phoneName by rememberSaveable { mutableStateOf(vm.defaultPhoneName) }

    // Scan only while idle, and only while this screen is composed; the
    // flow is cold so leaving the screen stops the system scanner.
    LaunchedEffect(phase is PairingPhase.Idle) {
        if (phase is PairingPhase.Idle) {
            vm.scan().collect { discovered[it.identifier] = it }
        }
    }

    // Reset pairing state when the screen is first shown.
    LaunchedEffect(Unit) { vm.resetPairing() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Pair a device") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedTextField(
                value = phoneName,
                onValueChange = { phoneName = it },
                label = { Text("This phone's name") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                "Make sure the shepherd device's TV is on — you'll compare a 6-digit code during pairing.",
                style = MaterialTheme.typography.bodySmall,
            )
            if (discovered.isEmpty()) {
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        CircularProgressIndicator(strokeWidth = 2.dp)
                        Text("Scanning for shepherd devices…")
                    }
                }
            }
            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(discovered.values.sortedByDescending { it.rssi }, key = { it.identifier }) { device ->
                    Card(
                        onClick = { vm.pair(device.identifier, phoneName.ifBlank { vm.defaultPhoneName }) },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        ListItem(
                            leadingContent = { Icon(Icons.Filled.Bluetooth, contentDescription = null) },
                            headlineContent = { Text(device.name ?: "shepherd") },
                            supportingContent = {
                                Text("${device.identifier}  ·  ${device.rssi} dBm", fontFamily = FontFamily.Monospace)
                            },
                        )
                    }
                }
            }
        }
    }

    PairingDialog(phase = phase, onDone = onDone, onDismiss = { vm.resetPairing() })
}

@Composable
private fun PairingDialog(phase: PairingPhase, onDone: () -> Unit, onDismiss: () -> Unit) {
    when (phase) {
        is PairingPhase.Idle -> Unit

        is PairingPhase.Connecting -> ProgressDialog("Connecting", "Reading device info…")

        is PairingPhase.Comparing -> AlertDialog(
            onDismissRequest = {},
            confirmButton = {},
            title = { Text("Compare the code") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        "Your phone will show a 6-digit number. If it matches the number on the TV, tap PAIR in the system dialog. Otherwise tap CANCEL.",
                    )
                    Text("Device: ${phase.mac}", fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
                }
            },
        )

        is PairingPhase.Claiming -> ProgressDialog("Finishing", "Claiming the device…")

        is PairingPhase.Success -> AlertDialog(
            onDismissRequest = onDone,
            confirmButton = { TextButton(onClick = onDone) { Text("Done") } },
            title = { Text("Paired") },
            text = { Text("${phase.record.displayName} is now paired with this phone.") },
        )

        is PairingPhase.Failed -> AlertDialog(
            onDismissRequest = onDismiss,
            confirmButton = { TextButton(onClick = onDismiss) { Text("OK") } },
            title = { Text(if (phase.alreadyClaimed) "Already paired" else "Pairing failed") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(phase.reason)
                    if (phase.alreadyClaimed) {
                        Text(
                            "To take over, factory-reset the device's bond (SSH in and touch the reset sentinel, then restart shepherdd) and scan again.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            },
        )
    }
}

@Composable
private fun ProgressDialog(title: String, body: String) {
    AlertDialog(
        onDismissRequest = {},
        confirmButton = {},
        title = { Text(title) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                CircularProgressIndicator(strokeWidth = 2.dp)
                Text(body)
            }
        },
    )
}
