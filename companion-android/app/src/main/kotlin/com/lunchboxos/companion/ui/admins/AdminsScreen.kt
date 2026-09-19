package com.lunchboxos.companion.ui.admins

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.lunchboxos.companion.domain.AdminSummary
import com.lunchboxos.companion.domain.EnrolmentRequestInfo
import com.lunchboxos.companion.ui.LinkStatus
import com.lunchboxos.companion.ui.DeviceViewModel
import com.lunchboxos.companion.util.Formatting
import kotlinx.coroutines.delay

/**
 * How often to ask the device who is waiting.
 *
 * The same three seconds as the web-access screen, for the same reason: the
 * other parent is standing there holding a phone that says "waiting for
 * approval", and a slow poll here reads as the feature being broken.
 */
private const val POLL_INTERVAL_MS = 3_000L

/**
 * The device's administrators, and the phones asking to become one
 * (issue #149).
 *
 * A device is claimed by the first phone that reaches it, because there is
 * nobody to ask yet. Every phone after that lands here as a request: it shows
 * six digits, the same digits appear on this screen, and somebody who is
 * already an administrator compares them and taps. The comparison is the
 * security property — a phone racing the one you meant carries a different
 * number, so a parent who actually checks cannot be tricked into enrolling it.
 *
 * That matters more here than for a browser sign-in. The anchor for a BLE
 * pairing is standing in front of the TV, and the person who does that most is
 * the child the device exists to supervise.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminsScreen(vm: DeviceViewModel, onBack: () -> Unit) {
    val state by vm.state.collectAsState()
    val admins by vm.admins.collectAsState()
    var confirmRevoke by remember { mutableStateOf<AdminSummary?>(null) }

    LaunchedEffect(state.link) {
        if (state.link != LinkStatus.Connected) return@LaunchedEffect
        while (true) {
            vm.refreshAdmins()
            delay(POLL_INTERVAL_MS)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Administrators") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = vm::refreshAdmins, enabled = !admins.loading) {
                        if (admins.loading) {
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
            Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            admins.error?.let {
                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            }
            admins.lastAction?.let {
                Text(it, style = MaterialTheme.typography.bodyMedium)
                LaunchedEffect(it) {
                    delay(3_000)
                    vm.consumeAdminsAction()
                }
            }

            for (request in admins.requests) {
                EnrolmentRequestCard(
                    request = request,
                    onApprove = { vm.decideEnrolment(request.id, approve = true) },
                    onDeny = { vm.decideEnrolment(request.id, approve = false) },
                )
            }

            Text(
                "Paired phones",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            if (admins.admins.isEmpty() && admins.loaded) {
                Text(
                    "Nobody. This shouldn't happen while you're connected — try refreshing.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            for (admin in admins.admins) {
                AdminRow(
                    admin = admin,
                    // The device does not say which row is us — over HTTP
                    // there is no phone to be, so the flag was dropped from
                    // the wire rather than made meaningless for one caller.
                    // This phone already knows its own identity address.
                    isSelf = admin.identityAddress
                        .equals(state.record?.identityAddress, ignoreCase = true),
                    // The device refuses to remove the last administrator, so
                    // don't offer it: a button whose only outcome is an error
                    // teaches people to ignore errors.
                    canRevoke = admins.admins.size > 1,
                    onRevoke = { confirmRevoke = admin },
                )
            }

            if (admins.requests.isEmpty() && admins.loaded) {
                Text(
                    "To add another phone, install the app on it and pair it with this " +
                        "device. It will appear here for you to approve.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }

    confirmRevoke?.let { admin ->
        val self = admin.identityAddress
            .equals(state.record?.identityAddress, ignoreCase = true)
        AlertDialog(
            onDismissRequest = { confirmRevoke = null },
            title = { Text(if (self) "Remove this phone?" else "Remove ${admin.deviceName}?") },
            text = {
                Text(
                    if (self) {
                        "This phone will lose access to the device and its Bluetooth bond " +
                            "will be removed. You'd have to pair again and be approved by " +
                            "another administrator."
                    } else {
                        "${admin.deviceName} will lose access and its Bluetooth bond will be " +
                            "removed. It can ask to be added again."
                    },
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmRevoke = null
                    vm.revokeAdmin(admin.id)
                }) { Text("Remove") }
            },
            dismissButton = { TextButton(onClick = { confirmRevoke = null }) { Text("Cancel") } },
        )
    }
}

/**
 * One phone waiting to be enrolled.
 *
 * The number is the largest thing on the card because comparing it is the only
 * thing being asked. Who is asking, and from what address, sit under it: a
 * request from a phone nobody recognises is exactly the case this screen
 * exists to catch.
 */
@Composable
private fun EnrolmentRequestCard(
    request: EnrolmentRequestInfo,
    onApprove: () -> Unit,
    onDeny: () -> Unit,
) {
    Card(
        Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.secondaryContainer,
        ),
    ) {
        Column(
            Modifier.fillMaxWidth().padding(16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "A phone wants to administer this device",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
                textAlign = TextAlign.Center,
            )
            Text(
                request.code,
                fontFamily = FontFamily.Monospace,
                fontSize = 40.sp,
                fontWeight = FontWeight.Bold,
            )
            Text(
                "Only approve if that phone is showing the same number.",
                style = MaterialTheme.typography.bodySmall,
                textAlign = TextAlign.Center,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                "${request.deviceName} · ${request.peer}",
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
            ) {
                OutlinedButton(onClick = onDeny) { Text("Not mine") }
                Button(onClick = onApprove) { Text("Approve") }
            }
        }
    }
}

@Composable
private fun AdminRow(
    admin: AdminSummary,
    isSelf: Boolean,
    canRevoke: Boolean,
    onRevoke: () -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        ListItem(
            headlineContent = {
                Text(admin.deviceName + if (isSelf) " (this phone)" else "")
            },
            supportingContent = {
                Column {
                    Text(admin.identityAddress, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
                    Text(
                        "Added ${Formatting.dateTime(admin.bondedAt)}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            },
            trailingContent = {
                if (canRevoke) {
                    TextButton(onClick = onRevoke) { Text("Remove") }
                }
            },
        )
    }
}
