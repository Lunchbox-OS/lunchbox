# shepherd-companion-android: Implementation notes (#65)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/65>
> Spec implemented: `2026-06-21 001 ble-companion-android-spec.md`

## Prompt

> Implement the Android BLE management app. Per the spec, use kable for
> BLE. You have passwordless sudo to install the Android SDK, just add a
> dedicated `deps` install step that performs this.

This is the build-out of the companion app the spec describes, living in
this repo at `companion-android/` (the spec listed it as a separate
project, but the owner asked to build it here with a dedicated deps
step).

## What landed

### Dependency tooling

- `scripts/deps/android.pkgs` — JDK 21 + `unzip` (apt).
- `scripts/lib/deps.sh` — new `android` package set and
  `install_android_sdk()` that downloads the command-line tools and uses
  `sdkmanager` to install `platform-tools`, `platforms;android-35`, and
  `build-tools;35.0.0` into `/opt/android-sdk`. Wired into
  `deps install/check/print android`. The set is standalone (not folded
  into `dev`) because it pulls a large SDK.
  - Gotcha fixed during bring-up: `yes | sdkmanager --licenses` makes
    `yes` exit 141 (SIGPIPE), which under `set -o pipefail` aborted the
    whole script right after "Accepting licenses". The license pipeline
    now toggles `pipefail` off locally.

### The app (`companion-android/`)

Kotlin + Jetpack Compose (Material 3), single activity, min SDK 31 /
compile+target 35. Gradle (Kotlin DSL) with a version catalog and a
committed wrapper (Gradle 8.10.2, AGP 8.7.3, Kotlin 2.0.21).

- **BLE**: **Kable** (`com.juul.kable:kable-core`), per the instruction
  to use kable rather than the spec's Nordic suggestion. Kable handles
  GATT; bonding (Numeric Comparison) is driven via the raw Android
  `BluetoothDevice.createBond` + `ACTION_BOND_STATE_CHANGED` since Kable
  doesn't manage bonds.
- **Wire**: faithful mirror of `shepherd-ble`/`shepherd-api` —
  `u16`-LE length-prefix framing, JSON-RPC envelope, the full error-code
  enum, every RPC method, and the event stream. snake_case is handled by
  `JsonNamingStrategy.SnakeCase` with explicit `@SerialName`s for enum
  constants and the externally-tagged `LaunchOutcome`.
- **Reconnect**: `ShepherdRecord.androidIdentifier` (the scan MAC) is the
  handle for `Peripheral(identifier)` so reconnects skip scanning. It's a
  client-side field, never sent on the wire.
- **Persistence**: `EncryptedSharedPreferences`; backup/transfer
  extraction disabled so the keystore-wrapped HTTP token can't leave the
  device.
- **UI**: pairing (scan + Numeric Comparison + claim), home (device
  chips + entry list), entry detail (launch / live countdown / extend /
  stop / today's override editor / 7-day usage bar chart), device
  controls (volume / brightness / reload / logout), settings (per-device
  factory reset, forget-all, nickname, version, issue link).
- **Constraints**: no telemetry/analytics, no background BLE (link bound
  to the foreground lifecycle), GPL-compatible deps only.

### Tests

- `FramingTest` — encode/chunk/reassembly, including frames split across
  notifications and the oversize-length guard.
- `WireTest` — decodes the exact sample payloads from the spec (entries,
  reason codes, both `LaunchOutcome` branches, device info, admin record,
  `state_changed`/`session_ended` events, error responses, volume).

The protocol/transport/persistence layers are unit-tested and hardware
independent; the live BLE path needs a real device per the spec's §10
smoke test.

## Open questions resolved

See `companion-android/README.md` → "Decisions" for the per-question
calls (package id, kable, EncryptedSharedPreferences, chip selector,
hand-rolled chart, 1/2/5s backoff, the `androidIdentifier` reconnect
field).
