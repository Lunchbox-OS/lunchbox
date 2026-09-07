# Network status page in the management UIs (#182) — scope

**Prompt:** "scope out #182"

**Issue:** <https://git.armeafamily.com/albert/shepherd-launcher/issues/182>

> Follow-up to #156.
>
> It would be useful to show the network status -- things like the WiFi network
> the device is connected to (if any), its IP address(es), the status of the
> network checks, and where the Web interface is listening (if at all). This is
> particularly useful from the management app to make it possible to SSH in or
> view the Web management page without having to `arp` or `nmap` for the device.
>
> This is read-only -- wifi reconfiguration, etc. are out of scope for this ticket.

This is a scope investigation, not an implementation.

## The shape of the feature

The *primary* consumer is the companion app over BLE. Over HTTP the client
already knows the device's address — it just typed it in. Over BLE it does not,
and that is exactly the case the issue is about: pair the phone, open a Network
screen, read "192.168.0.139" and "http://192.168.0.139:8080", then SSH or browse
without `arp`/`nmap`. The web UI gets the same page for symmetry and because it
is nearly free, but if anything gets cut, cut that one, not the phone.

## What exists today

### The management surface is one RPC, so a new read-only method is nearly free

`ManagementService` (`crates/shepherd-management/src/service.rs:44`) carries the
`#[shepherd_management_macros::management_rpc]` attribute, which generates a
JSON-RPC dispatcher over every trait method. Both transports are pass-throughs:

* HTTP is a **two-endpoint** API — `POST /api/v1/rpc` into `dispatch_json`, plus
  `GET /api/v1/events` for SSE (`crates/shepherd-http/src/handlers/mod.rs:22`).
  There is no per-method route to add.
* BLE dispatches by method name through the same function
  (`crates/shepherd-ble/src/rpc.rs:22`).

So `async fn network_status(&self) -> NetworkStatusView` on the trait is
reachable from both clients the moment it compiles. There is **no per-method
authorization** anywhere — an authenticated caller on either transport is admin
— so nothing needs gating work.

Only two impls exist: `DefaultManagementService`
(`crates/shepherd-management/src/service.rs:322`) and `MockSvc`
(`crates/shepherd-ble/src/testsupport.rs:36`). Both must grow the method.

### Generated client types cover both UIs

`shepherd-wire-codegen` emits the payload types for *both* clients from the Rust
definitions (`CONTRIBUTING.md:258`): register the new type in `WireTypes`
(`crates/shepherd-wire-codegen/src/wire_schema.rs`), run
`cargo run -p shepherd-wire-codegen --bin rpc-codegen`, and
`WireTypes.generated.kt` + `wire-types.generated.ts` follow.
`tests/rpc_codegen_drift.rs` fails until the regenerated output is committed.

### Half the data already flows

`ServiceStateSnapshot.internet_status` (`crates/shepherd-api/src/types.rs:1072`)
already carries the latest result of every configured connectivity check, on the
snapshot every client holds, with `InternetStatusChanged` deltas. **"The status
of the network checks" needs no new plumbing at all** — the page just renders
what it already has. `InternetMonitor` (`crates/shepherdd/src/internet.rs`) owns
the probing; the HUD already renders an aggregate of it
(`crates/shepherd-hud/src/app.rs:896`).

### NetworkManager is already a D-Bus dependency of shepherdd

`crates/shepherdd/src/system_events.rs` already holds a `#[zbus::proxy]` for
`org.freedesktop.NetworkManager` and subscribes to `StateChanged` to trigger an
immediate connectivity re-check. `zbus` is already a workspace dependency.

I probed the properties this feature needs on a live device as the unprivileged
`shepherd-admin` user — **all readable, no polkit prompt, no new dependency**:

| Want | Where |
| --- | --- |
| Overall connectivity / state | `…/NetworkManager` → `Connectivity`, `State`, `PrimaryConnection` |
| Interfaces | `…/NetworkManager` → `Devices`, then `Device.Interface` / `DeviceType` / `State` |
| SSID, signal, band | `Device.Wireless.ActiveAccessPoint` → `AccessPoint.Ssid` (`ay`), `Strength`, `Frequency` |
| Addresses | `Device.Ip4Config` → `AddressData` (`address`+`prefix`), `Gateway`, `NameserverData`; ditto `Ip6Config` |

The dev device shows exactly why "IP address**es**" is plural in the issue:
alongside `wlx28187845b61d` at 192.168.0.139 there is a ZeroTier interface
(`ztks5unao4`) and an LXC bridge. The SSH use case wants **every** usable
address, not just the wifi one — and `config.example.toml:201` already documents
`bind_retry_seconds` existing *because* the API binds to a ZeroTier address that
appears late.

### Nothing anywhere knows an SSID or an IP

`grep` for `ssid|wlan|getifaddrs|local_ip` across `crates`, `shepherd-webui`,
and `companion-android` finds exactly one hit: a UDP-connect trick in
`crates/shepherd-media-android/src/handoff.rs:87` used to build a handoff URL.
This is all new code.

### The web listener's real state is invisible

`HttpServer::run` (`crates/shepherd-http/src/lib.rs:64`) binds, logs
`"Management HTTP API listening"`, and serves. Nothing retains the bound
`SocketAddr`, and on failure `shepherdd` **only logs**:

```rust
if let Err(e) = http_server.run(http_shutdown_rx).await {
    error!(error = %e, "HTTP management API error");   // main.rs:1671
}
```

So today a device whose management API never bound is indistinguishable, from
every UI, from one that did. That is precisely the "(if at all)" in the issue,
and it is the one piece of this ticket that is not just a read-out — it needs a
new observable.

## Proposed shape

### 1. `NetworkStatusView` in `shepherd-api`

One wire type, deliberately bounded (`ServiceStateSnapshot` notes a 16 KiB BLE
frame cap; this rides its own RPC, but the same discipline applies):

* `connectivity` — NM's connectivity enum, mapped to our own snake_case enum.
* `interfaces: Vec<NetworkInterfaceView>` — `name`, `kind` (wifi / ethernet /
  vpn / bridge / loopback / other), `up`, `addresses: Vec<String>` (CIDR),
  `gateway`, plus `wifi: Option<WifiView>` (`ssid`, `signal_percent`, `band`).
* `management_api: WebListenerView` — see below.
* Loopback and container bridges are reported but flagged, so the UI can lead
  with the addresses a parent can actually reach.

Recommend **omitting MAC addresses and BSSIDs**. They are the fingerprintable
part, and neither serves "SSH in or open the web UI". Easy to add later.

The connectivity-check results are *not* duplicated here — the page reads
`internet_status` off the snapshot it already has.

### 2. A `NetworkInfoProvider` trait in `shepherd-host-api`

Follows the `VolumeController` / `BrightnessController` pattern exactly: trait
in `crates/shepherd-host-api/src/traits.rs`, NM-backed impl in a new
`crates/shepherd-host-linux/src/network.rs`, a stub in
`crates/shepherd-host-api/src/mock.rs`, composed into
`DefaultManagementService` like the other collaborators. Keeps
`shepherd-management` host-agnostic and keeps the feature testable without a
D-Bus daemon.

A missing or broken NetworkManager must degrade, not fail: the existing
`system_events.rs` doc comment already sets that precedent ("Missing D-Bus /
NetworkManager / logind is non-fatal"). Fall back to `getifaddrs` (needs
`nix`'s `net` feature; `nix` is already a workspace dep) for interfaces and
addresses, with no SSID.

### 3. Make the web listener observable

`HttpServer` needs to publish `NotConfigured | Binding { addr } | Listening
{ addr } | Failed { addr, error }`. The type must live in `shepherd-management`
(or `shepherd-api`), not `shepherd-http` — `shepherd-http` depends on
`shepherd-management`, so putting it the other way round is a dependency cycle.
A `tokio::sync::watch` handle written by `run()` and read by the service is
enough.

Two things fall out of this, both worth doing here:

* When bound to `0.0.0.0`, the UI can render one reachable URL per interface
  address (`http://192.168.0.139:8080`) rather than a useless `0.0.0.0:8080`.
* A `Failed` listener is a textbook administrator-facing condition and
  `shepherd-api`'s `DiagnosticSet` already exists for exactly this
  (`crates/shepherd-api/src/diagnostics.rs`). A new
  `DiagnosticCode::ManagementApiUnavailable` surfaces the failure on the
  existing Health page *and* the companion Health screen for free, with a
  remedy string. **This is the highest-value line item in the ticket** — it
  turns a silent failure into something a parent can see — and it is a handful
  of lines given the diagnostics machinery.

### 4. Freshness: poll, don't push (at first)

The page is transient, so `refetchInterval` in the web UI's react-query and a
re-fetch on resume in the companion is enough. NM's `StateChanged` signal is
already watched, so a `NetworkChanged` nudge event could be added later — but a
new `EventPayload` variant is a wire change both clients must handle and costs
BLE frames on a device where nothing is looking at the page. Not worth it for
v1.

### 5. The two UIs

* **Web**: a `NetworkPage.tsx` next to `DiagnosticsPage.tsx` (162 lines, the
  right model to copy), one `getNetworkStatus` line in
  `shepherd-webui/src/api/client.ts:214`, one `NAV` entry in `App.tsx:49`.
* **Companion**: a `ui/network/NetworkScreen.kt` next to `ui/health/`
  (237 lines), a `Routes.NETWORK` + `composable` in `ui/App.kt:145`, a
  `RpcMethods.NETWORK_STATUS` const and a `ManagementClient.networkStatus()`.
  Addresses and URLs should be **tap-to-copy** — the whole point is getting
  `192.168.0.139` out of the phone and into an SSH client.

## Relationship to #156

#182 is filed as a follow-up but does **not** depend on #156. Nothing here
changes the auth model: the data is visible to callers who are already fully
authorized to launch sessions and read usage. Worth stating plainly, though,
that #156 remains open — the HTTP bearer token still crosses the LAN in
plaintext, so this page adds SSID and internal addressing to what a sniffer on
the same network already gets. It makes #156 slightly more valuable; it does not
block on it.

## Explicitly out of scope

Per the issue: any wifi (re)configuration, joining networks, forgetting
networks, toggling adapters. Read-only.

## Rough size

Small-to-medium, and unusually well-supported by what is already here — one RPC
endpoint for both transports, generated client types, an existing D-Bus
dependency with the properties confirmed readable unprivileged, and an existing
diagnostics channel for the failure case.

| Piece | Size |
| --- | --- |
| `NetworkStatusView` + codegen registration + regenerate | small |
| `NetworkInfoProvider` trait + NM impl + mock + fallback | **the bulk of it** |
| Trait method on `ManagementService` + `MockSvc` | small |
| Web listener status observable + `ManagementApiUnavailable` diagnostic | small-medium |
| `NetworkPage.tsx` + nav + client call | small |
| `NetworkScreen.kt` + route + client call | small |

Tests: unit tests for the NM property decoding (non-UTF-8 SSIDs are a real case
— `Ssid` is `ay`, not `s`), a mock-provider test through `dispatch_json`, the
codegen drift test, and an end-to-end look via the `headless-dev` skill for the
web page. The companion screen wants a device pass via `companion-pairing`.
