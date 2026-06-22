package com.shepherd.companion.ui.home

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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.shepherd.companion.domain.EntryView
import com.shepherd.companion.domain.SessionInfo
import com.shepherd.companion.ui.ShepherdViewModel
import com.shepherd.companion.ui.components.LinkBanner
import com.shepherd.companion.ui.components.StatusBadge
import com.shepherd.companion.util.ReasonText

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    vm: ShepherdViewModel,
    onAddDevice: () -> Unit,
    onOpenEntry: (String) -> Unit,
    onOpenControls: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val state by vm.state.collectAsState()
    val records by vm.repository.records.collectAsState()
    val activeAddress by vm.repository.activeAddress.collectAsState()

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
                            selected = record.identityAddress == activeAddress,
                            onClick = { vm.selectDevice(record.identityAddress) },
                            label = { Text(record.displayName) },
                        )
                    }
                }
            }

            LinkBanner(link = state.link, onRetry = vm::retryConnection, onRepair = onAddDevice)

            val session = state.currentSession
            if (session != null) {
                CurrentSessionCard(session, onClick = { onOpenEntry(session.entryId) })
            }

            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(state.entries, key = { it.entryId }) { entry ->
                    EntryRow(
                        entry = entry,
                        session = session,
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

@Composable
private fun EntryRow(entry: EntryView, session: SessionInfo?, onClick: () -> Unit) {
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
            val reason = entry.reasons.firstOrNull()
            if (!entry.enabled && reason != null) {
                Text(
                    ReasonText.describe(reason),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        }
    }
}
