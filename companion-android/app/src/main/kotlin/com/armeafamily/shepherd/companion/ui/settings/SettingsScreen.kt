package com.armeafamily.shepherd.companion.ui.settings

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.armeafamily.shepherd.companion.R
import com.armeafamily.shepherd.companion.ui.ShepherdViewModel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    vm: ShepherdViewModel,
    onBack: () -> Unit,
    onAllForgotten: () -> Unit,
) {
    val context = LocalContext.current
    val state by vm.state.collectAsState()
    val records by vm.repository.records.collectAsState()
    val record = state.record

    var confirmReset by remember { mutableStateOf(false) }
    var confirmForgetAll by remember { mutableStateOf(false) }
    var nickname by remember(record?.identityAddress) { mutableStateOf(record?.nickname ?: "") }

    val version = remember {
        runCatching {
            context.packageManager.getPackageInfo(context.packageName, 0).versionName
        }.getOrNull() ?: "?"
    }
    val issueUrl = stringResource(R.string.issue_tracker_url)

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
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
            if (record != null) {
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(record.deviceName, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                        Text("Identity: ${record.identityAddress}", style = MaterialTheme.typography.bodySmall)
                        OutlinedTextField(
                            value = nickname,
                            onValueChange = { nickname = it },
                            label = { Text("Nickname (this phone only)") },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        OutlinedButton(
                            onClick = { vm.updateNickname(record.identityAddress, nickname) },
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Save nickname") }
                        Button(
                            onClick = { confirmReset = true },
                            modifier = Modifier.fillMaxWidth(),
                            colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                        ) { Text("Factory reset (unpair)") }
                    }
                }
            }

            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("All devices", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Text("${records.size} paired", style = MaterialTheme.typography.bodySmall)
                    OutlinedButton(
                        onClick = { confirmForgetAll = true },
                        enabled = records.isNotEmpty(),
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text("Forget all devices") }
                }
            }

            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("About", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Text("Shepherd Companion $version", style = MaterialTheme.typography.bodyMedium)
                    TextButton(onClick = {
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(issueUrl)))
                    }) { Text("Report an issue") }
                }
            }
        }
    }

    if (confirmReset && record != null) {
        AlertDialog(
            onDismissRequest = { confirmReset = false },
            title = { Text("Factory reset?") },
            text = { Text("This wipes the bond on ${record.displayName} and removes it from this phone. You'll need to pair again from scratch.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmReset = false
                    vm.factoryReset(onDone = onBack)
                }) { Text("Reset") }
            },
            dismissButton = { TextButton(onClick = { confirmReset = false }) { Text("Cancel") } },
        )
    }

    if (confirmForgetAll) {
        AlertDialog(
            onDismissRequest = { confirmForgetAll = false },
            title = { Text("Forget all devices?") },
            text = { Text("Removes every device from this phone. The devices keep their bonds — use this only to recover a phone that lost its device list.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmForgetAll = false
                    vm.forgetAll(onDone = onAllForgotten)
                }) { Text("Forget all") }
            },
            dismissButton = { TextButton(onClick = { confirmForgetAll = false }) { Text("Cancel") } },
        )
    }
}
