package com.armeafamily.shepherd.companion.ui.home

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.EntryView
import com.armeafamily.shepherd.companion.domain.GroupView
import com.armeafamily.shepherd.companion.domain.SessionInfo
import com.armeafamily.shepherd.companion.ui.LinkStatus
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel
import com.armeafamily.shepherd.companion.ui.components.LinkBanner
import com.armeafamily.shepherd.companion.ui.components.ReasonLines
import com.armeafamily.shepherd.companion.ui.components.StatusBadge
import com.armeafamily.shepherd.companion.util.Formatting

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    vm: ShepherdViewModel,
    onAddDevice: () -> Unit,
    onOpenEntry: (String) -> Unit,
    onOpenGroup: (String) -> Unit,
    onOpenControls: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val state by vm.state.collectAsState()
    val records by vm.repository.records.collectAsState()
    val activeId by vm.repository.activeId.collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(state.record?.displayName ?: "Shepherd") },
                actions = {
                    if (state.record != null) {
                        IconButton(onClick = onOpenControls) {
                            Icon(Icons.Filled.Tune, contentDescription = "Device controls")
                        }
                    }
                    IconButton(onClick = onOpenSettings) {
                        Icon(Icons.Filled.Settings, contentDescription = "Settings")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier.fillMaxSize().padding(padding).padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (records.isEmpty()) {
                EmptyDevices(onAddDevice)
                return@Column
            }

            if (records.size > 1) {
                Row(
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    records.forEach { record ->
                        FilterChip(
                            selected = record.androidIdentifier == activeId,
                            onClick = { vm.selectDevice(record.androidIdentifier) },
                            label = { Text(record.displayName) },
                        )
                    }
                }
            }

            // Accepting the re-pair offer is what drops the bond — the
            // connect loop no longer does it unprompted. Harmless on
            // NeedsRepair, where the OS bond is already gone.
            LinkBanner(
                link = state.link,
                onRetry = vm::retryConnection,
                onRepair = { vm.dropBondAndRepair(); onAddDevice() },
            )

            // Everything below is the last snapshot the device sent. While
            // the link is down that is history, not status, and this screen
            // exists to answer "what is happening right now" — so fade it
            // rather than letting a minutes-old "Blocked / Outside allowed
            // hours" render identically to a live one. The banner above
            // says why; this makes the staleness visible at a glance even
            // once the banner has been scrolled past.
            val stale = state.snapshot != null && state.link != LinkStatus.Connected &&
                state.link != LinkStatus.Idle
            Column(
                modifier = if (stale) Modifier.alpha(0.45f) else Modifier,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val session = state.currentSession
                if (session != null) {
                    CurrentSessionCard(session, onClick = { onOpenEntry(session.entryId) })
                }

                LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    // Categories first: their limits are shared, so a member can
                    // become unavailable because a sibling was played (issue #5).
                    if (state.groups.isNotEmpty()) {
                        item {
                            Text(
                                "Categories",
                                style = MaterialTheme.typography.titleSmall,
                                fontWeight = FontWeight.SemiBold,
                                modifier = Modifier.padding(top = 8.dp),
                            )
                        }
                        items(state.groups, key = { "group:" + it.groupId }) { group ->
                            GroupRow(group = group, onClick = { onOpenGroup(group.groupId) })
                        }
                        item {
                            Text(
                                "Activities",
                                style = MaterialTheme.typography.titleSmall,
                                fontWeight = FontWeight.SemiBold,
                                modifier = Modifier.padding(top = 8.dp),
                            )
                        }
                    }
                    items(state.entries, key = { it.entryId }) { entry ->
                        EntryRow(
                            entry = entry,
                            session = session,
                            groupLabel = state.groupOf(entry)?.label,
                            onClick = { onOpenEntry(entry.entryId) },
                        )
                    }
                    if (state.entries.isEmpty()) {
                        item {
                            Text(
                                "No activities yet.",
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.padding(16.dp),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun EmptyDevices(onAddDevice: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("No devices paired", style = MaterialTheme.typography.headlineSmall)
        Text(
            "Pair a shepherd device over Bluetooth to manage it.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Button(onClick = onAddDevice) { Text("Pair a device") }
    }
}

@Composable
private fun CurrentSessionCard(session: SessionInfo, onClick: () -> Unit) {
    Card(onClick = onClick, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text("Now playing", style = MaterialTheme.typography.labelMedium)
            Text(session.label, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Bold)
            StatusBadge.forSession(session)
        }
    }
}

/** A category row: shared budget usage and whatever is restricting it. */
@Composable
private fun GroupRow(group: GroupView, onClick: () -> Unit) {
    Card(onClick = onClick, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    group.label,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                StatusBadge.forGroup(group)
            }
            Text(
                buildString {
                    append(Formatting.coarse(group.usedToday.secs))
                    val quota = group.dailyQuota
                    if (quota != null) append(" of ${Formatting.coarse(quota.secs)}")
                    append(" used today · ")
                    append("${group.memberIds.size} activit")
                    append(if (group.memberIds.size == 1) "y" else "ies")
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
            )
            if (!group.enabled) {
                ReasonLines(group.reasons)
            }
        }
    }
}

@Composable
private fun EntryRow(
    entry: EntryView,
    session: SessionInfo?,
    groupLabel: String?,
    onClick: () -> Unit,
) {
    val inSession = session?.entryId == entry.entryId
    Card(onClick = onClick, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    entry.label,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                StatusBadge.forEntry(entry, inSession)
            }
            // Name the category so it's obvious the limits are shared.
            if (groupLabel != null) {
                Text(
                    groupLabel,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.outline,
                )
            }
            // The running activity is "unavailable" only against itself — it
            // carries `SessionActive` like everything else — so reporting
            // "Another activity is running" on its own row would be nonsense.
            if (!entry.enabled && !inSession) {
                ReasonLines(entry.reasons)
            }
        }
    }
}
