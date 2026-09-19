package com.lunchboxos.companion.ui.network

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.widget.Toast
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
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.lunchboxos.companion.domain.Connectivity
import com.lunchboxos.companion.domain.InternetStatusView
import com.lunchboxos.companion.domain.NetworkInterfaceKind
import com.lunchboxos.companion.domain.NetworkInterfaceView
import com.lunchboxos.companion.domain.NetworkSource
import com.lunchboxos.companion.domain.WebListenerState
import com.lunchboxos.companion.domain.WebListenerView
import com.lunchboxos.companion.domain.WifiView
import com.lunchboxos.companion.ui.LinkStatus
import com.lunchboxos.companion.ui.DeviceViewModel
import kotlinx.coroutines.delay

/**
 * How often the status re-reads itself while this screen is on top.
 *
 * Faster than the health screen's poll: an address appears the moment a cable
 * goes in or a VPN comes up, and somebody watching this screen is usually
 * waiting for exactly that.
 */
private const val POLL_INTERVAL_MS = 10_000L

private fun connectivityLabel(c: Connectivity): String = when (c) {
    Connectivity.FULL -> "Online"
    // The one that looks connected and is not. Named in its own words rather
    // than as a shade of "limited".
    Connectivity.PORTAL -> "Sign-in required"
    Connectivity.LIMITED -> "Limited"
    Connectivity.NONE -> "Offline"
    else -> "Unknown"
}

@Composable
private fun connectivityColor(c: Connectivity): Color = when (c) {
    Connectivity.FULL -> MaterialTheme.colorScheme.primary
    Connectivity.NONE -> MaterialTheme.colorScheme.error
    Connectivity.PORTAL, Connectivity.LIMITED -> MaterialTheme.colorScheme.tertiary
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

private fun kindLabel(kind: NetworkInterfaceKind): String = when (kind) {
    NetworkInterfaceKind.WIFI -> "Wi-Fi"
    NetworkInterfaceKind.ETHERNET -> "Ethernet"
    NetworkInterfaceKind.VPN -> "VPN"
    NetworkInterfaceKind.BRIDGE -> "Bridge"
    NetworkInterfaceKind.LOOPBACK -> "Loopback"
    else -> "Other"
}

/** Signal and band, for somebody working out why the picture keeps stalling. */
private fun wifiSummary(wifi: WifiView): String {
    val parts = mutableListOf<String>()
    wifi.signalPercent?.let { parts += "$it% signal" }
    wifi.frequencyMhz?.let { mhz ->
        parts += when {
            mhz in 2400..2500 -> "2.4 GHz"
            mhz in 4900..5900 -> "5 GHz"
            mhz in 5925..7125 -> "6 GHz"
            else -> "$mhz MHz"
        }
    }
    return parts.joinToString(" · ")
}

/**
 * Put `text` on the clipboard.
 *
 * The reason this screen exists at all: an address is only useful once it is
 * in an SSH client or a browser, and retyping `172.27.154.85` off a TV is
 * exactly the friction the ticket is about.
 */
private fun copyToClipboard(context: Context, label: String, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
    if (clipboard == null) {
        Toast.makeText(context, "No clipboard available", Toast.LENGTH_SHORT).show()
        return
    }
    clipboard.setPrimaryClip(ClipData.newPlainText(label, text))
    Toast.makeText(context, "Copied $text", Toast.LENGTH_SHORT).show()
}

/** A monospace value with a copy button. */
@Composable
private fun CopyableRow(value: String, label: String, emphasis: Boolean = false) {
    val context = LocalContext.current
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            value,
            fontFamily = FontFamily.Monospace,
            fontWeight = if (emphasis) FontWeight.SemiBold else FontWeight.Normal,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        IconButton(onClick = { copyToClipboard(context, label, value) }) {
            Icon(Icons.Filled.ContentCopy, contentDescription = "Copy $value")
        }
    }
}

/**
 * Where the device is on the network (issue #182).
 *
 * Read-only. This app reached the device over Bluetooth and would otherwise
 * have no idea what its address is — which is what stands between a caregiver
 * and either SSH or the web interface.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NetworkScreen(vm: DeviceViewModel, onBack: () -> Unit) {
    val state by vm.state.collectAsState()
    val net by vm.network.collectAsState()
    var showOther by remember { mutableStateOf(false) }

    LaunchedEffect(state.link) {
        if (state.link != LinkStatus.Connected) return@LaunchedEffect
        while (true) {
            vm.refreshNetwork()
            delay(POLL_INTERVAL_MS)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Network") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = vm::refreshNetwork, enabled = !net.loading) {
                        if (net.loading) {
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
            net.error?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            val status = net.status
            if (status == null) {
                if (!net.loading) {
                    Text(
                        "Nothing read yet.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                return@Column
            }

            AssistChip(
                onClick = {},
                enabled = false,
                label = { Text(connectivityLabel(status.connectivity)) },
                colors = AssistChipDefaults.assistChipColors(
                    disabledLabelColor = connectivityColor(status.connectivity),
                ),
            )

            when (status.source) {
                NetworkSource.UNAVAILABLE -> Text(
                    "This device could not read its own network. That is not the same as " +
                        "being offline.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                NetworkSource.INTERFACES -> Text(
                    "NetworkManager is not running on the device, so the addresses below " +
                        "are real but the network name, gateway and DNS are not available.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                else -> {}
            }

            WebInterfaceCard(status.managementApi, status.managementUrls)

            // From the service snapshot this app already holds, not from
            // `network_status`: the checks are pushed on every
            // `internet_status_changed`, so duplicating them onto the network
            // wire type would give one fact two sources that drift apart
            // between polls.
            ChecksCard(state.snapshot?.internetStatus.orEmpty())

            Text(
                if (net.reachable.isEmpty()) {
                    "No interface can be reached from another machine"
                } else {
                    "Reachable from another machine"
                },
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.SemiBold,
            )
            net.reachable.forEach { InterfaceCard(it) }

            if (net.other.isNotEmpty()) {
                TextButton(onClick = { showOther = !showOther }) {
                    Text(
                        if (showOther) {
                            "Hide other interfaces"
                        } else {
                            "Show ${net.other.size} other interface" +
                                if (net.other.size == 1) "" else "s"
                        },
                    )
                }
                if (showOther) net.other.forEach { InterfaceCard(it) }
            }

            if (status.truncated) {
                Text(
                    "This device has more interfaces than can be listed here.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/**
 * Where the web interface is listening — including when it is not.
 *
 * "Not" is the case this app is uniquely placed to report: it is on Bluetooth,
 * so it still works on a device whose network path is exactly what has broken.
 */
@Composable
private fun WebInterfaceCard(listener: WebListenerView, urls: List<String>) {
    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text("Web interface", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            when (listener.state) {
                WebListenerState.DISABLED -> Text(
                    "Not enabled on this device.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                WebListenerState.BINDING -> Text(
                    "Still starting — waiting for ${listener.addr} to exist. An address on " +
                        "an interface that comes up late, such as a VPN, can take a while.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                WebListenerState.FAILED -> Text(
                    "Configured for ${listener.addr} and not serving: " +
                        (listener.error ?: "no reason given") + ".",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                else -> if (urls.isEmpty()) {
                    Text(
                        "Serving on ${listener.addr}, but this device has no address " +
                            "another machine could reach it at.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    Text(
                        "Open from a browser at:",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    urls.forEach { CopyableRow(it, label = "lunchbox web interface", emphasis = true) }
                }
            }
        }
    }
}

/**
 * The configured connectivity checks and their latest result (issue #182).
 *
 * The reason to show them beside the interfaces rather than on the health
 * screen: "online" is one word for several different failures, and a device
 * with an address, a gateway and a check that keeps failing is the shape of a
 * captive portal or a blocked egress rule — which is a question about *this*
 * page, not a device fault. Also the only place a caregiver can see why an
 * activity that requires the internet is being held back.
 */
@Composable
private fun ChecksCard(checks: List<InternetStatusView>) {
    if (checks.isEmpty()) return
    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "Connectivity checks",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                "Activities that require the internet are held back when their check last " +
                    "failed.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            checks.forEach { check ->
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    AssistChip(
                        onClick = {},
                        enabled = false,
                        label = { Text(if (check.available) "Reachable" else "Unreachable") },
                        colors = AssistChipDefaults.assistChipColors(
                            disabledLabelColor = if (check.available) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.error
                            },
                        ),
                    )
                    Text(
                        check.target,
                        fontFamily = FontFamily.Monospace,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

/** One interface: what it is, what it is called, and how to reach it. */
@Composable
private fun InterfaceCard(iface: NetworkInterfaceView) {
    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    iface.name,
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.SemiBold,
                )
                AssistChip(onClick = {}, enabled = false, label = { Text(kindLabel(iface.kind)) })
                if (!iface.up) {
                    AssistChip(onClick = {}, enabled = false, label = { Text("Down") })
                }
            }

            iface.wifi?.let { wifi ->
                val ssid = wifi.ssid
                if (ssid == null) {
                    Text(
                        "Not connected to a network.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    val summary = wifiSummary(wifi)
                    Text(
                        "Connected to $ssid" + if (summary.isEmpty()) "" else " — $summary",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }

            if (iface.addresses.isEmpty()) {
                Text(
                    "No address.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                iface.addresses.forEach { address ->
                    CopyableRow(address.address, label = "${iface.name} address")
                }
            }

            val details = buildList {
                iface.gateway?.let { add("Gateway $it") }
                if (iface.dns.isNotEmpty()) add("DNS ${iface.dns.joinToString(", ")}")
            }
            if (details.isNotEmpty()) {
                Text(
                    details.joinToString(" · "),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
