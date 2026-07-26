# BLE advertisement name overflow made the device undiscoverable

## Symptom

The companion Android app could not pair with a shepherd device. It
"just never found it" — no error, the device simply never appeared in
the pairing list.

## Diagnosis

Filtering `adb logcat` by the app package hid the Bluetooth stack (it
logs under the system bluetooth process, not the app PID). Widening the
filter showed the companion's scan starting cleanly and receiving
**zero** results on its own `scannerId` — the phone's radio worked
(other scanners got hits), but nothing matched.

The companion scans with a hardware-offloaded `ScanFilter` on the
128-bit management service UUID (`ShepherdScanner.kt`). nRF Connect —
which scans unfiltered and matches in software — *did* see the device,
so the split was "radio sees it, UUID filter doesn't". nRF's raw
advertising data decoded to just Tx Power + the local name, with **no
Flags and no service UUID** — i.e. it was the adapter's own name-only
discoverable broadcast, not a registered `LEAdvertisement1`.

The device journal had the real failure:

```
bluetoothd: src/advertising.c:add_client_complete() Failed to add advertisement: Invalid Parameters (0x0d)
```

`device_name` defaults to the system hostname (here `copernicus`, 10
bytes). The advertisement overran the 31-byte legacy PDU:

```
Flags:                    3
128-bit service UUID:     2 + 16 = 18
Local name "copernicus":  2 + 10 = 12
                          --------------
                          33  >  31  ->  0x0d
```

The controller rejected the *whole* advertisement, so the service UUID
never went on air and the UUID-filtered scan had nothing to match. The
bug was host-dependent: a shorter hostname would have squeaked under 31
bytes and "worked", which is exactly what made it a latent trap.

## Fix

`crates/shepherd-ble/src/server.rs`: trim the *advertised* name to
`MAX_ADV_NAME_BYTES` (= 31 − 3 − 18 − 2 = 8 bytes) on a UTF-8 char
boundary before handing it to `advertise()`, with a trailing ellipsis
(`…`, 3 bytes, reserved out of the budget) so a shortened name reads as
shortened in the scan list (`copernicus` → `coper…`). `warn!` fires when
a name is trimmed. The service UUID always fits now; the full device
name still reaches the companion over GATT (the app already falls back
to the GATT name). `bluer` 0.17 has no scan-response field, so we can't
push the name into the scan response — trimming the primary PDU is the
reliable lever.

## If it recurs / follow-ups considered

- The 8-byte budget is conservative: it assumes BlueZ packs the name
  into the same 31 bytes as the UUID (which the 0x0d proved it does on
  this controller). If a future controller relocates the name to the
  scan response, an 8-byte name still fits — no regression.
- Longer names would need **LE Extended Advertising**
  (`secondary_channel`), but that also requires the Android side to scan
  for extended advertisements — deferred as more moving parts than the
  problem warranted.
