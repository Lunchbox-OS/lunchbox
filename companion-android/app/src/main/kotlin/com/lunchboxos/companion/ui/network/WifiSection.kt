package com.lunchboxos.companion.ui.network

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.LockOpen
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.SignalWifi4Bar
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.lunchboxos.companion.domain.SavedWifiNetwork
import com.lunchboxos.companion.domain.WifiJoinFailureKind
import com.lunchboxos.companion.domain.WifiJoinState
import com.lunchboxos.companion.domain.WifiNetwork
import com.lunchboxos.companion.domain.WifiSecurity
import com.lunchboxos.companion.ui.DeviceViewModel
import com.lunchboxos.companion.ui.WifiUiState

/**
 * Choosing a wireless network from the phone (issue #194).
 *
 * **This is the primary path for the feature, not the secondary one.** A
 * parent reaches for this screen when the device has no network — and
 * Bluetooth is the only management transport that still works then. The web UI
 * cannot be opened at all in that state, which is why it leads with "save for
 * later" and this leads with joining.
 *
 * Everything here polls rather than waits. A join takes 3 to 45 seconds
 * against this app's 15-second RPC timeout, so the device answers "accepted"
 * immediately and the outcome arrives on a later read.
 */

private fun securityLabel(security: WifiSecurity): String = when (security) {
    WifiSecurity.OPEN -> "Open"
    WifiSecurity.OWE -> "Enhanced Open"
    WifiSecurity.WPA_PSK -> "WPA2"
    WifiSecurity.SAE -> "WPA3"
    WifiSecurity.ENTERPRISE -> "Enterprise"
    else -> "WEP"
}

/** Whether this kind of network can be joined from here. */
private fun joinable(security: WifiSecurity): Boolean =
    security != WifiSecurity.ENTERPRISE && security != WifiSecurity.WEP

private fun needsPassword(security: WifiSecurity): Boolean =
    security == WifiSecurity.WPA_PSK || security == WifiSecurity.SAE

/** What a manual entry may choose. Enterprise and WEP are not written here. */
private val MANUAL_SECURITY = listOf(
    WifiSecurity.WPA_PSK to "WPA/WPA2 Personal",
    WifiSecurity.SAE to "WPA3 Personal",
    WifiSecurity.OPEN to "None (open network)",
)

/**
 * What a failed join means, as something to do about it.
 *
 * The first two are the point of the whole reason mapping: a network that
 * accepted the password and never handed out an address is a router problem,
 * and a parent told "wrong password" there retypes a correct one until they
 * give up.
 */
private fun joinFailureText(state: WifiJoinState.Failed): String = when (state.reason.kind) {
    WifiJoinFailureKind.WRONG_PASSWORD ->
        "The password for ${state.ssid} was refused. Check it and try again."
    WifiJoinFailureKind.NO_ADDRESS ->
        "${state.ssid} accepted the password but never gave the device an address. " +
            "Check the router."
    WifiJoinFailureKind.NOT_FOUND ->
        "No network called ${state.ssid} answered. It may be out of range, switched off, " +
            "or hidden — a hidden network has to be added by hand."
    WifiJoinFailureKind.NOT_AUTHORIZED ->
        "This device is not allowed to change Wi-Fi settings."
    else -> state.reason.detail?.let { "Couldn't join ${state.ssid}: $it" }
        ?: "Couldn't join ${state.ssid}."
}

/** The sheet for joining one network picked out of the scan. */
@Composable
private fun JoinDialog(
    network: WifiNetwork,
    busy: Boolean,
    onDismiss: () -> Unit,
    onJoin: (String?) -> Unit,
) {
    var password by remember { mutableStateOf("") }
    val wantsPassword = needsPassword(network.security)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(network.ssid) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    securityLabel(network.security) +
                        network.bandsGhz.joinToString("") { " · $it GHz" },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (wantsPassword) {
                    OutlinedTextField(
                        value = password,
                        onValueChange = { password = it },
                        label = { Text("Password") },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                Text(
                    "The device will leave its current network. This app stays connected " +
                        "over Bluetooth either way.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = {
            TextButton(
                enabled = !busy && (!wantsPassword || password.isNotEmpty()),
                onClick = { onJoin(if (wantsPassword) password else null) },
            ) { Text("Join") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** The sheet for a network that does not broadcast its name. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ManualDialog(
    busy: Boolean,
    onDismiss: () -> Unit,
    onSave: (String, WifiSecurity, String?, Boolean) -> Unit,
) {
    var ssid by remember { mutableStateOf("") }
    var security by remember { mutableStateOf(WifiSecurity.WPA_PSK) }
    var password by remember { mutableStateOf("") }
    var hidden by remember { mutableStateOf(true) }
    var expanded by remember { mutableStateOf(false) }

    val wantsPassword = needsPassword(security)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Other network") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = ssid,
                    onValueChange = { ssid = it },
                    label = { Text("Network name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                ExposedDropdownMenuBox(
                    expanded = expanded,
                    onExpandedChange = { expanded = it },
                ) {
                    OutlinedTextField(
                        value = MANUAL_SECURITY.first { it.first == security }.second,
                        onValueChange = {},
                        readOnly = true,
                        label = { Text("Security") },
                        trailingIcon = {
                            ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded)
                        },
                        modifier = Modifier
                            .menuAnchor(androidx.compose.material3.MenuAnchorType.PrimaryNotEditable)
                            .fillMaxWidth(),
                    )
                    ExposedDropdownMenu(
                        expanded = expanded,
                        onDismissRequest = { expanded = false },
                    ) {
                        MANUAL_SECURITY.forEach { (value, label) ->
                            DropdownMenuItem(
                                text = { Text(label) },
                                onClick = { security = value; expanded = false },
                            )
                        }
                    }
                }
                if (wantsPassword) {
                    OutlinedTextField(
                        value = password,
                        onValueChange = { password = it },
                        label = { Text("Password") },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Switch(checked = hidden, onCheckedChange = { hidden = it })
                    Text(
                        "Doesn't broadcast its name",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                enabled = !busy && ssid.isNotBlank() &&
                    (!wantsPassword || password.isNotEmpty()),
                onClick = {
                    onSave(
                        ssid.trim(),
                        security,
                        if (wantsPassword) password else null,
                        hidden,
                    )
                },
            ) { Text("Join") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun SavedRow(
    network: SavedWifiNetwork,
    busy: Boolean,
    onConnect: () -> Unit,
    onForget: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(network.ssid, style = MaterialTheme.typography.bodyMedium)
            Text(
                securityLabel(network.security) + if (network.hidden) " · Hidden" else "",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (network.active) {
            AssistChip(onClick = {}, enabled = false, label = { Text("Connected") })
        } else {
            TextButton(onClick = onConnect, enabled = !busy) { Text("Connect") }
        }
        TextButton(onClick = onForget, enabled = !busy) { Text("Forget") }
    }
}

/**
 * The Wi-Fi card on the network screen.
 *
 * Kept in its own file rather than added to `NetworkScreen.kt` because that
 * file is already 470 lines of address rendering, and this is a different
 * job — reading where the device is, versus changing it.
 */
@Composable
fun WifiSection(vm: DeviceViewModel, wifi: WifiUiState) {
    var selected by remember { mutableStateOf<WifiNetwork?>(null) }
    var manual by remember { mutableStateOf(false) }

    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "Wi-Fi",
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = vm::scanWifi, enabled = !wifi.busy) {
                    if (wifi.loading) {
                        CircularProgressIndicator(
                            strokeWidth = 2.dp,
                            modifier = Modifier.size(20.dp),
                        )
                    } else {
                        Icon(Icons.Filled.Refresh, contentDescription = "Scan again")
                    }
                }
            }

            val scan = wifi.scan
            if (scan == null) {
                Text(
                    if (wifi.loading) "Reading Wi-Fi…" else "Nothing read yet.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                return@Column
            }

            // "No adapter" and "no networks in range" are different answers,
            // and showing the second for the first sends somebody walking
            // around the house.
            if (!scan.supported) {
                Text(
                    "This device has no Wi-Fi adapter.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                return@Column
            }

            wifi.error?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
            }

            if (!scan.radioEnabled) {
                Text(
                    "The Wi-Fi radio is switched off on the device.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            if (!scan.canConfigure) {
                Text(
                    "This device can't save a new network — it can still join ones it " +
                        "already knows. The Health screen says what's missing.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            when (val join = scan.join) {
                is WifiJoinState.Connecting -> Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(16.dp))
                    Text(
                        "Connecting to ${join.ssid}… this can take up to a minute.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                is WifiJoinState.Connected -> Text(
                    "Connected to ${join.ssid}.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.primary,
                )
                is WifiJoinState.Failed -> Text(
                    joinFailureText(join),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                else -> Unit
            }

            if (wifi.networks.isEmpty()) {
                Text(
                    "No networks found yet.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            wifi.networks.forEach { network ->
                val canJoin = joinable(network.security) && scan.canConfigure
                Row(
                    Modifier
                        .fillMaxWidth()
                        .then(
                            if (canJoin && !network.active) {
                                Modifier.clickable { selected = network }
                            } else {
                                Modifier
                            }
                        ),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        Icons.Filled.SignalWifi4Bar,
                        contentDescription = "Signal ${network.signalPercent} percent",
                        modifier = Modifier.size(20.dp),
                    )
                    Icon(
                        if (network.security == WifiSecurity.OPEN) {
                            Icons.Filled.LockOpen
                        } else {
                            Icons.Filled.Lock
                        },
                        contentDescription = securityLabel(network.security),
                        modifier = Modifier.size(16.dp),
                    )
                    Column(Modifier.weight(1f)) {
                        Text(network.ssid, style = MaterialTheme.typography.bodyMedium)
                        Text(
                            buildString {
                                append("${network.signalPercent}%")
                                if (network.saved) append(" · Saved")
                                // Shown rather than hidden: a network missing
                                // from the list reads as a device that cannot
                                // see it.
                                if (!joinable(network.security)) {
                                    append(" · Not supported here")
                                }
                            },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    if (network.active) {
                        AssistChip(onClick = {}, enabled = false, label = { Text("Connected") })
                    }
                }
            }

            TextButton(
                onClick = { manual = true },
                enabled = !wifi.busy && scan.canConfigure,
            ) { Text("Other network…") }

            if (wifi.saved.isNotEmpty()) {
                Text(
                    "Saved networks",
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.padding(top = 8.dp),
                )
                wifi.saved.forEach { network ->
                    SavedRow(
                        network = network,
                        busy = wifi.busy,
                        onConnect = { vm.connectWifi(network.id) },
                        onForget = { vm.forgetWifi(network.id) },
                    )
                }
            }
        }
    }

    selected?.let { network ->
        // Keyed on the network so `remember` inside the dialog starts fresh:
        // without it a password typed for one network, then cancelled, is
        // still in the box when a different one is picked.
        key(network.ssid, network.security) {
        JoinDialog(
            network = network,
            busy = wifi.busy,
            onDismiss = { selected = null },
            onJoin = { password ->
                vm.saveWifi(
                    ssid = network.ssid,
                    security = network.security,
                    password = password,
                    hidden = false,
                    connect = true,
                )
                selected = null
            },
        )
        }
    }

    if (manual) {
        ManualDialog(
            busy = wifi.busy,
            onDismiss = { manual = false },
            onSave = { ssid, security, password, hidden ->
                vm.saveWifi(
                    ssid = ssid,
                    security = security,
                    password = password,
                    hidden = hidden,
                    connect = true,
                )
                manual = false
            },
        )
    }
}
