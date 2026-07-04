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
fun DeviceControlsScreen(vm: ShepherdViewModel, onBack: () -> Unit) {
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
            state.brightness?.let { BrightnessCard(it, vm) }

            Card(Modifier.fillMaxWidth()) {
                Column(androidx.compose.ui.Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Maintenance", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Button(onClick = vm::reloadConfig, modifier = Modifier.fillMaxWidth()) { Text("Reload config") }
                    OutlinedButton(onClick = vm::logoutDevice, modifier = Modifier.fillMaxWidth()) {
                        Text("Log out device session")
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
        }
    }
}
