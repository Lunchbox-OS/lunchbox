# End-to-end validation of the network status page (#182 / PR #187)

**Prompt:** "#187 is checked out. Do the end-to-end validation, including BLE"

**PR:** <https://git.armeafamily.com/albert/shepherd-launcher/pulls/187>
**Issue:** <https://git.armeafamily.com/albert/shepherd-launcher/issues/182>

A validation pass over the branch as it stands, exercising the paths no unit
test reaches: the RPC over both transports against a live daemon, the page in a
real browser, the screen on a real phone over a real BLE link, and the failure
mode the feature exists to make visible.

## What the box looked like this time

The implementation note describes a device on WiFi (`wlx28187845b61d`,
`192.168.0.139`) with ZeroTier. That hardware is gone: the USB WiFi adapter is
no longer attached to this guest (`iw dev` is empty, `rfkill` lists only the two
Bluetooth radios), and ZeroTier is not up. The box is now:

```
enp1s0    Ethernet  192.168.122.130/24  gw/dns 192.168.122.1
lxcbr0    Bridge    10.0.3.1/24 + fc42:…::1/64   (link-down)
docker0   Bridge    172.17.0.1/16                (link-down)
lo        Loopback  127.0.0.1/8, ::1/128
B8:F4:A4:E5:20:F1  NM `bt` device, down, no address
```

That is a *different* shape from the one the feature was built against, which
made it a better test of the derivation than a repeat of the original would
have been — and it means **the WiFi path (SSID, signal, band) was not
exercised**. `wifi` is `null` on every interface here. The D-Bus property names
for `AccessPoint`/`Ssid`/`Strength` are still only as verified as they were in
the original session.

## What was verified

| Check | Result |
| --- | --- |
| `cargo test --workspace --all-targets` | 1238 passed, 0 failed, 24 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `shepherd config validate` on the example + two fixtures | passes |
| webui typecheck / check:boundary / check:coverage / build | clean |
| webui tests | 88 passed |
| `:app:assembleDebug` + `:app:testDebugUnitTest` | BUILD SUCCESSFUL |
| `reads_this_device_over_dbus` (`--ignored`, real system bus) | passes |
| `network_status` over HTTP (`POST /api/v1/rpc`) | matches `ip`/`nmcli` ground truth |
| `network_status` over the local IPC socket | same payload |
| `network_status` over BLE from the phone | `method=network_status` in the daemon log |
| Web UI Network page in Firefox (Marionette) | renders; copy button writes the real clipboard |
| Companion Network screen | renders; copy writes the real Android clipboard |
| Failed listener → both UIs + diagnostic | see below |
| BLE: re-pair after factory reset, first claim, reconnect | see below |

Ground truth agreed with the payload on every field: addresses, prefixes,
gateway, DNS, `source: network_manager`, `connectivity: full`. `reachable` was
true for `enp1s0` only — the two container bridges and loopback were correctly
excluded even though `docker0`/`lxcbr0` carry routable-looking RFC1918
addresses, and `management_urls` offered exactly one URL
(`http://192.168.122.130:8080`) from a `0.0.0.0` bind, with the link-local
`fe80::…` listed but never offered.

## The failure this feature exists for

Booted with `bind = "10.99.99.99"` (an address this device does not have) and
`bind_retry_seconds = 20`, so the listener gives up:

- `management_api` became `{state: "failed", addr: "10.99.99.99:8080", error:
  "Failed to bind management API to 10.99.99.99:8080 within 20s"}` and
  `management_urls` went empty — no address that would refuse the connection is
  ever offered.
- `management_api_unavailable` was raised as a **warning** on the service, with
  the remedy text, and rendered on the companion's Health screen.
- The companion's Network screen replaced the URL with "Configured for
  10.99.99.99:8080 and not serving: …" in the error colour.

All of that was read **over BLE, with the HTTP API dead** — which is the
argument for the feature, demonstrated rather than asserted.

## BLE

Ran the full `companion-pairing` loop against the Realtek radio
(`adapter = "8C:68:8B:41:02:DC"` pinned in the boot config; BlueZ lists the
Qualcomm one first and it is the known-bad discovery radio).

- **Reconnect after a daemon restart** — bond intact, app reattached and
  rendered live state, `dispatch: response id=… matched=true` throughout.
- **Factory reset** — `touch dev-runtime/data/.factory-reset-ble` + restart:
  daemon logged `Removed BlueZ bond`, dropped `admin.toml`, and the app showed
  "Bond lost — re-pair needed" with `Re-pair` leading back to the scan list.
- **First pairing from unclaimed** — Numeric Comparison `168891` matched on
  both sides, phone bond `LE:Y` `EncryptionStatus{keySize=16`, `claim` RPC
  recorded, `admin.toml` rewritten.
- **Reconnect after an app restart** — clean, every response matched.
- The Network screen worked on the fresh claim and after the reconnect.

## Fixed here: the companion did not show the connectivity checks

The PR says "Both UIs show, in this order: … the connectivity checks, then the
interfaces". The web page did, via the new `getServiceState()`;
`NetworkScreen.kt` rendered only the summary chip ("Online") from
`NetworkStatusView.connectivity` and never the three per-target rows.

The phone already held them: `DeviceUiState.snapshot.internetStatus` is pushed
on connect and refreshed on every `internet_status_changed` event, so the fix
is a `ChecksCard` reading `state.snapshot` — no new RPC, no new plumbing, and
crucially *not* a copy onto `NetworkStatusView`, which would give one fact two
sources that drift apart between polls. Same card title, same explanatory
sentence, same Reachable/Unreachable wording and colours as the web page, in
the same position between the web-interface card and the interface list.

Verified on the phone against a live daemon: all three targets `Reachable`; a
config with `check = "tcp://127.0.0.1:9"` renders that row `Unreachable` in the
error colour while the other two stay green; and the card follows a pushed
snapshot — a config reload that changed the target updated the row's text with
the screen still open and untouched.

## Still worth a look

1. **A MAC address does reach the UI.** The PR says "No MAC addresses or
   BSSIDs. They are the fingerprintable part". True of the *fields* — but
   NetworkManager names its Bluetooth device by its address, so this device's
   interface list contains a row literally called `B8:F4:A4:E5:20:F1`, and both
   UIs render it under "other interfaces". It is a down, address-less `bt`
   device: no use to anybody looking for a way in, and the one MAC on the page.
2. **`[service.internet].check` is not re-read on a config reload** — found
   while exercising the new card, pre-existing and unrelated to #182.
   `InternetMonitor::from_policy` runs once at startup and its `targets` vector
   is moved into a spawned task that nothing rebuilds; `Service::run` never
   hands the reload path a way to reach it. Meanwhile
   `CoreEngine::internet_status_views()` lists targets from the *live* policy
   and defaults a target it has never seen to `false`. So after a reload that
   changes the service check:

   - the new target is reported unreachable **forever** — the probe is still
     hitting the old address, and nothing will ever write a result under the
     new key;
   - every entry with `internet.required` gated on it is held back
     permanently;
   - the old target keeps being polled, unasked.

   Reproduced: boot with `check = "tcp://127.0.0.1:9"` (correctly false), edit
   the file to a URL that `curl` answers `204` on — daemon logs `Config
   reloaded`, the snapshot's target string changes, and `available` stays
   `false` across four poll intervals. A fresh boot on that same file reports
   `true` immediately. Reloading between two targets that were *both* probed at
   boot looks fine, which is why this has not been noticed: the map still has
   an entry under the new key.

## Two driving gotchas, now written down

- **Snoozing a notification can block every later pairing.** The
  `companion-pairing` skill says to snooze noisy notifications before a run.
  The OS pairing prompt is itself a `com.android.settings` notification, and it
  reuses one key — so snoozing that key (easy to do when a stale pairing
  notification is the noise) silently suppresses the consent prompt for the
  whole snooze window. Three pairings failed here with "Pairing failed — Cannot
  connect peripheral that has been cancelled" (Kable, after the phone's 30 s
  `SMP_RSP_TIMEOUT`), while logcat cheerfully said
  `BluetoothPairingService: Show pairing notification`. Added to the skill.
- **`dev headless` immediately after `dev stop` can hang shepherdd at
  startup.** Twice out of four boots, the daemon stopped after
  `Activities will be launched into a cgroup of their own` (the last line of
  `shepherd_host_linux::process::init`) and never reached the volume-controller
  line or the compositor, so the harness reported "shepherdd did not connect to
  the compositor within 30s" with a fully-started sway behind it. Killing the
  survivors, removing `session.env` and booting again fixed it both times.
  Unrelated to this branch — it reproduced on a config with no #182 changes
  in play — but it costs a boot every time. Added to the `headless-dev` skill.
