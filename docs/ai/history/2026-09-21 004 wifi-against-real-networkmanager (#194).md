# What real NetworkManager does to #194's plan — a measurement pass

**Prompt:** "let's pick up where we left off on #194" → measure on hardware
first, before writing feature code.

**Issue:** <https://github.com/aarmea/lunchbox/issues/194>

Follow-up to `2026-09-11 006 wifi-configuration-scope (#194).md`. That note scoped the
feature and ended with a list of things it said could only be settled on real
hardware — the wrong-password reason code, the volatile → persist transition on
NM 1.54, update-in-place, and whether `GetSecrets` really is gated the way the
argument against option A assumed. This note is that measurement pass.

It is not an implementation. Nothing in `crates/` changed.

Two of the scope note's open questions were also answered by the maintainer
while this ran:

* **May the web join a network immediately?** Yes, with the warning. Lead with
  *Save for later*; put *Connect now* behind a confirmation.
* **Enterprise (802.1X) in v1?** No. Admin mode (#154) is the fallback.

## Headline: the scope note got one mechanism wrong, and missed one entirely

1. **The failure reason is on the `Device`, not the `ActiveConnection`.** The
   scope note's §3 says "the mapping comes from the active connection's
   `StateChanged(state, reason)`". Measured, that signal is useless: every
   failure, whatever the cause, reports `DEACTIVATED reason=3
   DEVICE_DISCONNECTED`. The cause only ever appears on
   `org.freedesktop.NetworkManager.Device.StateChanged`.
2. **This Ubuntu stores NM profiles through netplan, not keyfiles.** The scope
   note never mentions netplan. It changes where secrets live, what a saved
   profile actually contains, and — see §3 below — what a *forget* can destroy.

## The test bed

The dev device's only wifi adapter (`wlx28187845b61d`) carries the ZeroTier
link an agent works over, so it was never touched. Everything below ran on
`mac80211_hwsim` virtual radios.

* `modprobe mac80211_hwsim radios=4` → `wlan0` (station, NM-managed) plus
  `wlan1`/`wlan2`/`wlan3` (marked unmanaged, `nmcli device set wlanN managed no`).
* `hostapd` is not installed, so the APs are `wpa_supplicant` with `mode=2`:
  `lunchbox-hwsimtest` (WPA2-PSK, 2412), `lunchbox-wpa3` (SAE, `ieee80211w=2`,
  2437), `lunchbox-open` (open, 2462).
* A `dnsmasq --port=0` per AP radio for DHCP, so activations reach `ACTIVATED`
  rather than stalling at IP config.
* The probe itself is raw D-Bus (python-dbus), not libnm, because the Rust side
  will use `zbus` against the same raw API.

Two notes for whoever rebuilds this:

* **The APs take ~30 s to come up**, not 3. `wpa_supplicant` in AP mode scans
  first, and on a shared virtual medium those scans collide
  (`CTRL-EVENT-SCAN-FAILED ret=-16` repeatedly) before `AP-ENABLED`. Poll
  `wpa_cli status` for `wpa_state=COMPLETED` instead of sleeping.
* **Never tear the bed down with `pkill -x wpa_supplicant`** — that kills
  NetworkManager's own supplicant and drops the live link. Match on the AP
  config filename and skip any process whose argv has ` -u `. (`pkill -f` also
  happily matches the shell you are typing into, if the pattern appears in its
  argv. It does, when the command contains the script.)

Environment: Ubuntu 26.04.1, kernel 7.0.0-31, NetworkManager 1.54.3,
wpa_supplicant 2.11.

## 1. Failure reasons, measured

Every row below was read off `Device.StateChanged` over D-Bus **and**
corroborated against the reason string NetworkManager writes to its own
journal, because a numeric code is easy to mis-decode. (It was: an earlier pass
in this session read 53 as `NEW_ACTIVATION`. 53 is `SSID_NOT_FOUND`; 39 is
`USER_REQUESTED`.)

| what happened | terminal device transition | code | NM's own word | time |
| --- | --- | --- | --- | --- |
| wrong PSK | `need-auth -> failed` | **7** | `no-secrets` | 3.2 – 24.7 s |
| SSID not in range | `config -> failed` | **53** | `ssid-not-found` | ~25 s, consistently |
| associated, no DHCP | `ip-config -> failed` | **5** | `ip-config-unavailable` | ~45 s |
| success | `secondaries -> activated` | 0 | `none` | 3.3 – 22 s |

The active connection reports `DEACTIVATED / 3 DEVICE_DISCONNECTED` for all
three failures. It cannot tell them apart.

`SUPPLICANT_DISCONNECT` (8) does appear on the wrong-password path, but as an
intermediate `config -> need-auth` hop, never as the terminal reason. The scope
note listed it as one of the possible outcomes; it is not one. Only 7 is.

So the `WifiJoinState::Failed` mapping is:

| device reason | `reason` |
| --- | --- |
| 7 `no-secrets` | `WrongPassword` |
| 53 `ssid-not-found` | `NotFound` |
| 5 `ip-config-unavailable` | a new variant — associated but no address |
| anything else | `Other(name)` |

That fourth row matters for the UI. "Associated, but DHCP never answered" is
not a wrong password and must not be shown as one; the user's action is to
check the router, not to retype the key.

### The timings vindicate the polling design

The spread is wide and the tail is long. A wrong password took as little as
3.2 s and as much as 24.7 s across five runs of an *identical* request; the
DHCP failure took 45 s. The companion's RPC timeout is 15 s
(`DeviceConnection.REQUEST_TIMEOUT_MS`). Several of these outcomes cannot be
returned from the call that started them. The scope note's "no call waits for
the network to connect" is right, and it is not a stylistic choice.

### Autoconnect races a join in flight

A join started while a saved profile is eligible can be preempted — an early
run failed this way before the saved test profiles were cleared. Worth
handling: a join should block autoconnect on other profiles for its duration,
or at least not report the resulting failure as the user's fault.

## 2. netplan is the settings backend, and it rewrites what you give it

`NetworkManager.conf` says `plugins=ifupdown,keyfile`, which reads like a plain
keyfile setup. It is not. On this Ubuntu, `/etc/NetworkManager/system-connections/`
is **empty**, and a saved profile lands in two places:

* `/etc/netplan/90-NM-<uuid>.yaml` — the real stored profile, `0600 root:root`.
* `/run/NetworkManager/system-connections/netplan-NM-<uuid>-<id>.nmconnection`
  — a generated shadow copy NM reads back, also `0600 root:root`.

The PSK is written **in plaintext** into the netplan YAML:

```yaml
      access-points:
        "lunchbox-hwsimtest":
          auth:
            key-management: "psk"
            password: "correcthorse"
```

`0600 root:root` keeps it away from the kiosk user, so this does not by itself
change the privilege argument — but it does mean the secret's resting place is
a file lunchbox does not manage, in a format lunchbox does not write, and any
"back up the device config" or factory-reset feature has to know that.

### netplan pins every profile to the interface it was created on

Nothing in the settings dictionary asked for this. The profile was written with
no `interface-name`, and came back with one:

```yaml
      match:
        name: "wlan0"
```

```ini
[connection]
interface-name=wlan0
```

The two profiles GNOME created on this device, long before any of this, have
exactly the same shape (`match: name: "wlx28187845b61d"`). So it is netplan's
normalisation, not something the probe did.

The consequence is a real support case: **replace the wifi dongle and every
saved network stops matching.** `wlx28187845b61d` is derived from the adapter's
MAC. A parent who swaps a broken USB adapter gets a device with saved networks
that silently never autoconnect — and no network, which is exactly the state
#194 exists to rescue them from, via a BLE path that still works. Worth a
follow-up issue; out of scope here.

### Round-tripping loses and renames settings

`permissions: []` and `autoconnect: true` do not survive into the YAML as
written — netplan defaults them. Anything it has no schema for is stuffed into
a `passthrough:` block (`wifi-security.auth-alg`, `ipv6.ip6-privacy`, …). The
scope note's proposed test, "the profile dictionary, checked against the shape
GNOME wrote", is still the right test, but it has to compare what NM reports
back **after** the netplan round trip, not the dictionary that was sent.

## 3. A forget rewrote files lunchbox never wrote

This is the finding that most needs to reach the implementation.

Deleting a connection does not just delete its own
`/etc/netplan/90-NM-<uuid>.yaml`. netplan rewrites the whole directory. During
this pass it:

* stripped the comment line from `01-network-manager-all.yaml` (104 → 49
  bytes), and
* **deleted `/etc/netplan/00-installer-config.yaml` outright** — the file
  subiquity wrote at install time.

Both files' mtimes match the second of an NM `keyfile: deleting netplan
connection` log line. Neither was touched by anything else in this session.

Both were restored: the installer file byte-for-byte from curtin's own record
of what it wrote (`/var/log/installer/curtin-install/subiquity-curthooks.conf`
carries the literal content under `etc_netplan_installer`), and the comment on
the other from the stock Ubuntu text. Sizes and mode `0600` match the
originals, and `netplan get` parses.

For #194 this means `wifi_forget` — the most innocuous-sounding method in the
whole design — can destroy unrelated network configuration on the device,
including static or installer-provided config a household might depend on.
Before slice 2 ships, it needs either a check that `/etc/netplan` is unchanged
apart from the intended file, or an explicit decision that this is acceptable.
It should not be discovered by a user.

## 4. The privilege argument, now proven

The scope note rejected option A (put the kiosk user in `netdev`) partly on
this: *"per NM's permission model, `GetSecrets` on them. A child who can run
`nmcli -s connection show` would learn the house WiFi password"* — and flagged
it, honestly, with *"confirm the `GetSecrets` gating on 1.54 before relying on
this argument either way."*

Confirmed. There is no separate polkit action for reading secrets; `pkaction`
lists none. `settings.modify.system` alone is sufficient:

* as root — `GetSecrets` returns the PSK;
* as `shepherd-admin` over SSH (in `sudo`, but not a local active session) —
  `PermissionDenied`;
* as `shepherd-kiosk` — `PermissionDenied`;
* as `shepherd-admin` **with a temporary polkit rule granting only
  `settings.modify.system`** — `GetSecrets` returns the PSK.

That last case is the proof. The rule granted nothing else, and the subject was
not local-active, so `settings.modify.system` is doing all the work. Ubuntu's
stock rule grants exactly that action to local, active members of `sudo` or
`netdev`, and an activity running inside the kiosk's graphical session *is*
local and active. Option A would hand every activity the house WiFi password.

The rule was removed immediately afterwards and the denial re-verified;
`/etc/polkit-1/rules.d/` is back to just `50-shepherd-waydroid.rules`.

The measured permission table is otherwise unchanged from the scope note on
1.54.3: `network-control` and `wifi.scan` are `yes` for an active local
session, `settings.modify.own` is `yes`, `settings.modify.system` is
`auth_admin_keep`.

## 5. Join mechanics that worked as designed

* **Volatile → persist works.** `AddAndActivateConnection2(…, {persist:
  "volatile"})` then, once `ACTIVATED`, `Update2(settings, TO_DISK, {})` on the
  returned connection. Verified for WPA2-PSK, SAE and open. Keep the uuid in
  the settings passed to `Update2`.
* **NM discards a failed volatile profile itself.** After the wrong-password
  run, no profile remained in NM and nothing was on disk. The scope note's
  reason for using volatile in the first place holds.
* **Watch the return values.** `AddAndActivateConnection2` returns
  `(connection_path, active_connection_path, result)` — settings path *first*.
  Subscribing to `StateChanged` on the wrong one silently yields no signals.

### Update in place is mandatory, and for a worse reason than the note gives

`AddConnection2` with an identical id and SSID creates a second profile with a
new uuid — and, over D-Bus, **keeps the same name**. The scope note expected
NM to disambiguate to `SSID 1`, `SSID 2` (which is what this device shows, and
what `nmcli` does on its own side). It does not. The UI would show two rows
reading `lunchbox-hwsimtest`, identical in every visible field, one of them
with the old wrong password.

So `wifi_save` must look up saved profiles by SSID and `Update2` the match.
Verified: `Update2` changed the PSK in place, the uuid and profile count were
unchanged, and the netplan file was rewritten with the new secret. `Delete()`
removed both the profile and its YAML.

## 6. Scan results, measured

Flags from real beacons, all `Mode=2` (infrastructure):

| SSID | `Flags` | `WpaFlags` | `RsnFlags` |
| --- | --- | --- | --- |
| `lunchbox-hwsimtest` (WPA2-PSK) | `0x0003` PRIVACY\|WPS | `0x0000` | `0x0188` PAIR_CCMP\|GROUP_CCMP\|**KEY_MGMT_PSK** |
| `lunchbox-wpa3` (SAE) | `0x0003` PRIVACY\|WPS | `0x0000` | `0x0488` PAIR_CCMP\|GROUP_CCMP\|**KEY_MGMT_SAE** |
| `lunchbox-open` | `0x0002` WPS | `0x0000` | `0x0000` |

The bit values in the scope note (`0x100` psk, `0x200` 802.1X, `0x400` sae,
`0x800` owe) are right. Two things it does not say:

* **The open network has no `PRIVACY` bit.** So `Open` is "no PRIVACY and both
  flag words empty" — and **`WEP` is precisely `PRIVACY` set with both flag
  words empty**. That is the only way to detect WEP, and the classifier needs
  it to show those networks as unsupported rather than as open.
* **Filter devices on `DeviceType == 2` *and* `Managed == true`.** The
  unmanaged AP radios were `DeviceType=2` throughout. Type alone would have
  picked one. (The `p2p-dev-*` devices are type 30, as the note says.)

### `LastScan` and `LastSeen` are different units, and neither is a wall clock

`Device.Wireless.LastScan` is `CLOCK_BOOTTIME` **milliseconds**;
`AccessPoint.LastSeen` is `CLOCK_BOOTTIME` **seconds**. Confirmed against
`/proc/uptime` (93575 s uptime vs `LastScan=93549236`, `LastSeen=93536`).
`last_scan_age_s` needs `clock_gettime(CLOCK_BOOTTIME)`, not
`SystemTime::now()`, and `-1` means never scanned.

### `RequestScan` was not rate-limited

Eight back-to-back `RequestScan` calls with no delay all returned successfully.
The scope note's "NM rate-limits rescans; 'too soon' counts as success" did not
reproduce over D-Bus on 1.54 — nmcli's refusal comes from nmcli. Tolerating an
error is still correct; depending on one is not.

## What this changes in the scope note

| scope note says | measured |
| --- | --- |
| reason comes from the active connection | **wrong** — it comes from the device |
| wrong password → 7, 8 or 11 | **7 only**; 8 is an intermediate hop |
| duplicate saves produce `SSID 1` | **no** — same name, new uuid |
| profiles live in NM keyfiles | **netplan**, with the PSK in plaintext |
| `GetSecrets` gating "to be confirmed" | **confirmed**: `settings.modify.system` alone |
| rescan is rate-limited | not over D-Bus on 1.54 |
| — | netplan pins every profile to its interface |
| — | a forget rewrites all of `/etc/netplan` |
| — | `IP_CONFIG_UNAVAILABLE` is a fourth outcome needing its own message |

The three-slice plan survives. Slice 2 (the custodian write path) picks up the
`/etc/netplan` hazard; slice 3 (join) picks up the corrected reason mapping and
the new "no address" state.

## Not measured

* **Transition APs (WPA2+WPA3 mixed).** `wpa_supplicant` in AP mode will not
  offer PSK and SAE at once; that needs `hostapd`, which is not installed. The
  scope note's rule — a transition AP gets `wpa-psk` — is untested.
* **Hidden networks**, and non-UTF-8 SSIDs.
* **Enterprise**, deliberately (out of scope for v1).
* **Reboot persistence.** The device could not be rebooted; that the profile
  reaches `/etc/netplan` is good evidence, but the interface pinning above
  means a reboot test is worth doing on the installed kiosk.
* **The real write path under the custodian**, which is slice 2's job, and the
  polkit grant it needs.

## Device state afterwards

Restored and verified: the virtual radios are unloaded, the connection list is
byte-identical to the snapshot taken before any of this, `/etc/netplan` is back
to its original four files at their original sizes and mode, the temporary
polkit rule is gone and its denial re-verified, and the live link and its
ZeroTier route were never interrupted.
