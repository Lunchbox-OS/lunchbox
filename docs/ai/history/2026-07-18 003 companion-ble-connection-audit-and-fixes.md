# Companion app: BLE connection audit + fixes

> Part of the same investigation as
> `2026-07-18 001 ble-clear-outbox-on-disconnect.md` and
> `2026-07-18 002 ble-remove-bond-on-reset.md` (the server-side halves).
> All three ship on `u/albert/ble-reconnect-fixes`.
> Prior companion bug: `2026-07-09 001 ble-reconnect-poll-spin.md`.

## Prompt

After fixing the server side, audit the Android companion
(`companion-android`) for *other* sources of connection failure beyond
the already-fixed poll-spin, then fix the top cluster and validate
on-device against the live `shepherd-kiosk` daemon.

## Audit findings

Ranked; `[C]` = confirmed by reading code, `[P]` = plausible/runtime-dependent.

1. **[C] `connect()` masked an unusable encrypted link.** `drainAndDiscard`
   swallowed read failures and `connect()` only threw if the GATT connect
   itself threw. A bonded-but-one-sided link (server forgot the bond) made
   `connect()` "succeed", `refreshAll()` swallowed the ensuing RPC
   failures, and `runConnectionLoop`'s failure counter never advanced — so
   the give-up / `NeedsRepair` paths were unreachable and the loop spun
   reconnects forever while showing "Connected". Newly reachable in normal
   use because the server-side `factory_reset` fix now calls
   `remove_device`.
2. **[C] No recovery from an asymmetric bond, and no `removeBond` anywhere.**
   `isBonded()` reflects Android's bond table, which never learns the peer
   forgot the bond; `ensureBonded()` short-circuits on the stale bond, so
   even re-pair failed. Recovery required a manual "Forget" in system
   settings.
3. **[C] Pairing connection leaked; `adopt()` overwrote `connection`
   without teardown.** `pair()` held its connection in a local with no Job
   field, so backgrounding mid-pairing leaked a live link and ran BLE in
   the background.
4. **[C] Transient reconnect never cleared `pending`** → an in-flight RPC
   at drop time hung for the full 15 s `REQUEST_TIMEOUT_MS`.
5. **[C] Give-up path leaked pollers + peripheral** (`runConnectionLoop`
   returned without `conn.close()`).
6. **[C] `call()` didn't gate on `ready`** despite its doc claiming so —
   latent response-eaten race during the reconnect/drain window.
7. **[P] `isBonded()` in the reconnect `catch` could throw
   `SecurityException`** (revoked `BLUETOOTH_CONNECT`) and escape the loop.
8. **[C] Dead backoff entry** — `[1s,2s,5s]` but the loop gave up at the
   3rd failure, so 5 s never fired.
9. **[C, minor] `refreshAll` swallowed volume/brightness failures.**
10. **[latent] Reconnect trusts the stored MAC forever**, no re-scan.

## Fixes shipped (the top cluster, #1–#8)

`companion-android/.../ble/ShepherdConnection.kt`:
- `LinkUnauthenticatedException` / `ConnectionDroppedException`.
- `connect(probeEncryptedLink)` — the drain now propagates read failures;
  on reconnect a failure becomes `LinkUnauthenticatedException`. **On the
  initial pairing flow it must be `false`** (see hardware bug #2 below).
- `failPending()` invoked from the state collector on any disconnect (#4).
- `close()` is idempotent (`closed` flag) (#5).

`companion-android/.../ble/BondManager.kt`:
- `removeBond()` via the hidden `BluetoothDevice.removeBond` reflection,
  best-effort, to clear a one-sided OS bond so re-pair works (#2).

`companion-android/.../ui/ShepherdViewModel.kt`:
- `runConnectionLoop` reworked: distinct handling for a vanished OS bond,
  a `LinkUnauthenticatedException`, and the give-up path; guarded
  `isBonded` (#7); all three retry values used, give up at
  `failures > backoffs.size` (#8); `releaseConnection()` closes on give-up
  (#5).
- **Scan-probe recovery** — `deviceIsReachable()` briefly scans on give-up;
  bonded + still advertising ⟹ one-sided bond ⟹ `removeBond()` +
  `NeedsRepair` (this is the detection that actually fires — see #1 below).
- `pairingJob` tracked so `teardown()`/`onBackground()` cancel it;
  `onForeground` guards on it; `adopt()` defensively drops any raced
  connection (#3).
- `factoryReset()` now also `removeBond`s and tolerates the reset
  disconnecting us mid-RPC.

Not done (lower priority): #6 (`call()` ready-gate — fixed the doc intent
is still pending), #9, #10.

## On-device validation (Pixel 10a ↔ live `shepherd-kiosk` daemon)

Booted the daemon headless as `shepherd-kiosk`
(`shepherd dev headless --user shepherd-kiosk --config config.example.toml`
— the box's own config has a `type = "android"` activity this branch's
build can't parse yet, so the example config was used; XDG paths still
resolved to the real `shepherd-kiosk` bond/admin record). adb hot-swap
kept the bond.

Passed:
- **Happy path** — `connect: ready`, `service_state` `id=1 ok=true`.
- **No poll-spin regression** on BT toggle — 17 log lines / 1 read-failure
  (vs. the old 5.4 M).
- **Server-side disconnect-clear** — `BLE peer disconnected; clearing
  transport session state` fired on hardware.
- **Asymmetric-bond recovery** — removed the server bond, watched the phone
  detect it and clear its own bond (`Bonded devices: 0`), surfacing
  **"Bond lost — re-pair needed"**. The hidden `removeBond` reflection is
  **not blocked on this Pixel**.

### Two bugs the hardware test caught (fixed here)

1. **Detection point was wrong.** The one-sided bond fails link encryption
   *inside* `connect()` with a generic `com.juul.kable.NotConnectedException:
   Disconnect detected` (Android logs "Key missing" → link drop), which is
   indistinguishable by type from an out-of-range failure. The original
   drain-based `LinkUnauthenticatedException` never ran. Replaced the
   detection with the **scan-probe** on give-up (bonded + advertising ⟹
   stale bond). The drain-based path is kept for stacks that connect at
   GATT level before encrypting.
2. **`connect()` liveness broke pairing.** During `pair()`, `connect()`
   ran the encrypted drain *before* the bond exists; the read triggers
   Android auto-pairing but then throws → `LinkUnauthenticatedException` →
   "Pairing failed". Fixed with `connect(probeEncryptedLink = false)` in
   `pair()` (pre-bond drain failures are expected and swallowed).

### Note on manual pairing / headless

Passkey Entry (this controller is BT 4.0/Legacy) requires typing the
6-digit code the server displays. Headless, that code only exists in the
daemon log / the pairing-display overlay process args, and it expires in
60 s — so re-pairing over adb was flaky and is best done with the real
session's display up. The validation left the phone unbonded and the
server's `admin.toml` moved aside (Unclaimed) for a clean manual re-pair.

## Files
- `companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/ble/ShepherdConnection.kt`
- `companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/ble/BondManager.kt`
- `companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/ui/ShepherdViewModel.kt`

`:app:testDebugUnitTest` passes. The connection/pairing lifecycle needs a
real phone + adapter and stays a manual smoke test (exercised above).
