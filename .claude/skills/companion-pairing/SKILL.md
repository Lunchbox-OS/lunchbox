---
name: companion-pairing
description: >-
  Pair the Shepherd Companion Android app with a shepherd device over BLE, and
  verify the result on both sides — the flow no unit test can reach (Numeric
  Comparison, bonding, claim, reconnect, re-pair after factory reset). Use
  whenever you change `ShepherdConnection`, `BondManager`, the pairing UI, or
  anything in `crates/shepherd-ble`, and whenever you need to confirm a pairing
  or reconnect bug end-to-end. Drives a USB-attached phone over adb against the
  headless dev session, so it works over SSH with no graphical login.
---

# Pairing the companion app end-to-end

Pairing is the part of the stack unit tests cannot touch: Numeric
Comparison, OS-level bonding, the `claim` RPC, and the encrypted-link
reconnect all live in the Android and BlueZ stacks. Every regression
this project has shipped in `ShepherdConnection`/`BondManager` was a
mechanism error invisible in the Kotlin — implicit bonding, lazy
encryption, a queue outrunning its reader. **Changes to those files need
a pass through this skill before they land.**

## Prerequisites

1. **A kernel without the extended-advertising regression.** On Ubuntu
   26.04, `7.0.0-28` through `-30` cannot register *any* LE
   advertisement, so the device never goes on air and nothing below
   works. `-27` and `-31`-and-later are fine (the fix landed in `-31`,
   confirmed 2026-09-04). Check with `uname -r`, and see
   <docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md>
   and the "BLE management doesn't advertise" section of
   <docs/INSTALL.md>.
2. **A phone on USB with debugging authorised.** `adb devices` must show
   `device`, not `unauthorized` — the phone shows an RSA-fingerprint
   prompt the first time and a human has to accept it.

   Over SSH it can instead say **`no permissions (missing udev rules?)`
   even though you are in `plugdev`**: `uaccess` hands the USB node to
   whoever holds the graphical seat (`gdm-greeter` on a box sitting at
   the login screen), and an SSH session is not a seat. Grant the group
   explicitly, once:

   ```sh
   echo 'SUBSYSTEM=="usb", ATTR{idVendor}=="18d1", MODE="0664", GROUP="plugdev"' \
     | sudo tee /etc/udev/rules.d/51-android.rules
   sudo udevadm control --reload-rules
   sudo udevadm trigger --subsystem-match=usb --action=change
   adb kill-server && adb start-server
   ```

   **The rule is per *vendor*, so a new phone needs its own line.** The
   file already carries `18d1` (Google/Pixel) and `22b8` (Motorola);
   anything else reports `no permissions` until you add it. `lsusb` gives
   the id — and if the phone shows up there but not in `adb devices` at
   all, check the interface: a single `255/255/0` interface is MTP with
   USB debugging *off*, and no udev rule will conjure the missing
   `255/66/1` adb interface. That one needs a human at the phone
   (Settings -> About phone -> tap Build number x7, then Developer
   options -> USB debugging).

   It must also be **usable**: `uiautomator dump` on a lock screen returns
   a tree with no app labels, so `pair.sh ui` prints nothing and every
   `tap` reports "not found".

   **The dev phone has no lock screen.** It was deliberately cleared
   (2026-08-22) so automation never needs a human:

   ```sh
   adb shell locksettings clear --old <pin>   # secure PIN -> Swipe
   adb shell locksettings set-disabled true   # Swipe -> None
   ```

   Both are needed: `set-disabled` "can only change between Swipe and
   None" by its own help, so clearing alone leaves a swipe screen. Waking
   now lands straight on the launcher — `adb shell input keyevent
   KEYCODE_WAKEUP`, then confirm with `dumpsys window | grep
   mCurrentFocus` (expect `NexusLauncherActivity`, not
   `NotificationShade`).

   **If a lock ever gets set again, adb cannot remove it for you.**
   On this build (Pixel 10a, SDK 37) injected input does not reach the
   bouncer at all: a mid-screen swipe, `input text <pin>`, digit
   keyevents, and `wm dismiss-keyguard` were each tried and
   `deviceLocked` stayed `1` throughout. `locksettings verify --old <pin>`
   *does* authenticate, but authenticating is not dismissing — there is no
   supported "unlock over adb" for a secure lock. So either clear the
   credential again with the commands above, or ask a human to unlock the
   phone by hand. Do not burn time driving the bouncer; it does not work.

   A phone that is *not* the dev phone has none of this — ask its owner.
3. **`[service.ble_management] enabled = true`** in the config the
   session boots (true in `config.example.toml`).

## Two phones on the bench

Issue #149 (multiple companion bonds) needs a second phone, so the bench
now has two:

| Serial | Phone | Android | Notes |
| --- | --- | --- | --- |
| `63251JEA305665` | Pixel 10a (`stallion`) | 16 / SDK 37 | The original dev phone. No lock screen. |
| `ZY22F6Z6NT` | moto g power (2021) (`borneo`) | 11 / SDK 30 | No lock screen. **Below the app's `minSdk = 31`** — see below. |

**Every bare `adb` call fails the moment both are plugged in**
(`error: more than one device/emulator`), and `pair.sh` calls bare `adb`
throughout — including `require_adb`, so it aborts with the misleading
"no adb device (check 'adb devices' …)". Do not add `-s` to the script:
`adb` already reads **`ANDROID_SERIAL`**, so export it once and every
call in the shell — and in `pair.sh` — targets that phone.

```sh
export ANDROID_SERIAL=63251JEA305665   # the Pixel
./.claude/skills/companion-pairing/pair.sh ui
```

`SHOTDIR` is shared, so give each phone its own when driving both in one
session (`SHOTDIR=/tmp/shepherd-pairing/moto`) or the second run
overwrites the first's `result.png`.

The two phones are deliberately unalike — different Android versions,
vendors and Bluetooth stacks — which is the point for a multi-bond
feature: the "Android asks twice" consent ordering and the notification
mechanics below are Pixel/SDK 37 observations and do **not** hold on the
Motorola (see below).

### Only one phone at a time can even see the device

A peripheral stops advertising while a peer is connected. BlueZ goes on
reporting the advertisement as registered — `ActiveInstances` still reads
1 — so from the daemon's side everything looks healthy while the second
phone's scan lists nothing at all. **Whenever a scan comes up empty,
check whether the other phone is holding the link before anything else:**

```sh
busctl --system get-property org.bluez /org/bluez/hci2/dev_<peer> \
  org.bluez.Device1 Connected
adb -s <other-phone> shell am force-stop com.armeafamily.shepherd.companion
```

The device is back on air a few seconds after the peer drops. This makes
the two-phone enrolment flow inherently a relay — phone B asks, B is
parked, A approves, A is parked, B collects — and it is why #149 shipped
turn-taking rather than concurrent sessions: the radio was already
enforcing it.

### `pair.sh run` works on the Motorola — get the state right first

An earlier note here claimed it did not. That was wrong, and worth
recording as a wrong diagnosis: `run` reported `consent prompt never
opened` on a phone whose bond state was broken (the device held a key
the phone did not), and the prompt genuinely never appeared because the
link was being torn down mid-handshake. With both sides clean it drives
Android 11's prompts fine.

What `run` *does* do on a second phone is time out at the end, because
it waits for `Paired` and an unapproved phone stops at "Waiting for
approval". That is success, not failure — check the daemon log rather
than the exit code:

```sh
grep -aE "Numeric Comparison|second phone asked" dev-runtime/headless/sway.log
```

Tapping the scan row by hand and letting the OS raise its own prompt
also works, and needs no script.

### The entry point depends on whether the phone remembers the device

`pair.sh tap "Pair a device"` only finds that label on the empty-state
home screen. A phone that has been revoked still holds its local record
and lands on **"Bond lost — re-pair needed"** instead, whose button is
`Re-pair`. Both lead to the same scan list; tapping for the wrong one
reports `not found` and then `run` sits waiting for a scan nobody
started, which reads as "the device isn't advertising".

### Clearing a bond on the Motorola

There is no adb unpair, and `pm clear com.google.android.bluetooth` is
the thing the gotchas below warn against. Use the Settings UI, which
automates fine:

```sh
adb shell am start -a android.settings.BLUETOOTH_SETTINGS
# then: tap the gear beside the device -> Forget -> Forget device
```

Check *both* sides afterwards. A bond one side has and the other does not
is the asymmetric lockout, and it looks like a product bug:

```sh
adb shell dumpsys bluetooth_manager | grep -aA3 "^  Bonded devices"
sudo find /var/lib/bluetooth -maxdepth 2 -mindepth 2 -type d -not -name cache
```

### The Motorola runs the pre-Android-12 permission path

`minSdk` was lowered 31 -> 30 for this phone (2026-09-07, for #149), so
the app installs and runs on Android 11. Nothing in the dependency set
floored above 21; the 31 was purely the manifest's permission model.
Below Android 12 there are no split `BLUETOOTH_SCAN` /
`BLUETOOTH_CONNECT` permissions, so `AndroidManifest.xml` declares
`BLUETOOTH`, `BLUETOOTH_ADMIN` and `ACCESS_FINE_LOCATION` capped at
`maxSdkVersion="30"`, and `ui/App.kt` picks the matching runtime list off
`Build.VERSION.SDK_INT`.

Three consequences when driving the Motorola:

- **Grant location, not the Bluetooth pair.** The skill's `pm grant`
  loop over `BLUETOOTH_SCAN`/`BLUETOOTH_CONNECT`/`POST_NOTIFICATIONS` is
  a no-op here — those permissions do not exist on SDK 30. Grant
  `ACCESS_FINE_LOCATION` (and `ACCESS_COARSE_LOCATION`, which AGP pulls
  in alongside it) instead.
- **Location *services* must be on, not just the permission.** Android
  10+ returns an empty BLE scan with the global location toggle off, and
  it fails silently — the scan list simply stays empty, exactly as it
  looks when the device is not advertising. This phone shipped with it
  off:

  ```sh
  adb shell cmd location set-location-enabled true
  adb shell settings get secure location_mode      # expect 3
  ```

  (`settings put secure location_mode 3` does not stick; use the `cmd`.)
- **It sleeps at 60 s and then `uiautomator dump` returns nothing**, so
  `pair.sh` reports `not found:` for labels that are plainly on screen.
  Pin it awake while on USB:

  ```sh
  adb shell settings put global stay_on_while_plugged_in 7   # AC|USB|wireless
  ```

Verified end-to-end on 2026-09-07: install, permission gate, and a scan
that lists `shepherd` at the pinned adapter address. Pairing itself has
*not* been exercised from this phone — the "Android asks twice" consent
ordering and the notification mechanics below are Pixel/SDK 37
observations and should be re-derived here rather than assumed.


## One-time setup

```sh
export PATH=/opt/android-sdk/platform-tools:$PATH
cd companion-android && ANDROID_HOME=/opt/android-sdk ./gradlew :app:assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

If that fails with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`, a release-signed
build is already installed and the only way past it is
`adb uninstall com.armeafamily.shepherd.companion`. **That erases the
app's data, including admin records and claim tokens for real devices** —
confirm with the owner first; the token is not recoverable.

Grant the runtime permissions so the flow doesn't stop on a system
dialog:

```sh
for p in BLUETOOTH_SCAN BLUETOOTH_CONNECT POST_NOTIFICATIONS; do
  adb shell pm grant com.armeafamily.shepherd.companion android.permission.$p
done
```

## The loop

```sh
./scripts/shepherd dev headless          # device side (see the headless-dev skill)
adb shell am start -n com.armeafamily.shepherd.companion/.MainActivity
./.claude/skills/companion-pairing/pair.sh tap "Pair a device"
DEVICE=8C:68:8B:41:02:DC ./.claude/skills/companion-pairing/pair.sh run
```

`DEVICE` picks the scan row. Leave it unset for the plain `shepherd`
label; set it to the serving controller's address when a second shepherd
is in range (see the two-radios gotcha below) — the app prints the
address under each row, so this is the only way to tell them apart.

`run` waits for the scan list, taps the device, confirms the OS
*consent* prompt (which is what puts SMP on the wire), waits for a
*fresh* device-side passkey request, opens the comparison prompt,
compares the digits against the daemon's, confirms, and reports both
sides. A successful run
ends with `Paired`, a `claim` RPC in the daemon log, an `admin.toml`, and
a phone bond showing `LE:Y` with `EncryptionStatus{keySize=16`.

| Command | Purpose |
| --- | --- |
| `pair.sh ui` | Every on-screen label with tap coordinates |
| `pair.sh tap <text>` | Tap a node by label (exact match wins over substring) |
| `pair.sh run` | The full flow, from the "Pair a device" list |

Before a run, clear the notification shade — see the shade-reflow gotcha
below.

Screenshots land in `$SHOTDIR` (default `/tmp/shepherd-pairing`) — Read
`result.png` to actually see the end state.

## What to exercise

- **First pairing** — from unclaimed. The digits must match on the TV
  (`dev shot`) and the phone.
- **Reconnect** — `am force-stop` then relaunch. Watch for
  `… drained N stale bytes over M reads on (re)connect` followed by
  `dispatch: response id=…` — that is the bounded drain working.
- **Reconnect after a daemon restart** — restart the session with the
  bond intact; this is the path that needs link encryption to come up
  before the first read succeeds.
- **Re-pair after factory reset** — `touch dev-runtime/data/.factory-reset-ble`
  and restart the session. (That path is the dev stack's, which runs with
  `--no-state-custodian`. On an installed device the sentinel is the *device's*
  and lives at `/var/lib/shepherdd/admin/.factory-reset-ble` — issue #157.) The device returns to unclaimed and drops its
  bond; the app should show "Bond lost — re-pair needed", and `Re-pair`
  leads back to the scan list (not straight into pairing).

## Gotchas (each of these cost an hour)

- **Android asks twice, and the first ask is not the comparison.** On
  Android 16 / SDK 37 the phone raises `ACTION_PAIRING_REQUEST` with
  `pairingVariant=3` (consent) *before* it sends anything: `btmon` shows
  the device answering a read with `Insufficient Authentication (0x05)`,
  the phone going `BT_BOND_STATE_BONDING`, and then **nothing on the
  wire** until that consent is confirmed. Only then does it send the SMP
  Pairing Request and raise a second prompt, `pairingVariant=2`, carrying
  the six digits. Waiting for the daemon's `Numeric Comparison pairing
  requested` before touching the first prompt therefore **deadlocks**:
  the device cannot ask until consent has been given, and the run dies
  with "device never requested numeric comparison" after the phone's 30 s
  `SMP_RSP_TIMEOUT`. `pair.sh run` handles both rounds; drive them in the
  same order by hand.
- **The notification action is not the confirmation.** Each round arrives
  as a "Pairing request" notification whose `Pair & connect` action only
  *opens* the dialog; the dialog's own `Pair` button is what proceeds.
  Confirm with the notification shade **collapsed** — with it expanded, a
  `Pair` label up there shadows the dialog button, the tap lands on the
  notification, and the phone silently sits out the 30s SMP timeout. On
  the wire that appears as `Remote User Terminated Connection` exactly
  30.0s after `User Confirmation Request`, with no SMP `Pairing Failed`
  at all — it looks exactly like a product bug.
- **Clear the notification shade before a run.** The shade reflows as
  notifications arrive and leave, so a coordinate read a second ago lands
  on whatever slid into its place: a tap meant for `Pair & connect` opened
  *Battery settings* twice in a row off the phone's ongoing "Charging on
  hold to protect battery" notification, and the 30 s window ran out.
  `pair.sh` now verifies the dialog actually opened (`dumpsys window` →
  `BluetoothPairingDialog`) and retries, but the reliable fix is to snooze
  the noise first:

  ```sh
  adb shell cmd notification list
  adb shell "cmd notification snooze --for 1800000 '<key>'"   # quote it: | is a shell pipe
  ```
- **Never snooze a `com.android.settings` notification during pairing.** The
  OS pairing prompt *is* one, and it reuses a single key
  (`0|com.android.settings|17301632|null|1000` on this phone), so snoozing
  that key suppresses every later consent prompt for the whole snooze window.
  A stale pairing notification left over from an aborted attempt is exactly
  the "noise" the rule above tempts you to snooze, and doing so cost three
  failed pairings here: the app showed **"Pairing failed — Cannot connect
  peripheral that has been cancelled"** (Kable, after the phone's 30 s
  `SMP_RSP_TIMEOUT`) while logcat said the framework had done everything
  right —

  ```
  BluetoothBondStateMachine: sendPairingRequestIntent: ACTION_PAIRING_REQUEST … variant=3
  BluetoothPairingService: Show pairing notification for  (shepherd-26.04)
  ```

  …and `cmd notification list` showed no such notification. That combination —
  the service says it posted, the list does not have it — means snoozed, not
  wedged; no amount of restarting `bluetooth` or the app will help. Undo it:

  ```sh
  adb shell "cmd notification unsnooze '0|com.android.settings|17301632|null|1000'"
  ```

  Snooze the *charging* notification by all means; check the key's package
  first, and prefer clearing a stale pairing notification (swipe / relaunch)
  over snoozing it.
- **Confirm promptly.** The prompt times out in 30s. Don't interleave
  screenshots and image reads between the taps; let `run` do it.
- **Only trust a *fresh* prompt.** A leftover notification from an
  earlier attempt looks identical. `run` gates on the daemon logging a
  new `Numeric Comparison pairing requested` first; do the same by hand.
- **The daemon logs the passkey unpadded** (`passkey=90481` for a code
  the phone renders as `090481`), so a matching pair reads as a mismatch
  when eyeballing logs against screenshots.
- **Bonding starts before the app asks for it.** Any ATT read of an
  encrypt-authenticated characteristic initiates bonding, so the OS
  prompt can appear while the app still says "Reading device info…".
  Never tear down a bond mid-flow on the assumption it is stale.
- **The device auto-accepts the comparison.** Its agent replies
  `User Confirmation Reply: Success` within a millisecond and shows the
  digits on the TV; the human comparison happens on the phone. A stalled
  pairing is therefore always the phone's side.
- **Never suspend the dev box to test a reconnect.** It is a KVM/QEMU
  guest with a PCI-passthrough USB card: `rtcwake -m mem` enters s2idle
  and the guest never comes back (the RTC alarm doesn't wake it), so it
  takes a host-side reboot and `/tmp` — scratch scripts, results — goes
  with it. To exercise what a suspended box does to the companion, take
  the serving controller down instead: `sudo hciconfig hciN down`, wait
  longer than the app's backoff ladder (**6 minutes**; 75 s is not
  enough — it recovers on its own at the 4th attempt), then bring it up.
  See <docs/ai/history/2026-08-16 001 ble-connect-fails-after-long-session.md>.
- **Repeated aborted attempts can wedge the stack.** Runs of failed
  pairings have produced `SMP: Pairing Failed, Reason: DHKey check
  failed (0x0b)` on otherwise-valid attempts, cleared by
  `sudo systemctl restart bluetooth`. Rule that out before believing a
  pairing bug reproduces.
- **`smp_send_app_cback: Unexpected event:2` is not a wedged phone.**
  It is logged on *every* pairing, at the moment the framework enters
  `BOND_BONDING` and posts the consent prompt, and a healthy run carries
  straight on through it. Paired with the 30 s `SMP_RSP_TIMEOUT` that
  follows an unanswered consent prompt it looks exactly like the wedge
  below — it is not one, and a phone-side reset will not help. Check
  whether a `pairingVariant=3` prompt is waiting before concluding
  anything (confirmed 2026-09-04; see
  <docs/ai/history/2026-09-04 003 ble-advertising-fixed-on-7.0.0-31.md>).
- **The phone's stack really can wedge, and only a settings reset clears
  it.** After `pm clear com.google.android.bluetooth`, the phone stopped
  answering the device's `SMP: Security Request` — its framework reported
  `BOND_BONDING` while nothing went on the wire, and logcat showed
  `smp_act: smp_send_app_cback: Unexpected event:2` followed 30 s later
  by `SMP_RSP_TIMEOUT`. A reboot, a Bluetooth toggle and a second storage
  wipe all failed to fix it; **Settings → System → Reset options → Reset
  Bluetooth & Wi‑Fi** fixed it immediately (it erases saved Wi‑Fi
  networks but leaves cellular alone). Clearing the Bluetooth package's
  storage is *not* a safe reset — prefer the settings reset if you need
  to clear phone-side Bluetooth state at all.
- **Two shepherds in range look identical in the app.** The advertised
  name is capped at 8 bytes, so every device is just `shepherd` and
  `pair.sh`'s label match taps whichever the scan listed first — which may
  be a *different machine on the desk*, and then nothing works and the
  daemon log stays silent because it was never involved. The app prints
  each row's controller address; pass it as `DEVICE=`. `bluetoothctl scan
  le` from the host's other radio enumerates who is actually advertising.
- **With two radios present, shepherdd serves whichever BlueZ lists
  first** — not the one you meant, and not necessarily the one the phone
  is bonded to. Symptom: the app sits on "Connecting…" forever while the
  daemon logs `BLE management advertising started` and looks perfectly
  healthy, because the phone's GATT connects are going to the *other*
  controller's address. Compare the peer address in the phone's logcat
  (`btif_gattc_open_impl: … address=xx:xx:xx:xx:02:dc`) against the
  adapter in the daemon log (`BLE management server starting adapter=hciN`)
  and `bluetoothctl list`.
  **Pin it in config** — `[service.ble_management]` takes an `adapter`
  key, and it wants the controller *address*, not the `hciN` name (the
  index tracks USB probe order and renumbers across boots):

  ```toml
  [service.ble_management]
  adapter = "8C:68:8B:41:02:DC"
  ```

  An adapter that isn't present is a startup error listing the ones that
  are, rather than a silent fallback. Copy `config.example.toml`, set the
  key, and boot with `dev headless --config <copy>`.
  Downing the other radio does *not* redirect it — BlueZ still exposes a
  downed adapter and registration just fails. If you are on a build that
  predates the `adapter` key, unbind the one you don't want from `btusb`
  so BlueZ stops seeing it: `ls -l /sys/class/bluetooth/hci*` gives the
  USB id, then `echo -n 3-6:1.0 | sudo tee /sys/bus/usb/drivers/btusb/unbind`
  (`…/bind` to restore — note it may come back under a *different* hci
  index, which changes which adapter is "first").
- **A controller can be individually broken, and it looks like a code
  bug.** On the dev box the Qualcomm radio (`DC:56:7B:1F:7D:EA`,
  Foxconn `0489:e10a`) reached ACL connect and then never completed
  service discovery: `discoverServices()` fired, `onSearchComplete`
  arrived only when the 30 s connect budget tore the link down, and every
  pairing failed. It reproduced on an unmodified build, survived a phone
  reboot, `hciconfig reset`, a `bluetoothd` restart and a full wipe of
  the phone's Bluetooth storage — and vanished the moment the daemon was
  moved to the Realtek dongle, where Numeric Comparison completed first
  try. If discovery hangs for the whole connect budget, **swap adapters
  before you debug the app**.

## When it fails, get the wire

`btmon` is what separates a real defect from a driving artifact:

```sh
sudo btmon -i hci1 > /tmp/btmon.txt      # while running the flow
grep -aE "SMP:|Reason:|User Confirmation" /tmp/btmon.txt
```

A healthy Numeric Comparison run is Pairing Request/Response → Public Key
×2 → Confirm → Random ×2 → `User Confirmation Request` → DHKey Check →
encryption. Where it stops tells you which side gave up and why.
