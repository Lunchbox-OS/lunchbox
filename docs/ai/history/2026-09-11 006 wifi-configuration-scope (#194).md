# WiFi configuration from the management UIs (#194) — scope

**Prompt:** "scope out #194"

**Issue:** <https://git.armeafamily.com/albert/shepherd-launcher/issues/194>

> Following up on #182, it would be useful to actually be able to select the
> WiFi network over BLE management to unblock the network connection without
> having to temporarily log into a different graphical session.
>
> For this exercise, you should be able to:
> * Pick an SSID from the available ones, or enter one manually
> * Enter a password if required (and pick the encryption method if SSID was
>   manually entered)
>
> Usage from Web management would also be useful to set up known networks ahead
> of time.
>
> Explicitly out of scope for now:
> * Manual IP address configuration -- let's stick with the default DHCP for now
> * Captive portals -- this is best done as an admin mode (#154) task

This is a scope investigation, not an implementation.

## The shape of the feature

There are two operations here, and they suit different transports:

* **Join**: save a profile and connect to it now. This is the BLE case. BLE is
  the only management transport that still works when the device has no
  network, which is exactly when a parent reaches for this. Pair, open Network,
  pick the SSID, type the password, and watch the #182 status page go green.
* **Remember**: save a profile and don't activate it. This is what the issue
  asks the web for ("set up known networks ahead of time"). It also matters
  because joining from a browser is the one action that can cut the browser off
  (see §4).

Both need a list of saved networks with **Forget**. Otherwise a mistyped
password or a network that no longer exists can only be removed over SSH.

## What exists today

### The read half is done (#182)

`LinuxNetworkInfo` (`crates/shepherd-host-linux/src/network.rs`) already reads
NetworkManager over `zbus`. It opens a fresh connection per call, has a 3 s
timeout, and falls back to `getifaddrs`. `NetworkStatusView` already carries the
associated SSID, signal and frequency. `NetworkPage.tsx` (479 lines) and
`NetworkScreen.kt` (468 lines) render it. So the check after a join ("did it
work, and what is my address now?") is a plain `network_status` re-read, with no
new plumbing.

As in #182, one `ManagementService` method is reachable over both HTTP and BLE
the moment it compiles. Wire types go through `shepherd-wire-codegen`. There is
no per-method authorization: an authenticated caller is an admin.

### NetworkManager has everything needed

NM 1.54.3 on the dev device. Introspection confirms the calls this needs:
`Settings.AddConnection2`, `NetworkManager.AddAndActivateConnection2`,
`Device.Wireless.RequestScan` / `GetAllAccessPoints` / `LastScan`, and the
`Checkpoint*` family.

The profiles GNOME created on this device are **system** profiles:
`connection.permissions` is empty and `psk-flags = 0` (NM stores the secret
itself). That is the shape to write. They also show a trap. There are two
profiles for the *same* SSID, `DIRECT-04-HP OfficeJet 250` (`sae`) and
`DIRECT-04-HP OfficeJet 250 1` (`wpa-psk`). Adding a profile with the same name
creates a second one rather than replacing the first.

### Permissions are the crux

Measured with `pkaction` (implicit defaults):

| action | active local session | anyone else |
| --- | --- | --- |
| `network-control` (activate/deactivate) | yes | auth_admin |
| `wifi.scan` | yes | auth_admin |
| `settings.modify.own` (private profile) | yes | auth_self_keep |
| `settings.modify.system` (system profile) | **auth_admin_keep** | auth_admin_keep |

Ubuntu's `/usr/share/polkit-1/rules.d/org.freedesktop.NetworkManager.rules`
grants `settings.modify.system` to local, active members of `sudo` or `netdev`.
The installed kiosk user is in neither:

```
shepherd-kiosk: shepherd-kiosk video users input bluetooth shepherd-firewall shepherd-waydroid
```

`SHEPHERD_REQUIRED_GROUPS` (`scripts/lib/install.sh:480`) is `input video
bluetooth`. So on an installed device, `shepherdd` can **scan** and **switch to a
network that is already saved**, but it **cannot save a new one**. The kiosk
session has no polkit agent, so the call fails with NotAuthorized instead of
prompting. (Every row reads `auth` in `nmcli general permissions` over SSH
because an SSH login is not an active local session. That is the right-hand
column, not the kiosk's.)

### The custodian is the only part of shepherd not running as the kiosk user

`shepherd-stated` runs as `shepherd-state` in a system unit. It accepts only the
kiosk session's cgroup (activities cannot reach it). It already talks to the
system bus (logind), and it already holds one polkit grant with a written
justification (`dist/polkit/50-shepherd-session-guard.rules`, "What this widens,
said plainly"). Its sandbox is `RestrictAddressFamilies=AF_UNIX` plus
`PrivateNetwork=yes`. The system bus is a Unix socket, so NM is reachable from
inside it without loosening either setting.

### Admin mode is already the on-device fallback

#154 lets a parent open `gnome-control-center` or `nm-connection-editor` from the
picker. That is why captive portals and anything exotic can stay out of this
ticket.

## The decision that shapes the rest: who writes the profile

**A. Add the kiosk user to `netdev`.** A one-line install change, and
`shepherdd` calls NM directly. But the grant goes to every process running as
the kiosk user, which means every activity. `settings.modify.system` covers
rewriting or deleting the parent's profiles (including DNS or a VPN), and, per
NM's permission model, `GetSecrets` on them. A child who can run `nmcli -s
connection show` would learn the house WiFi password. *(Confirm the
`GetSecrets` gating on 1.54 before relying on this argument either way.)* The
custodian was built (#157, #172) because activities run as the same user as
`shepherdd`, so granting the whole kiosk user network-configuration rights
would reverse that stance.

**B. Private profiles via `settings.modify.own`.** No new grant. But a private
profile activates only while that user is logged in. That means no network at
the greeter, for SSH before login, or for a second kiosk user. And the same user
account, activities included, can read and delete it, so it has A's exposure
with worse behaviour. Rejected.

**C. Route writes through the custodian (recommended).** Add a polkit rule
granting `shepherd-state` `network-control`, `wifi.scan` and
`settings.modify.system`. Add typed `StateRequest` variants and have
`shepherdd`'s wifi implementation call the custodian. Activities gain nothing.
The costs:

* The custodian's job grows beyond files plus the watchdog.
* polkit cannot narrow the grant to wifi profiles, so it really lets
  `shepherd-state` change any NM setting. The mitigation is the custodian's
  shape: typed operations only, with no raw settings dictionary accepted over
  the socket.

The custodian runs one instance per kiosk user while polkit grants by user
account, so this works. And profiles are device-wide, like the admin record
already in `/var/lib/shepherdd/admin`.

**D. A `pkexec` helper, like `shepherd-firewall-helper`.** Its polkit rule is
gated on a group the kiosk user is in, so it has the same exposure as A.
Rejected.

The rule for C: **anything that changes the device goes through the
custodian.** Reads (the AP list, saved profiles, a join's progress) stay in
`shepherdd`, next to #182's reader, because NM properties are readable without
any grant.

With no custodian (dev sessions run `--no-state-custodian`, and there is also
the fallback store), try NM directly and report a typed "not authorized" if
polkit refuses. Never pretend it worked.

## Proposed shape

### 1. Wire types (extend `crates/shepherd-api/src/network.rs`)

* `WifiSecurity`: `Open`, `Owe` (Enhanced Open), `WpaPsk` (WPA/WPA2 Personal,
  which also joins WPA2/WPA3 transition APs), `Sae` (WPA3 Personal),
  `Enterprise` and `Wep`. The last two appear in scan results but can't be
  joined here. Manual entry offers Open / WPA2 Personal / WPA3 Personal.
* `WifiNetworkView` (a scan entry, **aggregated by SSID** across BSSIDs and
  bands): `ssid`, `security`, `signal_percent` (best), `bands`, `saved`,
  `active`. No BSSID, matching #182's no-MACs stance. Hidden (empty) and
  non-UTF-8 SSIDs are left out: they can't be named in a picker, and manual
  entry covers hidden networks.
* `WifiScanView`: `networks`, `radio_enabled`, `last_scan_age_s`, `truncated`,
  and `join: WifiJoinState` (so one poll serves the picker and the progress
  indicator). Cap at `MAX_WIFI_NETWORKS ≈ 40`: at about 150 bytes of JSON per
  entry that is roughly 6 KiB, well under BLE's 16 KiB frame.
* `SavedWifiNetworkView`: `id` (NM UUID; the SSID is not unique, see above),
  `ssid`, `security`, `hidden`, `autoconnect`, `active`. **Never a secret.**
* `WifiJoinRequest`: `ssid`, `security`, `password: Option<String>`, `hidden`,
  `connect: bool`.
* `WifiJoinState`: `Idle | Connecting { ssid } | Connected { ssid } | Failed {
  ssid, reason }`, with reason `WrongPassword | NotFound | Timeout |
  NotAuthorized | Other(String)`.

### 2. RPC methods

| method | does |
| --- | --- |
| `wifi_scan()` | starts a scan and returns at once (NM rate-limits rescans; "too soon" counts as success) |
| `wifi_networks() -> WifiScanView` | the current AP list plus the last join's state |
| `wifi_saved_networks() -> Vec<SavedWifiNetworkView>` | wireless profiles only |
| `wifi_save(request) -> SavedWifiNetworkView` | `connect: false` remembers the network, `true` joins it |
| `wifi_connect(id)` | activate a saved profile |
| `wifi_forget(id) -> bool` | delete a profile |

**No call waits for the network to connect.** The companion's RPC timeout is
15 s (`ShepherdConnection.REQUEST_TIMEOUT_MS`). Association plus DHCP
routinely takes 5–20 s, and with no secret agent a wrong password fails only
after the supplicant gives up. So a call returns once NM has accepted the
request, and the UI polls `wifi_networks`. #182 made the same choice: poll
rather than push, with no new `EventPayload` variant.

The password is a JSON-RPC parameter. Neither dispatcher appears to log
parameters (nothing in `dispatch.rs`, `shepherd-ble/src/rpc.rs` or the HTTP
handlers), but confirm that while implementing, and add a test. Validate on the
server before NM sees the request:

* SSID: 1–32 bytes.
* WPA-PSK: 8–63 printable ASCII characters or 64 hex digits.
* SAE: non-empty.

Audit it like `PolicyWritten` / `AdminAppLaunched` do, with
`WifiNetworkSaved { ssid }` and `WifiNetworkForgotten { ssid }` and no secret.

### 3. The NetworkManager mechanics (where the bugs will be)

* **The profile to write** matches the GNOME-created ones: `802-11-wireless`,
  `autoconnect = true`, `permissions = []`, `ssid` as `ay`, `hidden` for manual
  entry, `key-mgmt` = `none`/`owe`/`wpa-psk`/`sae`, `psk`, `psk-flags = 0`,
  `ipv4.method = auto`, `ipv6.method = auto`.
* **Saving a profile for an SSID that already has one updates it in place**
  (`Update2`). Otherwise re-entering a corrected password produces `SSID 1`,
  `SSID 2`, as this device already shows.
* **Remember** is `AddConnection2(settings, TO_DISK)`.
* **Join a new network** is `AddAndActivateConnection2(…, {persist: "volatile"})`,
  then `Update2(…, TO_DISK)` once the connection is `ACTIVATED`. NM discards a
  failed volatile profile itself. Without this, a wrong password leaves a broken
  profile that autoconnects forever. *Check the volatile → on-disk transition on
  1.54.*
* **Keep custodian requests short.** The custodian starts the activation and
  returns the active-connection path. `shepherdd` watches it (read-only), then
  asks the custodian to persist a profile it created. The custodian persists
  only volatile wifi profiles it created in that run.
* **Wrong-password detection.** No secret agent runs in the kiosk session, so a
  bad PSK should end activation with `NO_SECRETS` (7), or sometimes
  `SUPPLICANT_DISCONNECT` (8) or `SUPPLICANT_TIMEOUT` (11). The mapping comes
  from the active connection's `StateChanged(state, reason)`. **Which reason
  actually arrives has to be measured on real hardware.** Unit tests can't
  settle it.
* **Security from a scan** comes from the AP's `Flags` / `WpaFlags` / `RsnFlags`
  (`0x100` psk, `0x200` 802.1X, `0x400` SAE, `0x800` OWE). A transition AP
  (PSK + SAE) gets `wpa-psk`, which every card supports. `sae` is used only when
  the AP offers nothing else.
* **Choosing the device.** Filter to `DeviceType = 2` (wifi). This device also
  has a `p2p-dev-wlx…` device of type 30 (wifi-p2p), which must not be picked.
  If there are several wifi adapters, use the first managed one; letting the
  user choose is out of scope.

### 4. Changing the network a client is connected over

* **BLE:** unaffected. This is why it's the primary path.
* **Web:** if the browser reached the device over the wifi being changed,
  joining (or forgetting the active network) drops the page. The device then
  shows up at a new address, which the companion's Network screen shows. The
  web UI should lead with **Save for later** and put a confirmation in front of
  **Connect now** ("this device will leave *X*; this page will stop
  responding"). Optional polish: the HTTP handler knows the request's local
  address, so it can warn only when the request actually arrived over the wifi
  interface.
* **NM checkpoint/rollback**: considered and not proposed for v1. If the new
  network fails to activate, autoconnect already falls back to the other saved
  networks.

### 5. The two UIs

* **Companion:** a Wi-Fi section on `NetworkScreen.kt`, or a
  `ui/network/WifiScreen.kt` it links to. It has:
  * a scan list with a lock icon, signal strength, and *Saved* / *Connected*
    tags;
  * tap a network → password sheet → "Connecting to X…" driven by polling →
    result;
  * "Other network…" for manual entry: SSID, security dropdown, hidden;
  * a saved-networks list with Forget.
* **Web:** the same on `NetworkPage.tsx`, with "Add network" (save for later)
  and the saved list first.
* **Codegen:** register the new types in `wire_schema.rs` and regenerate. The
  drift test enforces the rest.

### 6. A diagnostic for a grant that isn't there

Following `session_not_guarded`: at startup the custodian asks polkit (with no
interaction) whether `shepherd-state` holds `settings.modify.system`. If not,
and the device has a wifi adapter, `shepherdd` raises a Warning
`wifi_config_unavailable` whose remedy names the rules file. It then shows on
the Health page and the companion Health screen without further work.

## Explicitly out of scope

From the issue: manual IP configuration, and captive portals (#154).

Also recommended out, with admin mode as the fallback for each:

* 802.1X / Enterprise: identity, certificates and phase-2 auth need a UI of
  their own. Scan results show these networks as "not supported here".
* WEP.
* Choosing between several wifi adapters.
* Toggling the wifi radio on or off. `radio_enabled` is reported, not changed.
* Hotspot mode.
* A picker on the device itself.
* Per-network proxy, metered, priority and MAC randomization settings.
* Reading a saved password back. Secrets are write-only.

## Open questions for the maintainer

1. ~~**Privilege path: C (custodian) or A (`netdev`)?**~~ **Decided: C.** The
   maintainer agreed the custodian is the right home, and asked what else
   belongs there — see `2026-09-11 007 custodian-candidates (#194).md`.
2. **May the web join a network immediately (with a warning), or only save
   networks?** Recommend allowing it with the warning.
3. **Enterprise networks in v1?** Recommend no.

## Testing

* **Unit:**
  * AP flags → `WifiSecurity`;
  * aggregation by SSID;
  * PSK/SAE validation;
  * the profile dictionary, checked against the shape GNOME wrote;
  * state reason → `WifiJoinState`;
  * a full-size `WifiScanView` fits in a BLE frame;
  * passwords never appear in `Debug` output or logs.
* **Through `dispatch_json`:** a mock `WifiManager` behind `MockSvc` and
  `DefaultManagementService`.
* **Custodian:** `StateRequest` round-trips, and the request is refused for a
  profile the custodian didn't create.
* **Real NM without endangering the dev device.** The dev device's only wifi
  adapter carries the SSH/ZeroTier connection an agent works over, so joining a
  network on it can cut the agent off. `mac80211_hwsim` is present on this
  kernel and `wpa_supplicant` is installed (AP mode, `mode=2`); `hostapd` is
  not. Use two hwsim radios, one as an access point and one that NM joins. This
  gives an `#[ignore]`d integration test for the wrong-password reason, the
  volatile → persist step, update-in-place and forget. It needs root to load
  the module, so it won't run in CI.
* **End to end:**
  * The headless dev session runs `--no-state-custodian` and isn't an active
    logind session, so the write path can't be driven there. Use `headless-dev`
    for web UI screenshots (with a mock, or in the "not authorized" state).
  * Verify the real write path on the installed kiosk, the way #172 was.
  * Verify the phone flow with the `companion-pairing` skill.

## Rough size

**Medium**, clearly bigger than #182. The read side exists, but this is
shepherd's first network *change*. It brings a new polkit grant, an extension to
the custodian's protocol, a join flow that only reports its result later, and
two forms.

| piece | size |
| --- | --- |
| Wire types + codegen | small |
| Scan + saved-profile reads next to `LinuxNetworkInfo` | small–medium |
| Custodian NM write operations + polkit rule + install script + startup check / diagnostic | medium |
| Join flow (volatile → persist, update in place, failure reasons) | **medium: the part that has to be measured on hardware** |
| `ManagementService` methods + `MockSvc` + audit events | small |
| Companion Wi-Fi UI | medium |
| Web Wi-Fi UI | small–medium |
| hwsim test bed (optional) | small–medium |

Suggested PR slicing, each shippable on its own:

1. Read-only scan list and saved networks on both UIs. No privilege change.
2. The custodian write path: save for later, and forget.
3. Join now, with progress and failure reasons.
