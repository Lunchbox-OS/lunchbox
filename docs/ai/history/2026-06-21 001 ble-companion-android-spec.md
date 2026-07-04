# shepherd-companion-android: Implementation Specification

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/65>
> Companion: see `2026-06-20 002 ble-management.md` for the design that
> the device side implements (Numeric Comparison pairing + TOFU
> single-admin claim model + unified HTTP/BLE bearer-token identity).

## 0. Purpose and scope

`shepherd-companion-android` is the Android phone app a parent uses to
manage one or more shepherd-launcher devices over Bluetooth LE. It is
the *primary* admin interface — the device's HTTP API still exists, but
it is now opt-in/secondary because it needs IP autodiscovery or a
known static IP, neither of which the BLE path requires.

### In scope (this document)

- An Android app, distributed as an APK (sideload-friendly; Play Store
  release is out of scope).
- BLE discovery, Numeric Comparison pairing, the full JSON-RPC catalog
  on the GATT service exposed by `shepherd-ble`, and the live
  `Event` stream from the Events characteristic.
- A simple UI that exposes every operation the device side already
  supports: list entries, launch / stop / extend sessions, edit daily
  overrides, view usage analytics, adjust volume and brightness, reload
  config, factory-reset the BLE bond.
- Multi-device support: one phone, many shepherd devices (e.g. one per
  household location).
- Local persistence of the per-device admin record so the phone
  reconnects without prompting and so the issued HTTP bearer token is
  available if the user is ever on the same LAN.

### Out of scope (this document)

- iOS app. Permanently out of scope for v1; the protocol is iOS-
  capable but no iOS work is funded.
- Play Store distribution, in-app purchases, telemetry, crash
  reporters.
- Multi-admin / approve-new-device flows. The device side is TOFU
  single-admin in v1; the phone only ever sees its own bond.
- Background BLE connections (foreground services, "always connected"
  modes). v1 connects on demand when the user opens the screen for a
  device; backgrounding the app disconnects.
- Anything related to the HTTP transport beyond storing the bearer
  token. The phone has the token in case the user later builds a
  desktop client; the app itself only ever talks BLE.

### Non-negotiable constraints (from the parent project)

These come from shepherd-launcher's project posture; the implementing
agent must respect them.

1. **No telemetry. No analytics. No phone-home. No PII collection.**
   This rules out Firebase Analytics, Crashlytics, Google Mobile Ads,
   etc. The app makes no network calls except over the bonded BLE
   link.
2. **No DRM circumvention.** The app never speaks Steam, YouTube,
   etc. directly — it only sends RPCs to the device, which already
   enforces these limits.
3. **No background data collection.** The phone never records usage,
   location, or anything else when the app isn't open.
4. **License-compatible with the parent project (GPL-3.0).** All
   third-party libs must be GPL-3.0-compatible.

## 1. Project setup

### Toolchain

- **Language**: Kotlin (current stable).
- **UI**: Jetpack Compose (current stable Material 3).
- **Min SDK**: API 31 (Android 12). BLE permissions were reshuffled in
  Android 12; targeting 31+ avoids the legacy
  `ACCESS_FINE_LOCATION`-for-BLE-scan hack.
- **Target SDK**: latest stable (one or two below "preview").
- **Build**: Gradle with the Kotlin DSL.

### Recommended dependencies

| Concern | Recommendation | Notes |
|---|---|---|
| BLE central role | **Nordic Android-BLE-Library v2** (`no.nordicsemi.android:ble`) | Hand-rolled `BluetoothGatt` is a state-machine swamp. Nordic's lib handles the queue, MTU negotiation, disconnect/reconnect, and bond-state coordination. Alternative: `Kable` (Kotlin coroutine-native) — fine if the agent prefers coroutines over Nordic's request DSL. |
| JSON | **kotlinx.serialization** | Matches the snake_case wire format with `@SerialName` overrides. |
| Persistence | **DataStore (proto or preferences)** with the per-device admin record encrypted via Jetpack Security or Android Keystore | One row per bonded shepherd device. |
| Coroutines | `kotlinx-coroutines-android` | Standard. |
| Dependency injection | Optional (`Hilt` if you want it, plain manual wiring if you don't) | The surface is small enough that DI is not required. |
| Testing | JUnit 5, MockK, Turbine for coroutine streams | Standard. |

### Module layout (suggested)

```
app/
  src/main/kotlin/com/shepherd/companion/
    MainActivity.kt                     # single activity host
    ble/
      ShepherdBleManager.kt             # Nordic BleManager subclass
      Framing.kt                        # u16 length-prefix codec
      Rpc.kt                            # RpcRequest/RpcResponse types + dispatcher
      Protocol.kt                       # UUIDs, ErrorCode, DeviceInfo, constants
    domain/
      ShepherdRecord.kt                 # persisted per-device admin record
      ShepherdRepository.kt             # multi-device list + active selection
      ManagementClient.kt               # typed wrapper around RpcClient that mirrors
                                        # the ManagementService trait methods
      Models.kt                         # Kotlin mirrors of shepherd-api types
    ui/
      pairing/                          # scan + pair + claim flow
      home/                             # device picker + entry list
      session/                          # current-session view + launch
      overrides/                        # daily-override editor
      usage/                            # screen-time charts
      device/                           # volume + brightness + config reload
      settings/                         # factory-reset, app info
    persistence/
      AdminRecordStore.kt               # encrypted DataStore wrapper
```

### Android manifest essentials

```xml
<uses-permission android:name="android.permission.BLUETOOTH_SCAN"
    android:usesPermissionFlags="neverForLocation" />
<uses-permission android:name="android.permission.BLUETOOTH_CONNECT" />
<uses-feature android:name="android.hardware.bluetooth_le" android:required="true" />
```

No `ACCESS_FINE_LOCATION` is needed when `neverForLocation` is set on
`BLUETOOTH_SCAN` and the targeted shepherd devices are filtered by
service UUID rather than scanned location-aware.

## 2. BLE protocol (the wire)

### 2.1 Service and characteristic UUIDs

All UUIDs are 128-bit; copy them verbatim.

| Symbol | UUID |
|---|---|
| `SHEPHERD_MANAGEMENT_SERVICE` | `8c0c0001-3b21-4abc-9e3f-0a9c1f2e3d40` |
| `DEVICE_INFO_CHAR`             | `8c0c0002-3b21-4abc-9e3f-0a9c1f2e3d40` |
| `REQUEST_CHAR`                 | `8c0c0003-3b21-4abc-9e3f-0a9c1f2e3d40` |
| `RESPONSE_CHAR`                | `8c0c0004-3b21-4abc-9e3f-0a9c1f2e3d40` |
| `EVENTS_CHAR`                  | `8c0c0005-3b21-4abc-9e3f-0a9c1f2e3d40` |

Characteristic properties as exposed by the server:

| Char | Properties | Encryption |
|---|---|---|
| `DeviceInfo` | read | none — readable pre-pairing |
| `Request`    | write, write-without-response | authenticated bonded link required |
| `Response`   | notify | authenticated bonded link required |
| `Events`     | notify | authenticated bonded link required |

Two implications:

1. **`DeviceInfo` can be read before any bond exists.** The companion
   app uses it during onboarding to decide whether to initiate
   pairing — see §3.
2. **The other three characteristics demand an authenticated bonded
   link.** The Android stack will normally trigger pairing
   automatically when you try to read/write/subscribe to an
   encryption-required characteristic. The app should not pre-bond as a
   separate step; the pairing flow in §3 is the recommended sequence
   anyway, but if you take a different route, expect the OS to prompt
   the user with the Numeric Comparison dialog on first
   write/subscribe.

### 2.2 Framing

The Request, Response, and Events characteristics carry *logical
frames* that are larger than a single ATT write/notify packet. The
wire format on each characteristic is:

```
+----+----+----+----+----+----+...
| length (u16 LE)  |     payload (JSON, length bytes)
+----+----+----+----+----+----+...
```

Reading and writing both fragment and reassemble on top of this:

- **Writes** (Request): the companion concatenates `[u16 LE length, JSON
  bytes]` and splits it into chunks of at most `MTU - 3` bytes each.
  Each chunk goes out as a separate `WriteRequest`. The chunks must
  arrive in order; Nordic's `BleManager` write queue does this for
  you. Use `Write Without Response` if available (faster); fall back
  to `Write Request` otherwise.
- **Notifies** (Response and Events): the server sends one or more
  notifications per logical frame. The companion buffers all
  notification payloads, then peeks the first two bytes as a u16 LE
  length, and once enough bytes are buffered, slices out a frame.
  Reset the buffer on disconnect.

**Negotiate the MTU early.** Right after `onServicesDiscovered`, call
`requestMtu(517)`. BlueZ on the device side accepts up to 517; what
you actually get back depends on the phone's controller. Use the
returned MTU − 3 as your chunk size; never assume more than 20 bytes
(the BLE 4.0 default).

**Per-connection state cap.** The server caps each logical frame at
16 KiB (`MAX_FRAME_BYTES`). Larger frames are dropped server-side
and the connection is reset. The companion never produces frames
that large in practice (typical request <500 B), so this is a safety
net rather than a constraint.

### 2.3 RPC envelope

Requests and responses are JSON, fragmented into frames. The on-the-
wire shapes are stable; treat them like a public API.

**Request** (companion → device, written to `REQUEST_CHAR`):

```json
{
  "id": 7,
  "method": "list_entries",
  "params": { "at": null }
}
```

- `id`: u32 client-chosen correlation id. Increment per request. The
  device echoes it on the matching response. Requests can be
  in-flight concurrently; responses may arrive out of order.
- `method`: one of the strings in §4.
- `params`: method-specific JSON object. May be omitted (treated as
  `null`) for methods that take no parameters.

**Response** (device → companion, notified on `RESPONSE_CHAR`):

```json
{ "id": 7, "result": [ /* method-specific */ ] }
```

or, on error:

```json
{
  "id": 7,
  "error": {
    "code": "not_found",
    "message": "No entry with id 'missing-game'"
  }
}
```

Exactly one of `result` or `error` will be present per response.

### 2.4 Error codes

Wire enum, serialized as snake_case strings:

| Code | When | UI hint |
|---|---|---|
| `parse_error` | Frame is not valid JSON. Generally indicates a wire bug. | "Communication error; try again." |
| `invalid_request` | Reserved; not currently used. | — |
| `method_not_found` | Server doesn't know the method name. App is older than the device. | "This device has features your app doesn't yet support." |
| `invalid_params` | Param JSON didn't deserialize into the method's expected shape. | "Bad request" — surface the `message`. |
| `not_claimed` | App tried an admin RPC before the device was claimed. | Should not happen if §3 flow is followed; redirect to pairing. |
| `already_claimed` | Claim RPC sent to a device that already has an admin. | "Another phone is already paired with this device." Offer factory-reset instructions. |
| `permission_denied` | Authenticated peer is not the bonded admin. | "This phone is not authorised to control this device." |
| `not_found` | Entity (entry, session, override) does not exist. | Method-specific copy. |
| `bad_request` | Validation error (e.g. `from > to`). | Surface the `message`. |
| `forbidden` | Operation disallowed by policy (e.g. volume change disabled). | "This action is not allowed by the policy." |
| `conflict` | Reserved; not currently used. | — |
| `unprocessable` | Used by `reload_config` when the config file fails validation. | Surface the `message`. |
| `internal` | Server-side error. The `message` is often informative. | "Device error" — show the `message`. |

## 3. Pairing flow (Numeric Comparison)

The device side advertises its `SHEPHERD_MANAGEMENT_SERVICE_UUID` and
its local name (default `"shepherd"`; admin can override per device
in config).

Step by step:

1. **Permission gate.** Request `BLUETOOTH_SCAN` and `BLUETOOTH_CONNECT`
   if not already granted. If denied, the app should explain *why* —
   "Your phone needs to talk to the shepherd device over Bluetooth."
2. **Scan.** Use the system scanner with a filter on
   `SHEPHERD_MANAGEMENT_SERVICE_UUID`. Show each result as
   `<local name> · RSSI · MAC` so the user can disambiguate two
   identical-named devices by signal strength.
3. **Read DeviceInfo.** Tap on a result → connect → discover services
   → read `DEVICE_INFO_CHAR`. The payload is:

   ```json
   {
     "protocol_version": 1,
     "firmware_version": "0.1.0",
     "claim_state": "unclaimed",   // or "claimed"
     "device_name": "shepherd"
   }
   ```

   - If `protocol_version` ≠ 1, the app should refuse to proceed and
     prompt the user to update the app.
   - If `claim_state == "claimed"`, this device is already paired with
     someone. Offer two options: "Try to reconnect" (only succeeds if
     this phone is the bonded admin — falls through to the §3.5
     reconnect flow) and "Take over" (instructs the user to physically
     trigger the factory-reset sentinel on the device, then re-scan).
   - If `claim_state == "unclaimed"`, proceed.
4. **Initiate bond.** Call `device.createBond()`. This kicks the OS
   pairing flow. With IO capability `DisplayYesNo` on the device side
   the OS will display its Numeric Comparison dialog showing a 6-digit
   passkey.
5. **Verify against TV.** The device renders the same 6 digits as a
   full-screen overlay (`shepherd-pairing-display`). Tell the user:
   *"If the number on your TV matches the number shown here, tap PAIR.
   Otherwise tap CANCEL."* Do not auto-confirm in the app — Numeric
   Comparison's security depends on the user actually comparing.
6. **Wait for bond.** Listen on `ACTION_BOND_STATE_CHANGED`. On
   `BOND_BONDED`, proceed. On `BOND_NONE` (user cancelled or
   timeout), surface the failure and let them retry.
7. **Connect and negotiate MTU.** Establish GATT connection,
   `requestMtu(517)`, `discoverServices`. Use the returned MTU − 3 as
   your write/notify chunk size.
8. **Subscribe.** Enable notifications on `RESPONSE_CHAR` and
   `EVENTS_CHAR`.
9. **Send `claim`.** With `device_name = "${Build.MODEL}"` (or a name
   the user chose):

   ```json
   {"id": 1, "method": "claim", "params": {"device_name": "Pixel 8"}}
   ```

   Expected success result (an `AdminRecord` mirror):

   ```json
   {
     "id": 1,
     "result": {
       "identity_address": "AA:BB:CC:DD:EE:FF",
       "address_type": "public",
       "device_name": "Pixel 8",
       "bonded_at": "2026-06-20T22:30:00-04:00",
       "http_token": "9b2e..." ,
       "role": "admin"
     }
   }
   ```
10. **Persist locally.** Store the returned `AdminRecord` under the
    phone's record for this device. The `http_token` is the bearer
    token the device's HTTP API will accept; treat it as secret (store
    encrypted, never log).
11. **Done.** The phone now reconnects on demand without re-pairing.

### 3.5 Reconnect (subsequent sessions)

Significantly simpler:

1. Look up the persisted admin record for the chosen device by its
   `identity_address`.
2. Get the `BluetoothDevice` from `BluetoothAdapter.getRemoteDevice`.
3. Connect, discover services, request MTU, subscribe.
4. Send RPCs. The OS reuses the bonded LTK; the link comes up
   encrypted automatically.

If the connection fails with `BluetoothGatt.GATT_INSUFFICIENT_AUTHENTICATION`
or the OS reports the bond is gone, the device has been factory-reset
(or someone else paired in the meantime). Surface this clearly — the
phone needs to re-pair — and do not auto-attempt to claim, since the
device might now belong to someone else.

## 4. RPC methods

Names are stable; payloads mirror the Rust types in
`crates/shepherd-api/src/types.rs` and `events.rs` (see References).
All Rust struct fields are JSON-serialised as their lowercase Rust
identifier; all enums use `#[serde(rename_all = "snake_case")]` —
i.e., variant names lowercase with `_` separators. `Duration` is
serialised by `serde` as `{"secs": u64, "nanos": u32}`; treat it as
"seconds, integer" for UI purposes and ignore `nanos`.

### 4.1 Claim-flow methods

These bypass the per-RPC admin check.

#### `claim`

Take ownership of an unclaimed device.

- Params: `{ "device_name": "Pixel 8" }`
- Result: `AdminRecord` (see §3 step 9).
- Errors:
  - `invalid_params` — missing `device_name`.
  - `already_claimed` — device already has an admin.
  - `internal` — disk write of the admin record failed.

#### `factory_reset`

Wipe the admin record and bond. Required call before re-pairing.

- Params: `{}` (or omitted).
- Result: `null`.
- Errors:
  - `permission_denied` — caller is not the current admin.
  - `internal` — disk write of the admin record cleanup failed.

### 4.2 Health / state

#### `health`

- Params: `{}`.
- Result:
  ```json
  {
    "live": true, "ready": true, "policy_loaded": true,
    "host_adapter_ok": true, "store_ok": true
  }
  ```

#### `service_state`

Full snapshot. Use as the initial population on the home screen,
then keep up to date by listening to `state_changed` events (§5).

- Params: `{}`.
- Result: `ServiceStateSnapshot` —
  ```json
  {
    "api_version": 1,
    "policy_loaded": true,
    "current_session": null,
    "entry_count": 14,
    "entries": [ /* EntryView[] */ ],
    "internet_status": [ /* InternetStatusView[] */ ]
  }
  ```

### 4.3 Entries

#### `list_entries`

- Params: `{ "at": "2026-06-21T18:00:00-04:00" }` — optional `at`
  evaluates availability at the given time (so the UI can preview
  what'll be available later). Omit for "now".
- Result: `EntryView[]`:
  ```json
  [
    {
      "entry_id": "steam-celeste",
      "label": "Celeste",
      "icon_ref": "steam://celeste/icon",
      "kind_tag": "steam",
      "enabled": true,
      "reasons": [],
      "max_run_if_started_now": { "secs": 1800, "nanos": 0 }
    }
  ]
  ```
  `reasons` is an array of `ReasonCode` (see §4.10).

#### `get_entry`

- Params: `{ "id": "steam-celeste", "at": null }`.
- Result: single `EntryView`.
- Errors: `not_found`.

### 4.4 Sessions

#### `current_session`

- Params: `{}`.
- Result: `SessionInfo | null`:
  ```json
  {
    "session_id": "uuid…",
    "entry_id": "steam-celeste",
    "label": "Celeste",
    "state": "running",
    "started_at": "2026-06-21T18:05:00-04:00",
    "deadline": "2026-06-21T18:35:00-04:00",
    "time_remaining": { "secs": 1800, "nanos": 0 },
    "warnings_issued": []
  }
  ```
  `state` is one of `launching | running | warned | expiring | ended`.
  `deadline` and `time_remaining` are `null` when the session has no
  time limit.

#### `launch`

- Params: `{ "id": "steam-celeste" }`.
- Result: tagged enum:
  ```json
  { "Approved": { "session_id": "uuid…", "deadline": "2026-…" } }
  ```
  or
  ```json
  { "Denied": { "reasons": [ /* ReasonCode[] */ ] } }
  ```
  *(See §4.10 for `ReasonCode`.)*
- Errors:
  - `not_found` — entry id unknown.
  - `internal` — spawn failed; message has details.

#### `stop_current`

- Params: `{ "mode": "graceful" }` — `"graceful"` (default) or
  `"force"`.
- Result: `null`.
- Errors: `not_found` — no active session.

#### `extend_current`

Add or remove time from the active session. Negative seconds shorten.

- Params: `{ "seconds": 600 }`.
- Result: `{ "new_deadline": "2026-06-21T18:45:00-04:00" }` (or
  `{ "new_deadline": null }` for unlimited sessions that weren't
  bounded).
- Errors: `not_found` — no active session.

### 4.5 Daily overrides

Parent controls that modify a single entry on a single date. Default
date is "today" if omitted.

#### `list_overrides`

- Params: `{ "date": "2026-06-21" }` (optional).
- Result: `DailyOverride[]`:
  ```json
  [
    {
      "entry_id": "steam-celeste",
      "date": "2026-06-21",
      "availability": false,
      "quota_delta_seconds": -1800,
      "created_at": "2026-06-21T08:00:00-04:00",
      "updated_at": "2026-06-21T08:00:00-04:00"
    }
  ]
  ```
  `availability`: `true` = force-allow (overrides time window),
  `false` = force-block, `null` = no change.
  `quota_delta_seconds`: signed adjustment to today's quota.

#### `get_override`

- Params: `{ "id": "steam-celeste", "date": "2026-06-21" }`.
- Result: `DailyOverride | null`.

#### `upsert_override`

- Params:
  ```json
  {
    "id": "steam-celeste",
    "date": "2026-06-21",
    "availability": false,
    "quota_delta_seconds": null
  }
  ```
  At least one of `availability` and `quota_delta_seconds` must be
  non-null.
- Result: `DailyOverride`.
- Errors: `bad_request` — empty body.

#### `delete_override`

- Params: `{ "id": "steam-celeste", "date": "2026-06-21" }`.
- Result: `{ "deleted": true }` (or `false` when nothing was there).

### 4.6 Usage analytics

#### `usage_all`

- Params: `{ "from": "2026-06-15", "to": "2026-06-21" }`.
- Result: `UsageStat[]`, sorted by `(date, entry_id)`:
  ```json
  [
    {
      "entry_id": "steam-celeste",
      "label": "Celeste",
      "date": "2026-06-21",
      "duration_seconds": 1740
    }
  ]
  ```
- Errors: `bad_request` — `from > to`.

#### `usage_entry`

- Params: `{ "id": "steam-celeste", "from": "...", "to": "..." }`.
- Result: `UsageStat[]` (for the one entry, sorted by date).
- Errors: `not_found` — entry id unknown.

### 4.7 Volume

#### `get_volume`

- Params: `{}`.
- Result: `VolumeInfo`:
  ```json
  {
    "percent": 42,
    "muted": false,
    "available": true,
    "backend": "pipewire",
    "restrictions": {
      "max_volume": 80,
      "min_volume": null,
      "allow_mute": true,
      "allow_change": true
    }
  }
  ```

#### `set_volume`

- Params: `{ "percent": 60 }`. Server clamps to
  `restrictions.min_volume..=restrictions.max_volume`.
- Result: updated `VolumeInfo`.
- Errors: `forbidden` — `allow_change: false`.

#### `set_mute`

- Params: `{ "muted": true }`.
- Result: updated `VolumeInfo`.
- Errors: `forbidden` — `allow_mute: false`.

### 4.8 Brightness

#### `get_brightness`

- Params: `{}`.
- Result: `BrightnessInfo`:
  ```json
  {
    "percent": 75,
    "available": true,
    "backend": "sysfs",
    "device": "intel_backlight",
    "restrictions": {
      "max_brightness": null,
      "min_brightness": 10,
      "allow_change": true
    }
  }
  ```
  When `available: false`, hide the slider in the UI rather than
  treating it as an error.

#### `set_brightness`

- Params: `{ "percent": 60 }`. Server clamps.
- Result: updated `BrightnessInfo`.
- Errors: `forbidden` — `allow_change: false`.

### 4.9 Misc

#### `reload_config`

Hot-reload the device's TOML configuration. Useful when the parent
edits the config file directly.

- Params: `{}`.
- Result: `{ "entry_count": 14 }`.
- Errors: `unprocessable` — config failed validation; `message`
  holds the parse/validation error.

#### `logout`

Asks the device to terminate the user's desktop session (kicks back
to the launcher / Sway logout). Doesn't affect the BLE link.

- Params: `{}`.
- Result: `null`.

#### `list_windows`

Debug introspection on the host compositor (Sway).

- Params: `{}`.
- Result: `WindowInfo[]`. Most apps can ignore this; useful for a
  hidden "developer" panel.

#### `act_on_window`

- Params: `{ "id": 42, "action": "close" }` — action one of
  `close | hide | show`.
- Result: `null`.

### 4.10 `ReasonCode` shapes

`ReasonCode` is a tagged enum. `code` discriminates; per-variant
fields follow.

```jsonc
{ "code": "outside_time_window",
  "next_window_start": "2026-06-21T17:00:00-04:00" /* or null */ }

{ "code": "quota_exhausted",
  "used": { "secs": 3600, "nanos": 0 },
  "quota": { "secs": 3600, "nanos": 0 } }

{ "code": "cooldown_active",
  "available_at": "2026-06-21T19:00:00-04:00" }

{ "code": "session_active",
  "entry_id": "steam-celeste",
  "remaining": { "secs": 600, "nanos": 0 } /* or null */ }

{ "code": "unsupported_kind", "kind": "steam" }

{ "code": "disabled", "reason": "manually disabled by parent" /* or null */ }

{ "code": "internet_unavailable", "check": "https://example.com" /* or null */ }

{ "code": "manually_disabled", "until": "2026-06-22" }
```

## 5. Event stream

Subscribe to `EVENTS_CHAR` notifications after connecting. Each
notification is the same framed JSON described in §2.2; the decoded
JSON is an `Event`.

**The first notification after every subscribe is always a
`state_changed` event** carrying the current `ServiceStateSnapshot`.
The companion can use that to populate its UI without having to send
a separate `service_state` RPC. (The device synthesises this initial
event in the subscribe handler — see
`crates/shepherd-ble/src/server.rs::events_characteristic`.)

Example payload of the initial frame:

```json
{
  "api_version": 1,
  "timestamp": "2026-06-21T18:05:00-04:00",
  "payload": { "type": "session_started",
               "session_id": "uuid…",
               "entry_id": "steam-celeste",
               "label": "Celeste",
               "deadline": "2026-06-21T18:35:00-04:00" }
}
```

`payload.type` discriminator + per-variant fields:

| `type` | Fields |
|---|---|
| `state_changed` | a full `ServiceStateSnapshot` is inlined alongside `type` — the snake_case fields of that struct appear at the top level of `payload`. |
| `session_started` | `session_id`, `entry_id`, `label`, `deadline` |
| `warning_issued` | `session_id`, `threshold_seconds`, `time_remaining`, `severity` (`info | warn | critical`), `message` |
| `session_expiring` | `session_id` |
| `session_ended` | `session_id`, `entry_id`, `reason` (`SessionEndReason`), `duration` |
| `policy_reloaded` | `entry_count` |
| `entry_availability_changed` | `entry_id`, `enabled` |
| `volume_changed` | `percent`, `muted` |
| `brightness_changed` | `percent` |
| `hud_scale_changed` | `factor` — phone can ignore |
| `internet_status_changed` | `target`, `available` |
| `shutdown` | (no fields) |
| `audit_entry` | `event_type`, `details` (free-form JSON) |

`SessionEndReason` is itself tagged:

```jsonc
{ "type": "expired" }
{ "type": "user_stop" }
{ "type": "admin_stop" }
{ "type": "process_exited", "exit_code": 0 /* or null */ }
{ "type": "policy_stop" }
{ "type": "service_shutdown" }
{ "type": "launch_failed", "error": "..." }
```

### Reconnection behaviour

If the BLE link drops while the app is in the foreground, attempt
silent reconnect up to ~3 times with backoff, then surface the
disconnection in the UI. On reconnect, re-subscribe to `EVENTS_CHAR`
— the device's initial `state_changed` push (see above) will
repopulate the UI automatically, so you do **not** need to issue an
explicit `service_state` RPC. The device does not replay other
events the app missed while disconnected; UI that displays
running-session state should refresh on any future event rather than
assume the snapshot is still current.

## 6. App UI

The user-facing surface is intentionally small. Each screen below
maps to a function in §4.

### 6.1 Pairing (first-run + add-device)

- Scan list (filtered to the service UUID).
- Tap → DeviceInfo read → "Pair with this device?" sheet.
- During pairing, a card with: "Compare the 6 digits on your TV with
  the number Android shows you. They must match exactly." Show the
  device's MAC address as a sanity check.
- On success → claim → home screen.

### 6.2 Home (device picker + entry list)

- Top: chip selector when more than one shepherd device is bonded.
- Body: list of entries with `label`, status badge (Enabled / Blocked
  / In session), and any present `ReasonCode` translated to a one-
  line human message (e.g. "Outside allowed hours — next 17:00").
- FAB or per-row tap → entry detail.

### 6.3 Entry detail

- Label, icon (best-effort), kind tag, current availability.
- "Launch" button — only enabled if `enabled: true`.
- If currently running: large countdown derived from
  `time_remaining`, with `+10 min` / `−10 min` (extend_current) and
  "Stop" (stop_current) buttons.
- "Daily overrides" subsection showing today's override if any, with
  Edit/Clear.
- "Usage" subsection showing the last 7 days as a tiny bar chart.

### 6.4 Daily overrides editor

- Date picker (defaults to today).
- "Allow today" toggle (sets `availability: true | false | null`).
- "Adjust quota" stepper (in 5-minute chunks; produces
  `quota_delta_seconds`).
- Save → upsert_override.
- Delete → delete_override.

### 6.5 Device controls

- Volume slider (`get_volume` → seek → `set_volume`), mute toggle,
  hidden when `restrictions.allow_change == false` or `available ==
  false`.
- Brightness slider, hidden when `available == false`.
- "Reload config" button → `reload_config`; surface the entry_count
  on success and the validation error on failure.

### 6.6 Settings

- Per-device "Factory reset (unpair)" — sends `factory_reset`, then
  deletes the local admin record on success.
- App-level "Forget all devices" — clears every persisted admin
  record (does not call `factory_reset` on each; that's a deliberate
  choice for "I lost my devices" recovery).
- App version, link to issue tracker.

## 7. Persistence

One record per bonded shepherd device:

```kotlin
@Serializable
data class ShepherdRecord(
    val identityAddress: String,    // BlueZ-resolved peer address
    val addressType: String,        // "public" | "random"
    val deviceName: String,         // the local_name we saw at scan time
    val bondedAt: Instant,
    val httpToken: String,          // bearer token also valid for HTTP API
    val role: String,               // "admin" in v1
    val nickname: String? = null    // user-set display name
)
```

Persistence requirements:

- Encrypted at rest. Either use Jetpack Security's
  `EncryptedSharedPreferences` (deprecated path but stable) or the
  current recommendation: a `DataStore<ByteArray>` whose bytes are a
  proto/JSON blob, encrypted with `AndroidKeyStore`-managed AES-GCM.
- Never log `httpToken`.
- Wipe `httpToken` on factory_reset success even if the local delete
  fails partway — the device's token is invalidated server-side, so a
  stale local copy is useless but should not linger.

## 8. Multi-shepherd support

The phone may be paired with several devices over time. The app keeps
a list of `ShepherdRecord`s (§7) and presents them as a horizontal
chip selector on the home screen. Switching between them only changes
which BLE peer the app talks to — there is no shared state across
devices.

A single phone is allowed to be the admin of multiple devices (one
record per device, distinct `identity_address` and `http_token`).

## 9. Non-negotiable constraints (recap)

- **No telemetry, no analytics, no PII collection.** The app must not
  initiate any network connection that isn't the BLE link or the
  user-visible companion HTTP fallback (out of scope for v1).
- **License GPL-3.0-compatible.** All third-party libs must be
  compatible.
- **Background restraint.** Do not register `BluetoothLeScanner`
  callbacks that fire while the app is in the background. Do not
  request `FOREGROUND_SERVICE_CONNECTED_DEVICE`. Connections happen
  only while a relevant screen is visible.

## 10. Verification

### Without a real device

A minimal "device emulator" can be stood up by running shepherdd in
an Ubuntu 25.10 VM with a USB Bluetooth adapter passed through.
Recommended for CI of integration tests on Linux runners but probably
overkill for app dev day-to-day.

For unit tests, mock at the `ManagementClient` layer (§ module
layout) — the wire protocol is fully specified here, so a mock that
returns canned `RpcResponse`s is trivial.

### Against a real device

The shepherd-launcher project ships a config with
`[service.ble_management] enabled = true` in
`config.example.toml`. To smoke-test the app against a freshly built
device:

1. Install shepherd-launcher per `docs/INSTALL.md` in that repo.
2. Confirm advertising: `bluetoothctl scan le` from another Linux
   host should show the device under its `device_name`.
3. On the phone, run through §3 — scan, pair, claim. Confirm the TV
   shows the same 6-digit code as Android does.
4. Run through each §4 method and confirm the device behaviour
   matches.

### Recovery test

1. Pair successfully.
2. SSH to the device, `touch <data_dir>/.factory-reset-ble`, restart
   shepherdd.
3. On the phone, attempt to reconnect — the app should detect the
   missing bond (likely a `GATT_INSUFFICIENT_AUTHENTICATION` or
   pairing failure on the first encrypted op) and prompt the user to
   re-pair.

## 11. Open questions for the implementing agent

Make a call and document it; do not block on these.

1. **App display name and package id.** Suggest `com.shepherd.companion`
   and "Shepherd Companion", but neither is locked in.
2. **Persistence library.** EncryptedSharedPreferences vs.
   DataStore+Keystore. Both are fine; pick one and stick with it.
3. **Multi-device home-screen layout.** Horizontal chip selector vs.
   bottom sheet vs. drawer. Whatever's idiomatic in current Material 3.
4. **Charts for usage analytics.** A pure-Compose tiny bar chart is
   easy enough to roll; a chart library is overkill for v1. Pick
   accordingly.
5. **Reconnection backoff.** 1s → 2s → 5s → manual is a reasonable
   default if you have no opinion.
6. **Nickname propagation.** The `deviceName` shown in `DeviceInfo` is
   set on the device side. A `nickname` field on the local record
   lets the user re-label it client-side ("Kid's room"). Out of scope
   to expose anywhere on the wire.

## 12. References

These live in the shepherd-launcher repository; the agent should either
clone it or have its maintainer publish the relevant files alongside
this spec.

- `crates/shepherd-ble/src/protocol.rs` — UUID constants, RPC envelope
  types, error codes. Authoritative source for §2.
- `crates/shepherd-ble/src/server.rs` — GATT application layout and
  claim-flow dispatch. Useful for confirming who handles what.
- `crates/shepherd-ble/src/rpc.rs` — method-name → trait-call map.
  Cross-check §4 against this if a method's params look surprising.
- `crates/shepherd-api/src/types.rs` and `events.rs` — authoritative
  Rust types for everything `result` and `payload` in §4 and §5.
- `docs/ai/history/2026-06-20 002 ble-management.md` — device-side
  design doc (auth model, claim state machine, sentinel-based
  factory reset). Useful background for understanding *why* the
  protocol is shaped this way.
