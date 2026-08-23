package com.armeafamily.shepherd.companion.ui.health

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.Diagnostic
import com.armeafamily.shepherd.companion.domain.DiagnosticSeverity
import com.armeafamily.shepherd.companion.domain.DiagnosticSubject
import com.armeafamily.shepherd.companion.ui.LinkStatus
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel
import kotlinx.coroutines.delay

/** How often the list re-reads itself while this screen is on top. */
private const val POLL_INTERVAL_MS = 15_000L

/**
 * The colour a severity is rendered in.
 *
 * Critical is the error colour rather than a warning tint: it means the
 * configuration promises a protection the device is not providing, which is a
 * different thing from a feature being degraded.
 */
@Composable
private fun severityColor(severity: DiagnosticSeverity): Color = when (severity) {
    DiagnosticSeverity.CRITICAL -> MaterialTheme.colorScheme.error
    DiagnosticSeverity.WARNING -> MaterialTheme.colorScheme.tertiary
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

private fun severityLabel(severity: DiagnosticSeverity): String = when (severity) {
    DiagnosticSeverity.CRITICAL -> "Critical"
    DiagnosticSeverity.WARNING -> "Warning"
    else -> "Info"
}

/** The activity a diagnostic names, or null when it is about the device. */
private fun entryIdOf(d: Diagnostic): String? =
    (d.subject as? DiagnosticSubject.Entry)?.entryId

/**
 * What a diagnostic is about, for the chip on its card.
 *
 * Falls back to the raw entry id when the label is not to hand — a caregiver
 * can still act on "movies" even if it is not the name they gave it, and
 * showing nothing would leave them guessing which activity is broken.
 */
private fun subjectLabel(d: Diagnostic, labelFor: (String) -> String?): String? =
    when (val s = d.subject) {
        is DiagnosticSubject.Entry -> labelFor(s.entryId) ?: s.entryId
        else -> null
    }

/**
 * Everything currently wrong with the device, for whoever set it up.
 *
 * Distinct from the entry list, which answers "can my child use this now".
 * These are problems with the device itself — a missing dependency, a
 * protection that is configured but not in effect — and they persist until
 * somebody fixes them.
 *
 * The daemon sorts the set (most severe first) and this screen keeps that
 * order, so the phone and the web UI agree on what matters most.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HealthScreen(vm: ShepherdViewModel, onBack: () -> Unit) {
    val state by vm.state.collectAsState()
    val health by vm.diagnostics.collectAsState()

    // Polled while composed, like the windows screen. Slower, because these
    // are conditions somebody has to go and fix rather than live state.
    LaunchedEffect(state.link) {
        if (state.link != LinkStatus.Connected) return@LaunchedEffect
        while (true) {
            vm.refreshDiagnostics()
            delay(POLL_INTERVAL_MS)
        }
    }

    val labelFor: (String) -> String? = { id ->
        state.entries.firstOrNull { it.entryId == id }?.label
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Device health") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = vm::refreshDiagnostics, enabled = !health.loading) {
                        if (health.loading) {
                            CircularProgressIndicator(
                                strokeWidth = 2.dp,
                                modifier = Modifier.size(20.dp),
                            )
                        } else {
                            Icon(Icons.Filled.Refresh, contentDescription = "Refresh")
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "Problems that need someone to fix them. These are separate from a child " +
                    "running out of time — they mean the device is not doing something its " +
                    "settings say it should.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            health.error?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            if (health.set.truncated) {
                Text(
                    "There are more problems than can be listed. Fix these and refresh to " +
                        "see the rest.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            // "Nothing to fix" only once a set has actually arrived: before
            // that the device is not healthy, it is unasked.
            if (health.loaded && health.items.isEmpty()) {
                Row(
                    Modifier.fillMaxWidth().padding(vertical = 24.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        Icons.Filled.CheckCircle,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.primary,
                    )
                    Column {
                        Text("Nothing to fix", fontWeight = FontWeight.SemiBold)
                        Text(
                            "Every dependency is installed and every configured protection " +
                                "is in effect.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }

            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(
                    health.items,
                    key = { "${it.code}:${entryIdOf(it) ?: "service"}" },
                ) { d ->
                    DiagnosticCard(d, subjectLabel(d, labelFor))
                }
            }
        }
    }
}

@Composable
private fun DiagnosticCard(d: Diagnostic, subject: String?) {
    val color = severityColor(d.severity)
    Card(colors = CardDefaults.cardColors()) {
        Column(
            Modifier.fillMaxWidth().padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(d.message, fontWeight = FontWeight.SemiBold, color = color)
            d.remedy?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                AssistChip(
                    onClick = {},
                    enabled = false,
                    label = { Text(severityLabel(d.severity)) },
                    colors = AssistChipDefaults.assistChipColors(disabledLabelColor = color),
                )
                subject?.let {
                    AssistChip(onClick = {}, enabled = false, label = { Text(it) })
                }
            }
        }
    }
}
