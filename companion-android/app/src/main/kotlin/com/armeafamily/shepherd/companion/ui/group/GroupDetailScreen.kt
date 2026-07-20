package com.armeafamily.shepherd.companion.ui.group

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.GroupView
import com.armeafamily.shepherd.companion.domain.subject
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel
import com.armeafamily.shepherd.companion.ui.components.ReasonLines
import com.armeafamily.shepherd.companion.ui.override.OverrideSection
import com.armeafamily.shepherd.companion.util.Formatting

/**
 * A category's shared state and its override editor (issue #5).
 *
 * The limits shown here are spent by *any* member, which is what makes a
 * category worth its own screen: no single activity's page can explain why
 * the budget is gone.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun GroupDetailScreen(
    vm: ShepherdViewModel,
    groupId: String,
    onBack: () -> Unit,
) {
    val state by vm.state.collectAsState()
    val group = state.groups.firstOrNull { it.groupId == groupId }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(group?.label ?: groupId) },
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
            if (group == null) {
                Text("This category is no longer configured.")
                return@Column
            }

            Text(
                "These limits are shared by every activity in the category.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
            )

            SharedLimitsCard(group)
            MembersCard(group, state.entries.associate { it.entryId to it.label })
            OverrideSection(vm, group.subject)
        }
    }
}

@Composable
private fun SharedLimitsCard(group: GroupView) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            val quota = group.dailyQuota
            Text(
                if (quota != null) {
                    "${Formatting.coarse(group.usedToday.secs)} of " +
                        "${Formatting.coarse(quota.secs)} used today"
                } else {
                    "${Formatting.coarse(group.usedToday.secs)} used today (no daily limit)"
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            if (quota != null && quota.secs > 0) {
                LinearProgressIndicator(
                    progress = {
                        (group.usedToday.secs.toFloat() / quota.secs.toFloat()).coerceIn(0f, 1f)
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
            }

            // Only meaningful while the category actually allows a session: a
            // blocked one reports a cap of zero, and "Up to 0s per session"
            // reads as a limit rather than as "not right now". The reasons
            // below say what is really going on.
            if (group.enabled) {
                group.maxRunIfStartedNow?.let {
                    Text(
                        "Up to ${Formatting.coarse(it.secs)} per session",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline,
                    )
                }
            } else {
                ReasonLines(group.reasons)
            }
        }
    }
}

@Composable
private fun MembersCard(group: GroupView, labels: Map<String, String>) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                "Activities in this category",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            if (group.memberIds.isEmpty()) {
                Text("None", style = MaterialTheme.typography.bodySmall)
            }
            group.memberIds.forEach { id ->
                Text(labels[id] ?: id, style = MaterialTheme.typography.bodyMedium)
            }
        }
    }
}
