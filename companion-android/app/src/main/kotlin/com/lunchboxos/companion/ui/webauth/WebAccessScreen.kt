package com.lunchboxos.companion.ui.webauth

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
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.material3.TopAppBar
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.lunchboxos.companion.domain.LoginRequestInfo
import com.lunchboxos.companion.ui.LinkStatus
import com.lunchboxos.companion.ui.DeviceViewModel
import kotlinx.coroutines.delay

/**
 * How often to ask the device whether a browser is waiting.
 *
 * Fast, unlike the health screen's fifteen seconds: a login request lives two
 * minutes, and the parent is on this screen *because* they just clicked
 * something on a laptop in the next room. A slow poll here reads as the
 * feature being broken.
 */
private const val POLL_INTERVAL_MS = 3_000L

/**
 * Web management access, from the phone (issue #156).
 *
 * Two jobs, and they are the two halves the issue asked for:
 *
 * 1. **Approve a sign-in.** A browser asks the device to let it in and shows
 *    six digits; the same digits appear here. The parent compares and taps.
 *    The comparison is the whole security property — somebody else's request,
 *    racing this one, carries a different number, so a parent who checks
 *    cannot be tricked into approving it.
 * 2. **Set the password.** No old password is asked for, because reaching this
 *    screen required a bonded BLE link to a device this phone is the admin of.
 *    This is what stops a forgotten password being an SSH problem.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WebAccessScreen(vm: DeviceViewModel, onBack: () -> Unit) {
    val state by vm.state.collectAsState()
    val web by vm.webAuth.collectAsState()

    LaunchedEffect(state.link) {
        if (state.link != LinkStatus.Connected) return@LaunchedEffect
        while (true) {
            vm.refreshWebAuth()
            delay(POLL_INTERVAL_MS)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Web access") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = vm::refreshWebAuth, enabled = !web.loading) {
                        if (web.loading) {
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
            web.error?.let {
                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            }
            web.lastAction?.let {
                Text(it, style = MaterialTheme.typography.bodyMedium)
                LaunchedEffect(it) {
                    delay(3_000)
                    vm.clearWebAuthAction()
                }
            }

            if (web.requests.isEmpty()) {
                Text(
                    "No browser is waiting to sign in. Open this device's web page and " +
                        "choose \"Approve on my phone\" — the request appears here.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                for (request in web.requests) {
                    LoginRequestCard(
                        request = request,
                        onApprove = { vm.decideLoginRequest(request.id, approve = true) },
                        onDeny = { vm.decideLoginRequest(request.id, approve = false) },
                    )
                }
            }

            PasswordCard(
                configured = web.status?.configured ?: true,
                onSet = vm::setWebPassword,
            )
        }
    }
}

/**
 * One waiting browser.
 *
 * The number is the largest thing on the card, because comparing it is the
 * only thing the parent is being asked to do. Who is asking, and from where,
 * sit under it: a request from an address nobody recognises is the signal that
 * this is not the browser they are sitting at.
 */
@Composable
private fun LoginRequestCard(
    request: LoginRequestInfo,
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
                "A browser wants to sign in",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                request.code,
                fontFamily = FontFamily.Monospace,
                fontSize = 40.sp,
                fontWeight = FontWeight.Bold,
            )
            Text(
                "Only approve if the browser shows the same number.",
                style = MaterialTheme.typography.bodySmall,
                textAlign = TextAlign.Center,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                "${request.label} · ${request.peer}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
            ) {
                OutlinedButton(onClick = onDeny) { Text("Not me") }
                Button(onClick = onApprove) { Text("Approve") }
            }
        }
    }
}

/** Set or replace the web UI's password, without leaving the sofa. */
@Composable
private fun PasswordCard(configured: Boolean, onSet: (String) -> Unit) {
    var editing by remember { mutableStateOf(false) }
    var password by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf("") }
    val tooShort = password.isNotEmpty() && password.length < 8
    val mismatch = confirm.isNotEmpty() && password != confirm

    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.fillMaxWidth().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text("Web password", style = MaterialTheme.typography.titleMedium)
            Text(
                if (configured) {
                    "The web page asks for a password. You can replace it from here if it " +
                        "has been forgotten — no need for the old one."
                } else {
                    "The web page has no password yet. Set one here, or use the setup code " +
                        "shown on the device's screen."
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (!editing) {
                OutlinedButton(onClick = { editing = true }) {
                    Text(if (configured) "Change password" else "Set password")
                }
            } else {
                OutlinedTextField(
                    value = password,
                    onValueChange = { password = it },
                    label = { Text("New password") },
                    isError = tooShort,
                    supportingText = { Text(if (tooShort) "At least 8 characters" else " ") },
                    visualTransformation = PasswordVisualTransformation(),
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = confirm,
                    onValueChange = { confirm = it },
                    label = { Text("Confirm") },
                    isError = mismatch,
                    supportingText = { Text(if (mismatch) "The passwords do not match" else " ") },
                    visualTransformation = PasswordVisualTransformation(),
                    modifier = Modifier.fillMaxWidth(),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = {
                        editing = false
                        password = ""
                        confirm = ""
                    }) { Text("Cancel") }
                    Button(
                        onClick = {
                            onSet(password)
                            editing = false
                            password = ""
                            confirm = ""
                        },
                        enabled = password.length >= 8 && password == confirm,
                    ) { Text("Save") }
                }
            }
        }
    }
}
