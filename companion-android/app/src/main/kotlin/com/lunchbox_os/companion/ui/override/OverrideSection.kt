package com.lunchbox_os.companion.ui.override

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.lunchbox_os.companion.domain.DailyOverride
import com.lunchbox_os.companion.ui.DeviceViewModel
import kotlinx.coroutines.launch
import java.time.LocalDate

/**
 * Today's override editor: availability tri-state + quota delta stepper.
 *
 * Keyed by limit subject, so it edits an activity's override or a whole
 * category's (`group:<id>`) with no other difference (issue #5).
 */
@Composable
internal fun OverrideSection(vm: DeviceViewModel, subject: String) {
    val today = remember { LocalDate.now().toString() }
    val scope = rememberCoroutineScope()
    var loaded by remember(subject) { mutableStateOf<DailyOverride?>(null) }
    var availability by remember(subject) { mutableStateOf<Boolean?>(null) }
    var quotaDeltaMinutes by remember(subject) { mutableStateOf(0) }
    // Distinct from "no override set": if the lookup failed we must not offer
    // an empty form, or saving from it would silently clobber a real override.
    var loadFailed by remember(subject) { mutableStateOf(false) }

    suspend fun reload() {
        val result = vm.loadOverride(subject, today)
        loadFailed = result.isFailure
        val ov = result.getOrNull()
        loaded = ov
        availability = ov?.availability
        quotaDeltaMinutes = ((ov?.quotaDeltaSeconds ?: 0) / 60).toInt()
    }
    LaunchedEffect(subject) { reload() }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Today's override", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)

            if (loadFailed) {
                Text(
                    "Couldn't read today's override, so it can't be edited safely. " +
                        "Reopen this screen to retry.",
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            Text("Availability", style = MaterialTheme.typography.labelMedium)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(selected = availability == true, onClick = { availability = true }, label = { Text("Allow") })
                FilterChip(selected = availability == false, onClick = { availability = false }, label = { Text("Block") })
                FilterChip(selected = availability == null, onClick = { availability = null }, label = { Text("No change") })
            }

            Text("Quota adjustment: ${quotaDeltaMinutes} min", style = MaterialTheme.typography.labelMedium)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                OutlinedButton(onClick = { quotaDeltaMinutes -= 5 }) { Text("−5") }
                OutlinedButton(onClick = { quotaDeltaMinutes += 5 }) { Text("+5") }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    modifier = Modifier.weight(1f),
                    enabled = !loadFailed && (availability != null || quotaDeltaMinutes != 0),
                    onClick = {
                        // Re-read the persisted override after a save so
                        // the Clear button reflects the freshly-created
                        // record — otherwise a first-time save leaves
                        // `loaded == null` and Clear stays disabled.
                        vm.upsertOverride(
                            id = subject,
                            date = today,
                            availability = availability,
                            quotaDeltaSeconds = if (quotaDeltaMinutes != 0) quotaDeltaMinutes * 60L else null,
                            onDone = { scope.launch { reload() } },
                        )
                    },
                ) { Text("Save") }
                OutlinedButton(
                    modifier = Modifier.weight(1f),
                    // Enable Clear whenever there's *something* to
                    // clear — either the persisted record (`loaded`)
                    // or unsaved local edits. Users hit this when
                    // they've dialed in an override and then decide
                    // to back out without saving; the web version
                    // handles the same case via a single "Clear
                    // Override" button that resets both.
                    enabled = !loadFailed
                        && (loaded != null
                            || availability != null
                            || quotaDeltaMinutes != 0),
                    onClick = {
                        if (loaded != null) {
                            vm.deleteOverride(subject, today, onDone = {
                                loaded = null
                                availability = null
                                quotaDeltaMinutes = 0
                            })
                        } else {
                            // Nothing persisted yet — just discard
                            // the local edits, no server round-trip.
                            availability = null
                            quotaDeltaMinutes = 0
                        }
                    },
                ) { Text("Clear") }
            }
        }
    }
}

