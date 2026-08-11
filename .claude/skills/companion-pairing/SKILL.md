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
   26.04 that means `7.0.0-27-generic`; `-28` and `-29` cannot register
   *any* LE advertisement, so the device never goes on air and nothing
   below works. Check with `uname -r`, and see
   <docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md>
   and the "BLE management doesn't advertise" section of
   <docs/INSTALL.md> for the pin.
2. **A phone on USB with debugging authorised.** `adb devices` must show
   `device`, not `unauthorized` — the phone shows an RSA-fingerprint
   prompt the first time and a human has to accept it.
3. **`[service.ble_management] enabled = true`** in the config the
   session boots (true in `config.example.toml`).

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
./.claude/skills/companion-pairing/pair.sh run
```

`run` waits for the scan list, taps the device, waits for a *fresh*
device-side passkey request, opens the OS prompt, compares the digits
against the daemon's, confirms, and reports both sides. A successful run
ends with `Paired`, a `claim` RPC in the daemon log, an `admin.toml`, and
a phone bond showing `LE:Y` with `EncryptionStatus{keySize=16`.

| Command | Purpose |
| --- | --- |
| `pair.sh ui` | Every on-screen label with tap coordinates |
| `pair.sh tap <text>` | Tap a node by label (exact match wins over substring) |
| `pair.sh run` | The full flow, from the "Pair a device" list |

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
  and restart the session. The device returns to unclaimed and drops its
  bond; the app should show "Bond lost — re-pair needed", and `Re-pair`
  leads back to the scan list (not straight into pairing).

## Gotchas (each of these cost an hour)

- **The notification action is not the confirmation.** The OS shows a
  heads-up "Pairing request" whose `Pair & connect` action only *opens*
  the numeric-comparison dialog; the dialog's `Pair` button is what
  completes SMP. Confirm with the notification shade **collapsed** — with
  it expanded, a `Pair` label up there shadows the dialog button, the tap
  lands on the notification, and the phone silently sits out the 30s SMP
  timeout. On the wire that appears as `Remote User Terminated
  Connection` exactly 30.0s after `User Confirmation Request`, with no
  SMP `Pairing Failed` at all — it looks exactly like a product bug.
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
- **Repeated aborted attempts can wedge the stack.** Runs of failed
  pairings have produced `SMP: Pairing Failed, Reason: DHKey check
  failed (0x0b)` on otherwise-valid attempts, cleared by
  `sudo systemctl restart bluetooth`. Rule that out before believing a
  pairing bug reproduces.
- **`default_adapter()` takes the lowest-indexed adapter**, so with two
  radios present shepherdd binds `hci1` regardless of which one you
  meant, and there is no config knob. Downing the other one does not
  redirect it — BlueZ still exposes a downed adapter and registration
  just fails.

## When it fails, get the wire

`btmon` is what separates a real defect from a driving artifact:

```sh
sudo btmon -i hci1 > /tmp/btmon.txt      # while running the flow
grep -aE "SMP:|Reason:|User Confirmation" /tmp/btmon.txt
```

A healthy Numeric Comparison run is Pairing Request/Response → Public Key
×2 → Confirm → Random ×2 → `User Confirmation Request` → DHKey Check →
encryption. Where it stops tells you which side gave up and why.
