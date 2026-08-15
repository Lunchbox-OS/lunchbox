# shepherd-companion-android

The Android companion app for managing one or more shepherd-launcher
devices over Bluetooth LE. It is the primary admin interface: it pairs
with a device using Numeric Comparison, claims it (TOFU single-admin),
and then drives the full management RPC catalog over the bonded GATT
link.

Implements the spec at
[`docs/ai/history/2026-06-21 001 ble-companion-android-spec.md`](../docs/ai/history/2026-06-21%20001%20ble-companion-android-spec.md).
The wire protocol mirrors `crates/shepherd-ble` and `crates/shepherd-api`
in the parent repo.

## Install

Released builds are published to this project's F-Droid repository (which keeps
the app updated) and attached to each release as an APK. See
[Installing the Android apps](../docs/INSTALL.md#installing-the-android-apps).
App listing metadata lives in [`dist/fdroid/`](../dist/fdroid/README.md).

## Build

The Android SDK and a JDK are installed by the parent repo's tooling:

```sh
./scripts/shepherd deps install android   # JDK 21 + Android SDK -> /opt/android-sdk
```

Then, from this directory:

```sh
export ANDROID_SDK_ROOT=/opt/android-sdk     # or set sdk.dir in local.properties
./gradlew :app:assembleDebug                 # build the debug APK
./gradlew :app:testDebugUnitTest             # run unit tests
```

The debug APK lands at `app/build/outputs/apk/debug/app-debug.apk` and is
sideload-friendly (`adb install`).

## Architecture

| Layer | Where | Notes |
|---|---|---|
| Wire (UUIDs, framing, RPC envelope, error codes) | `ble/Protocol.kt`, `ble/Framing.kt`, `ble/Rpc.kt` | `u16`-LE length-prefix framing; JSON-RPC over GATT. |
| Transport | `ble/ShepherdConnection.kt` | Wraps a Kable `Peripheral`: chunked writes, frame reassembly, request/response correlation by `id`, hot event stream. |
| Scanning / bonding | `ble/ShepherdScanner.kt`, `ble/BondManager.kt` | Kable for GATT; the raw Android `createBond` + bond-state broadcast for Numeric Comparison. |
| Domain types | `domain/Models.kt` | Kotlin mirrors of `shepherd-api`; snake_case via `JsonNamingStrategy`. |
| Typed client | `domain/ManagementClient.kt` | Mirrors the device's `ManagementService` trait. |
| Persistence | `persistence/AdminRecordStore.kt`, `domain/ShepherdRepository.kt` | Per-device record + secret HTTP token, encrypted at rest. |
| UI | `ui/**` | Jetpack Compose + Material 3, single activity, one shared `ShepherdViewModel`. |

### Decisions (spec §11 open questions)

1. **Package / name** — `com.armeafamily.shepherd.companion` / "Shepherd Companion".
2. **BLE library** — **Kable** (coroutine-native), not Nordic, per the
   project owner's instruction. Apache-2.0, GPL-compatible. Bonding
   itself is OS-driven (`BluetoothDevice.createBond`); Kable handles
   GATT only.
3. **Persistence** — `EncryptedSharedPreferences` (AES-256 GCM values via
   an AndroidKeystore master key). Backup/transfer extraction is disabled
   so the keystore-wrapped token blob never leaves the device.
4. **Multi-device** — horizontal `FilterChip` selector on Home.
5. **Charts** — a small hand-rolled Compose bar chart; no chart library.
6. **Reconnect backoff** — 1s → 2s → 5s, then the link is surfaced as
   dropped. A vanished bond short-circuits to a "re-pair needed" state.
7. **Reconnect handle** — `ShepherdRecord.androidIdentifier` stores the
   scan MAC so `Peripheral(identifier)` reconnects without re-scanning;
   it is a client-side field, never sent on the wire.

### Constraints honoured

- **No telemetry / analytics / phone-home.** The only egress is the
  bonded BLE link (and an `ACTION_VIEW` to the issue tracker the user
  taps explicitly). No Firebase, no crash reporter, no ads.
- **No background BLE.** Connections live only while the UI is in the
  foreground; `onStop` tears the link down. No foreground service, no
  background scan callbacks.
- **GPL-3.0-compatible dependencies only** (Kable/AndroidX/Compose/
  kotlinx are Apache-2.0).

## Verification

Unit tests cover the load-bearing, hardware-independent layers: the
framing codec (`FramingTest`) and the wire decode of every sampled
payload in the spec (`WireTest`).

Everything that has actually broken in production lives below that line —
implicit bonding, link encryption, and the connect-time drain are all in
the Android and BlueZ stacks, where no unit test reaches. Verify those
against real hardware with the **`companion-pairing` skill**
(<.claude/skills/companion-pairing/SKILL.md>), which drives a
USB-attached phone through pair → claim → reconnect → re-pair against the
headless dev session and records what to check on both sides. Treat a
pass through it as required for changes to `ShepherdConnection` or
`BondManager`; the operator-facing version of the same flow is "Pairing
your phone with a device" in <docs/INSTALL.md>.
