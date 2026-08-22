package com.armeafamily.shepherd.companion.ui.windows

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Visibility
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.domain.WindowAction
import com.armeafamily.shepherd.companion.domain.WindowInfo
import com.armeafamily.shepherd.companion.ui.LinkStatus
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel
import com.armeafamily.shepherd.companion.ui.windows.WindowPresentation.Placement
import kotlinx.coroutines.delay

/** How often the list re-reads itself while this screen is on top. */
private const val POLL_INTERVAL_MS = 5_000L

/**
 * Every window the device's compositor is tracking, with the three
 * actions `act_on_window` exposes: close, hide (stash on the
 * scratchpad), and show (pull it back).
 *
 * This is the maintenance view for the case the rest of the app can't
 * reach — an activity that has ended but left a window behind, a modal
 * that has stolen focus, a Steam client that should be hidden and
 * isn't. Closing the wrong window costs the child unsaved work rather
 * than anything worse, so Close confirms and the rest act immediately.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WindowsScreen(vm: ShepherdViewModel, onBack: () -> Unit) {
    val state by vm.state.collectAsState()
    val windows by vm.windows.collectAsState()
    var confirmClose by remember { mutableStateOf<WindowInfo?>(null) }

    // Poll only while this screen is composed and the link is up. Nothing
    // pushes window changes, and a list that silently ages is worse than
    // no list: the ids under the buttons are the ones being acted on.
    LaunchedEffect(state.link) {
        if (state.link != LinkStatus.Connected) return@LaunchedEffect
        while (true) {
            vm.refreshWindows()
            delay(POLL_INTERVAL_MS)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Windows") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = vm::refreshWindows, enabled = !windows.loading) {
                        if (windows.loading) {
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
                "Windows on ${state.record?.displayName ?: "the device"}, refreshed every " +
                    "${POLL_INTERVAL_MS / 1000}s.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
            )

            windows.error?.let { error ->
                Text(
                    error,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            if (!windows.loaded) {
                if (windows.error == null) {
                    Text(
                        "Reading the window list…",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(vertical = 16.dp),
                    )
                }
                return@Column
            }

            if (windows.windows.isEmpty()) {
                Text(
                    "No windows open.",
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(vertical = 16.dp),
                )
                return@Column
            }

            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                section("On screen", windows.onScreen, windows.busyId, vm, onConfirmClose = { confirmClose = it })
                // The scratchpad is where shepherd stashes windows it wants
                // running but out of sight (the Steam client, chiefly), so
                // its contents are worth their own heading rather than being
                // mixed in and distinguishable only by a chip.
                section("Scratchpad", windows.scratchpad, windows.busyId, vm, onConfirmClose = { confirmClose = it })
            }
        }
    }

    confirmClose?.let { target ->
        AlertDialog(
            onDismissRequest = { confirmClose = null },
            title = { Text("Close this window?") },
            text = {
                Text(
                    "${WindowPresentation.title(target)} will be asked to close. " +
                        "Anything unsaved in it is lost.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmClose = null
                    vm.actOnWindow(target.id, WindowAction.CLOSE)
                }) { Text("Close window") }
            },
            dismissButton = { TextButton(onClick = { confirmClose = null }) { Text("Cancel") } },
        )
    }
}

private fun LazyListScope.section(
    heading: String,
    windows: List<WindowInfo>,
    busyId: Long?,
    vm: ShepherdViewModel,
    onConfirmClose: (WindowInfo) -> Unit,
) {
    if (windows.isEmpty()) return
    item(key = "heading:$heading") {
        Text(
            "$heading (${windows.size})",
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.padding(top = 8.dp),
        )
    }
    items(windows, key = { it.id }) { w ->
        WindowCard(
            w = w,
            busy = busyId != null,
            onAct = { action -> vm.actOnWindow(w.id, action) },
            onConfirmClose = { onConfirmClose(w) },
        )
    }
}

@Composable
private fun WindowCard(
    w: WindowInfo,
    busy: Boolean,
    onAct: (WindowAction) -> Unit,
    onConfirmClose: () -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(
                        WindowPresentation.title(w),
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        WindowPresentation.subtitle(w),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline,
                    )
                    val workspace = w.workspace?.takeIf { !w.inScratchpad }
                    if (workspace != null) {
                        Text(
                            "Workspace $workspace",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.outline,
                        )
                    }
                }
                PlacementChip(w)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (w.inScratchpad) {
                    OutlinedButton(onClick = { onAct(WindowAction.SHOW) }, enabled = !busy) {
                        Icon(
                            Icons.Filled.Visibility,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp),
                        )
                        Text("Show", Modifier.padding(start = 8.dp))
                    }
                } else {
                    OutlinedButton(onClick = { onAct(WindowAction.HIDE) }, enabled = !busy) {
                        Icon(
                            Icons.Filled.VisibilityOff,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp),
                        )
                        Text("Hide", Modifier.padding(start = 8.dp))
                    }
                }
                OutlinedButton(onClick = onConfirmClose, enabled = !busy) {
                    Icon(
                        Icons.Filled.Close,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Text("Close", Modifier.padding(start = 8.dp))
                }
            }
        }
    }
}

@Composable
private fun PlacementChip(w: WindowInfo) {
    val placement = WindowPresentation.placement(w)
    // Focus outranks placement: a focused window is on screen by
    // definition, and "which one is the child looking at" is the first
    // thing this screen is opened to answer.
    val (label, color) = when {
        w.focused -> "Focused" to MaterialTheme.colorScheme.tertiary
        placement == Placement.ON_SCREEN -> placement.label to MaterialTheme.colorScheme.primary
        else -> placement.label to MaterialTheme.colorScheme.outline
    }
    AssistChip(
        onClick = {},
        enabled = false,
        label = { Text(label) },
        colors = AssistChipDefaults.assistChipColors(disabledLabelColor = color),
    )
}
