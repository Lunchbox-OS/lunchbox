package com.lunchboxos.companion.ui.components

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
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.lunchboxos.companion.domain.EntryView
import com.lunchboxos.companion.domain.GroupView
import com.lunchboxos.companion.domain.ReasonCode
import com.lunchboxos.companion.domain.TokenStatus
import com.lunchboxos.companion.domain.SessionInfo
import com.lunchboxos.companion.ui.LinkStatus
import com.lunchboxos.companion.util.Formatting
import com.lunchboxos.companion.util.ReasonText

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
        // Re-pair is offered here too, not just on the repair states.
        // Nothing removes a stale bond automatically any more, so if the
        // scan probe can't run (BT off, permission revoked) this is the
        // only banner the user ever sees — and without the affordance
        // they'd have no way out of a one-sided bond short of forgetting
        // the device entirely.
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
                    Text(
                        "Disconnected",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f),
                    )
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        TextButton(onClick = onRetry) { Text("Retry") }
                        TextButton(onClick = onRepair) { Text("Re-pair") }
                    }
                }
            }
        }
        // Re-pairing is a trip to the TV, so it's offered rather than
        // imposed: the bond stays intact until the user taps Re-pair.
        // Retry comes first because the faults that land here (radio
        // congestion, a daemon restart, an event backlog) usually clear
        // on their own.
        LinkStatus.RepairSuggested -> {
            Card(
                modifier = Modifier.fillMaxWidth(),
                colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer),
            ) {
                Row(
                    Modifier.fillMaxWidth().padding(start = 16.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "Can't reach this device securely",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f),
                    )
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        TextButton(onClick = onRetry) { Text("Retry") }
                        TextButton(onClick = onRepair) { Text("Re-pair") }
                    }
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

/** Step for the earned-time stepper, matching the quota stepper's ±5 min. */
private const val TOKEN_STEP_SECONDS = 5L * 60

/**
 * Banked time on a token gate, with a stepper to grant or revoke it (issue #8).
 *
 * Used for an activity's own gate and for a category's alike — the caller
 * passes the subject through [onAdjust]. The progress toward `minimum` is the
 * point of the card: granting blind is how a caregiver hands out time that is
 * still short of the threshold and wonders why nothing unlocked.
 */
@Composable
fun TokenCard(tokens: TokenStatus, onAdjust: (Long) -> Unit) {
    val balance = tokens.balance.secs
    val minimum = tokens.minimum.secs
    val atCeiling = tokens.maxBalance?.let { balance >= it.secs } == true

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                "Earned time",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                buildString {
                    append(Formatting.coarse(balance))
                    if (minimum > 0) append(" of ${Formatting.coarse(minimum)} needed")
                    append(if (tokens.unlocked) " · unlocked" else " · locked")
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            if (minimum > 0 && !tokens.unlocked) {
                LinearProgressIndicator(
                    progress = { (balance.toFloat() / minimum.toFloat()).coerceIn(0f, 1f) },
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            if (atCeiling) {
                Text(
                    "At the maximum — more time can't be banked.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                )
            }
            if (!tokens.carryOver) {
                Text(
                    "Unspent time expires at midnight.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = { onAdjust(-TOKEN_STEP_SECONDS) },
                    enabled = balance > 0,
                    modifier = Modifier.weight(1f),
                ) { Text("−5 min") }
                OutlinedButton(
                    onClick = { onAdjust(TOKEN_STEP_SECONDS) },
                    enabled = !atCeiling,
                    modifier = Modifier.weight(1f),
                ) { Text("+5 min") }
            }
        }
    }
}
