package com.armeafamily.shepherd.companion.ui.device

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.BrightnessInfo
import com.armeafamily.shepherd.companion.domain.VolumeInfo
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeviceControlsScreen(
    vm: ShepherdViewModel,
    onBack: () -> Unit,
    onOpenWindows: () -> Unit,
    onOpenHealth: () -> Unit,
    onOpenNetwork: () -> Unit,
    onOpenWebAccess: () -> Unit,
) {
    val state by vm.state.collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Device controls") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.padding(padding).padding(16.dp).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            state.volume?.let { VolumeCard(it, vm) }
            // Gated the same way as VolumeCard: on a device with no sound
            // backend the per-device list is noise, not an empty state.
            if (state.volume?.available == true) {
                AudioOutputsCard(state.audioOutputs, state.audioBusy, vm)
            }
            state.brightness?.let { BrightnessCard(it, vm) }

            Card(Modifier.fillMaxWidth()) {
                Column(androidx.compose.ui.Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Maintenance", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Button(onClick = vm::reloadConfig, modifier = Modifier.fillMaxWidth()) { Text("Reload config") }
                    // Next to "reload config" because it is the same gesture
                    // for the other half of what a parent changes: the config
                    // file is one source of what the child sees, the playlist
                    // behind a media activity is the other, and only the first
                    // had a button (issue #165).
                    OutlinedButton(onClick = vm::refreshMedia, modifier = Modifier.fillMaxWidth()) {
                        Text("Refresh media")
                    }
                    OutlinedButton(onClick = vm::logoutDevice, modifier = Modifier.fillMaxWidth()) {
                        Text("Log out device session")
                    }
                    // Below "log out the session" on purpose: ending the
                    // session is the blunt fix for a stuck window, and
                    // reaching for individual windows should be the step
                    // taken after it isn't enough (issue #140).
                    OutlinedButton(onClick = onOpenWindows, modifier = Modifier.fillMaxWidth()) {
                        Text("Windows…")
                    }
                    // Reachable even when nothing is wrong, so a caregiver can
                    // confirm the device is healthy rather than only ever
                    // seeing this when it isn't (issue #143).
                    OutlinedButton(onClick = onOpenHealth, modifier = Modifier.fillMaxWidth()) {
                        Text("Device health…")
                    }
                    // The one screen here that is useful precisely when the
                    // network path is broken: this app is on Bluetooth, so it
                    // can still say where the device is and whether its web
                    // interface came up at all (issue #182).
                    OutlinedButton(onClick = onOpenNetwork, modifier = Modifier.fillMaxWidth()) {
                        Text("Network…")
                    }
                    // Approving a browser, and setting the web password (issue
                    // #156). Here rather than in Settings because it is a thing
                    // done *to the device* in the moment — a laptop in the next
                    // room is waiting — not a preference about this phone.
                    OutlinedButton(onClick = onOpenWebAccess, modifier = Modifier.fillMaxWidth()) {
                        Text("Web access…")
                    }
                }
            }
        }
    }
}

@Composable
private fun VolumeCard(volume: VolumeInfo, vm: ShepherdViewModel) {
    if (!volume.available || !volume.restrictions.allowChange) return
    val min = (volume.restrictions.minVolume ?: 0).toFloat()
    val max = (volume.restrictions.maxVolume ?: 100).toFloat()
    var slider by remember(volume.percent) { mutableFloatStateOf(volume.percent.toFloat()) }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                Text("Volume", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                Text("${slider.toInt()}%")
            }
            Slider(
                value = slider,
                onValueChange = { slider = it },
                onValueChangeFinished = { vm.setVolume(slider.toInt()) },
                valueRange = min..max,
            )
            if (volume.restrictions.allowMute) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                    Text("Muted")
                    Switch(checked = volume.muted, onCheckedChange = { vm.setMute(it) })
                }
            }
        }
    }
}

@Composable
private fun BrightnessCard(brightness: BrightnessInfo, vm: ShepherdViewModel) {
    if (!brightness.available) return
    val enabled = brightness.restrictions.allowChange
    val min = (brightness.restrictions.minBrightness ?: 0).toFloat()
    val max = (brightness.restrictions.maxBrightness ?: 100).toFloat()
    var slider by remember(brightness.percent) { mutableFloatStateOf(brightness.percent.toFloat()) }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                Text("Brightness", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                Text("${slider.toInt()}%")
            }
            Slider(
                value = slider,
                onValueChange = { slider = it },
                onValueChangeFinished = { vm.setBrightness(slider.toInt()) },
                valueRange = min..max,
                enabled = enabled,
            )
            if (brightness.autoAvailable) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                    Text("Automatic")
                    Switch(checked = brightness.autoEnabled, onCheckedChange = { vm.setAutoBrightness(it) })
                }
            }
        }
    }
}
