package com.armeafamily.shepherd.companion.ui.entry

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.DailyOverride
import com.armeafamily.shepherd.companion.domain.EntryView
import com.armeafamily.shepherd.companion.domain.SessionInfo
import com.armeafamily.shepherd.companion.domain.UsageStat
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel
import com.armeafamily.shepherd.companion.util.Formatting
import com.armeafamily.shepherd.companion.util.ReasonText
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.time.LocalDate

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EntryDetailScreen(
    vm: ShepherdViewModel,
    entryId: String,
    onBack: () -> Unit,
) {
    val state by vm.state.collectAsState()
    val entry = state.entries.firstOrNull { it.entryId == entryId }
    val session = state.currentSession?.takeIf { it.entryId == entryId }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(entry?.label ?: entryId) },
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
            if (entry == null) {
                Text("This activity is no longer available.")
                return@Column
            }

            Text(
                entry.kindTag.name.lowercase().replaceFirstChar { it.uppercase() },
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.outline,
            )

            if (session != null) {
                SessionSection(session, onExtend = vm::extendCurrent, onStop = vm::stopCurrent)
            } else {
                LaunchSection(entry, onLaunch = { vm.launchEntry(entry.entryId) })
            }

            OverrideSection(vm, entryId)
            UsageSection(vm, entryId)
        }
    }
}

@Composable
private fun LaunchSection(entry: EntryView, onLaunch: () -> Unit) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            val limit = entry.maxRunIfStartedNow
            Text(
                when {
                    !entry.enabled -> "Not available right now"
                    limit != null -> "Up to ${Formatting.coarse(limit.secs)} if started now"
                    else -> "No time limit"
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            entry.reasons.firstOrNull()?.let {
                Text(ReasonText.describe(it), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
            }
            Button(onClick = onLaunch, enabled = entry.enabled, modifier = Modifier.fillMaxWidth()) {
                Text("Launch")
            }
        }
    }
}

@Composable
private fun SessionSection(session: SessionInfo, onExtend: (Long) -> Unit, onStop: () -> Unit) {
    // Live countdown derived from the deadline; falls back to the
    // snapshot's time_remaining for unlimited or clock-skew cases.
    var remaining by remember(session.deadline, session.sessionId) {
        mutableStateOf(Formatting.secondsUntil(session.deadline) ?: session.timeRemaining?.secs)
    }
    LaunchedEffect(session.deadline, session.sessionId) {
        while (session.deadline != null) {
            remaining = Formatting.secondsUntil(session.deadline)
            delay(1000)
        }
    }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Running", style = MaterialTheme.typography.labelMedium)
            Text(
                remaining?.let(Formatting::hms) ?: "No time limit",
                style = MaterialTheme.typography.displaySmall,
                fontWeight = FontWeight.Bold,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { onExtend(-600) }, modifier = Modifier.weight(1f)) { Text("−10 min") }
                OutlinedButton(onClick = { onExtend(600) }, modifier = Modifier.weight(1f)) { Text("+10 min") }
            }
            Button(
                onClick = onStop,
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Stop") }
        }
    }
}

/** Today's override editor: availability tri-state + quota delta stepper. */
@Composable
private fun OverrideSection(vm: ShepherdViewModel, entryId: String) {
    val today = remember { LocalDate.now().toString() }
    val scope = rememberCoroutineScope()
    var loaded by remember(entryId) { mutableStateOf<DailyOverride?>(null) }
    var availability by remember(entryId) { mutableStateOf<Boolean?>(null) }
    var quotaDeltaMinutes by remember(entryId) { mutableStateOf(0) }

    suspend fun reload() {
        val ov = vm.loadOverride(entryId, today)
        loaded = ov
        availability = ov?.availability
        quotaDeltaMinutes = ((ov?.quotaDeltaSeconds ?: 0) / 60).toInt()
    }
    LaunchedEffect(entryId) { reload() }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Today's override", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)

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
                    enabled = availability != null || quotaDeltaMinutes != 0,
                    onClick = {
                        // Re-read the persisted override after a save so
                        // the Clear button reflects the freshly-created
                        // record — otherwise a first-time save leaves
                        // `loaded == null` and Clear stays disabled.
                        vm.upsertOverride(
                            id = entryId,
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
                    enabled = loaded != null
                        || availability != null
                        || quotaDeltaMinutes != 0,
                    onClick = {
                        if (loaded != null) {
                            vm.deleteOverride(entryId, today, onDone = {
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

/** Last-7-days usage as a tiny pure-Compose bar chart. */
@Composable
private fun UsageSection(vm: ShepherdViewModel, entryId: String) {
    var stats by remember(entryId) { mutableStateOf<List<UsageStat>>(emptyList()) }
    LaunchedEffect(entryId) {
        val to = LocalDate.now()
        val from = to.minusDays(6)
        stats = vm.loadUsage(entryId, from.toString(), to.toString())
    }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Last 7 days", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            if (stats.isEmpty()) {
                Text("No usage recorded.", style = MaterialTheme.typography.bodySmall)
            } else {
                BarChart(stats)
                val total = stats.sumOf { it.durationSeconds }
                Text("Total: ${Formatting.coarse(total)}", style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

@Composable
private fun BarChart(stats: List<UsageStat>) {
    val maxSeconds = (stats.maxOfOrNull { it.durationSeconds } ?: 1L).coerceAtLeast(1L)
    Row(
        Modifier.fillMaxWidth().height(96.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.Bottom,
    ) {
        stats.forEach { stat ->
            val fraction = (stat.durationSeconds.toFloat() / maxSeconds).coerceIn(0.02f, 1f)
            Column(
                Modifier.weight(1f).fillMaxHeight(),
                verticalArrangement = Arrangement.Bottom,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .fillMaxHeight(fraction)
                        .clip(RoundedCornerShape(4.dp))
                        .background(MaterialTheme.colorScheme.primary),
                )
                Text(
                    stat.date.takeLast(2),
                    style = MaterialTheme.typography.labelSmall,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
        }
    }
}
