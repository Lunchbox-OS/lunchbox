package com.armeafamily.shepherd.companion.ui.device

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.AudioOutputKind
import com.armeafamily.shepherd.companion.domain.AudioOutputRecord
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel

/** Default cap offered when a limit is first switched on. */
private const val DEFAULT_CAP = 50f

/**
 * Per-output volume limits (issue #124): a lower cap for headphones than for
 * speakers.
 *
 * Rows come from discovery on the device rather than from configuration —
 * audio hardware identifies itself in ways that cannot be predicted ahead of
 * time — so a parent plugs the device in once and then sets a limit on the row
 * they recognise.
 */
@Composable
fun AudioOutputsCard(outputs: List<AudioOutputRecord>, vm: ShepherdViewModel) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                "Volume limits per device",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                "Set a different maximum for headphones than for speakers. Devices " +
                    "appear here once they have been used. The strictest limit always " +
                    "applies — a per-device limit can lower the overall limit but " +
                    "never raise it.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            if (outputs.isEmpty()) {
                Text(
                    "No audio devices seen yet.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                outputs.forEachIndexed { index, record ->
                    if (index > 0) HorizontalDivider()
                    OutputRow(record, vm)
                }
            }
        }
    }
}

@Composable
private fun OutputRow(record: AudioOutputRecord, vm: ShepherdViewModel) {
    val capped = record.maxVolume != null
    // Local slider state so dragging is smooth; committed on release.
    var draft by remember(record.maxVolume) {
        mutableFloatStateOf(record.maxVolume?.toFloat() ?: DEFAULT_CAP)
    }

    Column(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(
                    record.output.description.ifBlank { record.output.key },
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    kindLabel(record.output.kind),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (record.active) {
                AssistChip(
                    onClick = {},
                    enabled = false,
                    label = { Text("In use now") },
                    colors = AssistChipDefaults.assistChipColors(
                        disabledLabelColor = MaterialTheme.colorScheme.primary,
                    ),
                )
            }
        }

        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Switch(
                checked = capped,
                onCheckedChange = { on ->
                    vm.setAudioOutputLimit(record.output.key, if (on) draft.toInt() else null)
                },
            )
            Slider(
                value = if (capped) draft else 100f,
                onValueChange = { draft = it },
                onValueChangeFinished = {
                    vm.setAudioOutputLimit(record.output.key, draft.toInt())
                },
                valueRange = 0f..100f,
                enabled = capped,
                modifier = Modifier.weight(1f).padding(horizontal = 8.dp),
            )
            Text(
                if (capped) "Max ${draft.toInt()}%" else "No limit",
                style = MaterialTheme.typography.bodySmall,
                color = if (capped) {
                    MaterialTheme.colorScheme.onSurface
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
            )
        }

        // Forgetting the device in use would only rediscover it on the next tick.
        if (!record.active) {
            TextButton(onClick = { vm.forgetAudioOutput(record.output.key) }) {
                Text("Forget this device")
            }
        }
    }
}

/**
 * The kind is advisory and is often [AudioOutputKind.UNKNOWN] — a plain USB
 * interface reports nothing that identifies it. The description above is what a
 * parent actually recognises the device by, so an unknown kind gets a neutral
 * label rather than a guess.
 */
private fun kindLabel(kind: AudioOutputKind): String = when (kind) {
    AudioOutputKind.HEADPHONES -> "Headphones"
    AudioOutputKind.SPEAKERS -> "Speakers"
    AudioOutputKind.HDMI -> "HDMI / TV"
    AudioOutputKind.BLUETOOTH -> "Bluetooth"
    AudioOutputKind.LINE_OUT -> "Line out"
    AudioOutputKind.DIGITAL -> "Digital out"
    AudioOutputKind.UNKNOWN -> "Audio device"
}
