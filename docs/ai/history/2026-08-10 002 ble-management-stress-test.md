# BLE management stress test

> stress test the BLE management, including high CPU load on either and
> both ends, restarting shepherdd and the app, sleep and waking the
> phone, and anything else you think might be a good idea

Run against real hardware: Pixel 10a (Android 17) over adb, shepherdd
headless on a Qualcomm 5.3 controller (`hci1`), kernel `7.0.0-27-generic`
(the advertising-regression pin — see
<docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md>).
Harness and per-scenario evidence were driven by the `companion-pairing`
skill's approach; the probe is background → foreground (which tears the
connection down and rebuilds it) with a pass requiring **fresh RPCs in
the daemon log**, so it exercises connect → encrypt → drain → dispatch
rather than merely checking a process is alive.

## Results: 38 scenarios, 35 passed

Reconnect is consistently **3–7 s** and did not degrade under any load.

| Scenario | n | Result |
| --- | --- | --- |
| Baseline reconnect | 3 | pass, 3–4 s |
| Host CPU saturated (8 busy loops) | 3 | pass, 3–4 s |
| Phone CPU saturated | 3 | pass, 4 s |
| Both saturated (loadavg ≈7.5 / 7.0) | 3 | pass, 3–4 s |
| App force-stop + relaunch | 3 | pass, 4–5 s |
| Rapid background/foreground churn | 5 | pass, 3–4 s |
| Screen off → wake + PIN unlock | 3 | pass, 4/11/4 s |
| 90 s screen-off (doze) | 1 | pass, 3 s |
| Locked idle (no unlock) | 1 | pass, 0 disconnects |
| Phone Bluetooth off/on | 2 | pass, 5–6 s |
| Airplane mode on/off | 1 | pass, 7 s |
| Device adapter reset (`hci1` down/up) | 1 | pass, 4 s |
| Kill app mid-connect (races the handshake) | 3 | pass, 5 s |
| Everything at once (both loaded + daemon restart + sleep/wake) | 2 | pass, 5–6 s |
| 5-minute idle soak | 1 | pass — 0 disconnects, 0 exceptions |
| **Daemon restart, app left untouched** | 3 | **fail** |

The link layer is solid. Every failure is the same defect, and it is not
in the transport.

## Finding 1: the app never reconnects on its own

Restart shepherdd while the companion app is foregrounded and it stays
dead. Not for 60 s, not for 180 s — it makes no attempt at all. The only
recovery is backgrounding and re-foregrounding the app, because
`ShepherdViewModel.onBackground()` tears the connection down and
returning rebuilds it. Every "pass" in the table above is really that
manual cycle.

For the daemon this is a routine event (it restarts with the session),
so a parent who restarts the TV and then opens the app on a phone that
was already showing it gets a screen that never updates.

## Finding 2: a dead link looks exactly like a live one

Worse than the missing retry. With the daemon **down**, the foregrounded
app renders a fully populated screen — every activity, block reason and
time budget — with no banner, no greying, no staleness marker
(`stress/daemon-down.png`). The state shown is whatever was last
fetched, presented as current.

That is a correctness problem, not a cosmetic one: the whole point of
the app is telling a parent what the device is doing right now. A
"Blocked / Outside allowed hours" row that is silently minutes stale is
indistinguishable from a live one.

`LinkStatus` already exists and `onBackground()` sets `LinkStatus.Idle`,
so the model has somewhere to put this — nothing surfaces it when the
link drops underneath a foregrounded app.

## Finding 3: re-pairing accumulates duplicate devices

Four re-pairings during testing left **four device chips in the
switcher**, all for the same physical device.

`ShepherdRepository.upsert` dedupes on `identityAddress`:

```kotlin
val next = _records.value.filter { it.identityAddress != record.identityAddress } + record
```

`identityAddress` is what the daemon put in `admin.toml` — the peer
address BlueZ surfaced at claim time, which mid-pairing is the phone's
**resolvable private address** and is different on every pairing. The
filter therefore never matches and each re-pair appends a new record
carrying its own now-useless token.

The correct key is already in the record and unused for identity:
`androidIdentifier`, the device's BLE address (`DC:56:7B:1F:7D:EA`),
which is stable across re-pairings.

This is the concrete consequence of the RPA handling described in
<docs/ai/history/2026-08-01 001 ble-connect-drain-unbounded.md>. The
daemon-side `authorize()` deliberately tolerates the drift, so it was
believed cosmetic; it is not — it corrupts the app's device list.

## Finding 4: devices are labelled with the phone's name

Every chip reads "Pixel 10a" — and so does the pairing confirmation
("Pixel 10a is now paired with this phone"). `ShepherdRecord.deviceName`
is documented as "The device's own name (from DeviceInfo / the claim
record)", but it is populated from the claim response, whose
`device_name` is the **admin phone's** name (`claim(phoneName)` — correct
for an admin record, wrong as a device label). The device's real name is
already fetched as `info.deviceName` and used for
`PairingPhase.Claiming`; the record should use it too.

With findings 3 and 4 together, a user who re-pairs twice sees a device
switcher full of identically-named entries with no way to tell them
apart.

## What did not break

Worth recording, because these were the suspicions going in: sustained
CPU starvation on either or both ends changed nothing (reconnect stayed
3–4 s at loadavg ≈7.5); the connection survives 5 minutes idle with zero
spontaneous disconnects; radio loss on either end recovers cleanly; and
killing the app mid-handshake leaves no wedged state on the device — the
next attempt connects normally. The bounded-drain and link-encryption
fixes on this branch held up under every one of these.

## Harness notes

Two harness bugs produced false failures before they were fixed, both
worth avoiding next time. `grep -c` prints `0` *and* exits non-zero, so
`grep -ac … || echo 0` yields `"0\n0"` and every arithmetic comparison
breaks. And a blind wake+swipe+PIN silently no-ops if the display has
not come up — three "sleep/wake failures" were an unlocked-phone
assumption, not a BLE problem. Verify the unlock (`isKeyguardShowing`)
and retry. Note also that `screencap` of a locked phone returns pure
black: the keyguard is a secure surface, so a black PNG means "locked",
not "screen off".
