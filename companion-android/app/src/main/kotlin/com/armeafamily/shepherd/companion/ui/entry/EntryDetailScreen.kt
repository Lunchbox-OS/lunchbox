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
import androidx.compose.material.icons.filled.Category
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
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
import com.armeafamily.shepherd.companion.ui.components.ReasonLines
import com.armeafamily.shepherd.companion.ui.override.OverrideSection
import com.armeafamily.shepherd.companion.util.Formatting
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.time.LocalDate

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EntryDetailScreen(
    vm: ShepherdViewModel,
    entryId: String,
    onBack: () -> Unit,
    onOpenGroup: (String) -> Unit,
) {
    val state by vm.state.collectAsState()
    val entry = state.entries.firstOrNull { it.entryId == entryId }
    val session = state.currentSession?.takeIf { it.entryId == entryId }
    val group = entry?.let { state.groupOf(it) }

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

            // The category is where a shared limit can actually be inspected or
            // overridden, and it is often the thing blocking this activity, so
            // offer the way there rather than only naming it (issue #5).
            if (group != null) {
                AssistChip(
                    onClick = { onOpenGroup(group.groupId) },
                    label = { Text(group.label) },
                    leadingIcon = {
                        Icon(
                            Icons.Filled.Category,
                            contentDescription = null,
                            modifier = Modifier.size(AssistChipDefaults.IconSize),
                        )
                    },
                )
            }

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
            ReasonLines(entry.reasons)
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
