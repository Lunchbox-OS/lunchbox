# The BLE advertising regression is fixed on `7.0.0-31`, and a bond to prove it

## Prompt

> I booted into 7.0.0-31 -- check to see if the BLE advertisement kernel
> regression is fixed now
>
> if the advertisement test works, go ahead and validate a BLE bond. you
> can use the attached phone, but you'll have to rebuild/reinstall the
> app and remove it's bond (it's to a different dev host, so it's
> recoverable)
>
> also keep in mind that one of the adapters on this environment is known
> to not work -- use the dongle

## The kernel question

The regression is
<docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md>: from
`7.0.0-28` the kernel's MGMT `Add Extended Advertising Data (0x0055)`
handler rejected valid payloads with `Invalid Parameters (0x0d)`, which
takes out *all* D-Bus advertising, shepherd included. It was still broken
on `-29`, and the fleet remedy was to pin `-27`.

On `7.0.0-31-generic` it is fixed. Three checks, in increasing fidelity:

1. `bluetoothctl advertise peripheral` registers, on **both** radios —
   the Realtek 5.4 dongle (`8C:68:8B:41:02:DC`) and the Qualcomm 5.3
   (`DC:56:7B:1F:7D:EA`). `btmon` shows the exact command that used to
   fail returning `Add Extended Advertising Data (0x0055) … Status:
   Success (0x00)`.
2. The same with a shepherd-shaped payload — Flags plus the 128-bit
   management service UUID, 21 bytes of advertising data, local name in
   the scan response. Also `Success`, and no `0x0d` in the bluetooth
   journal.
3. shepherdd itself: `BLE management advertising started device=shepherd
   service=8c0c0001-…`, and the companion app lists the device.

Nothing was pinned on this box (`apt-mark showhold` was empty), so no
unpinning was needed here; <docs/INSTALL.md> now carries the unhold
commands for hosts that are still pinned.

## The bond

Then the whole `companion-pairing` flow, from unclaimed to claimed, on
the dongle. It took five attempts, and every failure was environmental —
worth recording, because each one looked like a product bug.

**The dongle had to be pinned in config.** `[service.ble_management]
adapter = "8C:68:8B:41:02:DC"` in a copy of `config.example.toml`, booted
with `dev headless --config`. The daemon confirms the choice:
`Selected Bluetooth controller adapter=hci2 address=8C:68:8B:41:02:DC
selector="8C:68:8B:41:02:DC"`. Note `hciN` renumbers: `hciconfig` called
the dongle `hci0` at one point and `/sys/class/bluetooth` called it
`hci2` minutes later. The address is the only stable handle.

**adb over SSH needed a udev rule.** `adb devices` reported
`no permissions (missing udev rules? user is in the plugdev group)`
despite `plugdev` membership: `uaccess` had granted the node to
`gdm-greeter`, the seat user at the login screen, and an SSH session is
not a seat. `/etc/udev/rules.d/51-android.rules` granting `plugdev` fixes
it; the recipe is in the skill.

**Both stale bonds had to go.** The phone was bonded to a *different*
shepherd host (`28:18:78:45:B6:1E`), and this host had a stale record for
the phone under **both** adapters. `bluetoothctl remove` on the default
controller only clears one — `/var/lib/bluetooth/<adapter>/<peer>/info`
is the thing to check.

**Two shepherds were in range, and they are indistinguishable in the
app.** The advertised name is capped at 8 bytes, so both rows read
`shepherd`; `pair.sh` tapped the other machine and reported "device never
requested numeric comparison" while the daemon log sat empty — because
the daemon was never talked to. `bluetoothctl scan le` from the host's
*other* radio listed both advertisers and settled it. `pair.sh` now takes
`DEVICE=<address>`.

**Android asks for consent before it will pair at all.** This was the
real one. `btmon` showed the device answering a read with `Insufficient
Authentication (0x05)`, the phone entering `BT_BOND_STATE_BONDING`, and
then *nothing on the wire* for 30 s until `SMP_RSP_TIMEOUT`. On Android
16 / SDK 37 the phone raises `ACTION_PAIRING_REQUEST` with
`pairingVariant=3` (consent) **before** sending the SMP Pairing Request;
confirming it is what starts SMP, and only then does a second prompt
arrive with `pairingVariant=2` and the six digits. `pair.sh run` waited
for the daemon's `Numeric Comparison pairing requested` *before* touching
any prompt, which deadlocks: the device cannot ask until consent is
given. The fix in `pair.sh` is to confirm the consent round first.

This also **retires a misleading gotcha**. The skill listed
`smp_act: smp_send_app_cback: Unexpected event:2` followed by
`SMP_RSP_TIMEOUT` as the signature of a wedged phone whose only cure is
Settings → Reset Bluetooth & Wi-Fi. It is not: that line is logged on
every pairing, at the moment the consent prompt is posted, and a healthy
run goes straight through it. A Bluetooth stack restart changed nothing
here, as expected. The skill now says so, so nobody reaches for the
destructive reset first.

**The notification shade reflows under you.** Twice the tap aimed at
`Pair & connect` landed on the phone's ongoing "Charging on hold to
protect battery" notification and opened Battery settings, burning the
30 s window. `pair.sh` now verifies the dialog actually opened
(`dumpsys window` → `BluetoothPairingDialog`) and retries; snoozing the
other notifications first (`cmd notification list`, then
`cmd notification snooze --for 1800000 '<key>'`, quoted — the key
contains `|`) makes it first-try reliable.

## Result

```
[22:26:19] tapping row: 8C:68:8B:41:02:DC  ·  -36 dBm
[22:26:29] consented — SMP starts here
[22:26:30] device passkey: 963642
[22:26:40] MATCH: phone 963642 == device 963642
```

with, on the device:

```
INFO shepherdd::pairing_display: Spawned pairing-display overlay passkey=963642 method="compare"
INFO shepherd_ble::agent:  Pairing complete; hiding passkey overlay
INFO shepherd_ble::server: BLE RPC received id=1 method=claim
INFO shepherd_ble::claim:  Admin claim recorded device=Pixel 10a
```

an `admin.toml` written, the app showing "Paired", and the phone's bond
reading `LE:Y` with `EncryptionStatus{keySize=16, algorithm=2}`. A
reconnect (`am force-stop`, relaunch) came back over the encrypted link
and served `get_volume`, `get_brightness` and `list_audio_outputs`.

## Changes

- <docs/INSTALL.md>, <docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md>:
  the affected range is `-28` … `-30`; `-31` is fixed; unhold instructions.
- <.claude/skills/companion-pairing/SKILL.md>: kernel range, the adb
  udev rule for SSH, the two-shepherds trap, the consent round, the
  shade-reflow trap, and the corrected `Unexpected event:2` reading.
- `.claude/skills/companion-pairing/pair.sh`: `DEVICE=` selector,
  consent-then-comparison flow, verified prompt opening, exact-label
  matching for the OS buttons, and it no longer confirms a **mismatched**
  comparison.
