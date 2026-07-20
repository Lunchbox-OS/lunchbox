package com.armeafamily.shepherd.companion.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.EntryView
import com.armeafamily.shepherd.companion.domain.GroupView
import com.armeafamily.shepherd.companion.domain.ReasonCode
import com.armeafamily.shepherd.companion.domain.SessionInfo
import com.armeafamily.shepherd.companion.ui.LinkStatus
import com.armeafamily.shepherd.companion.util.Formatting
import com.armeafamily.shepherd.companion.util.ReasonText

/** A banner reflecting the BLE link state, with recovery actions. */
@Composable
fun LinkBanner(link: LinkStatus, onRetry: () -> Unit, onRepair: () -> Unit) {
    when (link) {
        LinkStatus.Connecting, LinkStatus.Reconnecting -> {
            Card(modifier = Modifier.fillMaxWidth()) {
                Row(
                    Modifier.fillMaxWidth().padding(12.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.padding(2.dp))
                    Text(
                        if (link == LinkStatus.Connecting) "Connecting…" else "Reconnecting…",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
        }
        LinkStatus.Disconnected -> {
            Card(
                modifier = Modifier.fillMaxWidth(),
                colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer),
            ) {
                Row(
                    Modifier.fillMaxWidth().padding(start = 16.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("Disconnected", style = MaterialTheme.typography.bodyMedium)
                    TextButton(onClick = onRetry) { Text("Retry") }
                }
            }
        }
        LinkStatus.NeedsRepair -> {
            Card(
                modifier = Modifier.fillMaxWidth(),
                colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer),
            ) {
                Row(
                    Modifier.fillMaxWidth().padding(start = 16.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("Bond lost — re-pair needed", style = MaterialTheme.typography.bodyMedium)
                    TextButton(onClick = onRepair) { Text("Re-pair") }
                }
            }
        }
        LinkStatus.Connected, LinkStatus.Idle -> Unit
    }
}

/** Small status chips for entries and sessions. */
object StatusBadge {
    @Composable
    fun forEntry(entry: EntryView, inSession: Boolean) {
        val (label, color) = when {
            inSession -> "In session" to MaterialTheme.colorScheme.tertiary
            entry.enabled -> "Available" to MaterialTheme.colorScheme.primary
            else -> "Blocked" to MaterialTheme.colorScheme.error
        }
        chip(label, color)
    }

    /** Category status (issue #5): whether the shared limits currently allow its members. */
    @Composable
    fun forGroup(group: GroupView) {
        val (label, color) = if (group.enabled) {
            "Available" to MaterialTheme.colorScheme.primary
        } else {
            "Blocked" to MaterialTheme.colorScheme.error
        }
        chip(label, color)
    }

    @Composable
    fun forSession(session: SessionInfo) {
        val remaining = session.timeRemaining?.let { "${Formatting.hms(it.secs)} left" }
            ?: "No time limit"
        chip(remaining, MaterialTheme.colorScheme.tertiary)
    }

    @Composable
    private fun chip(label: String, color: Color) {
        AssistChip(
            onClick = {},
            enabled = false,
            label = { Text(label) },
            colors = AssistChipDefaults.assistChipColors(
                disabledLabelColor = color,
            ),
        )
    }
}

/**
 * Every reason an activity or category is unavailable, one per line.
 *
 * All of them, not just the first: something blocked by both a cooldown and a
 * spent quota would otherwise reveal the second reason only once the first is
 * cleared, which reads like the limit moved.
 */
@Composable
fun ReasonLines(reasons: List<ReasonCode>) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        reasons.forEach { reason ->
            Text(
                ReasonText.describe(reason),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}
