# BLE management advertising fails to register (companion can't pair)

## Symptom

The companion Android app could not pair with a shepherd device. It
"just never found it" — no error, the device simply never appeared in
the pairing list.

## Root cause (the real one): kernel extended-advertising regression

The device journal showed shepherd's advertisement being rejected:

```
bluetoothd: src/advertising.c:add_client_complete() Failed to add advertisement: Invalid Parameters (0x0d)
```

`btmon` pinned it precisely. bluetoothd registers advertisements through
the kernel's **extended** advertising MGMT API, and the kernel rejects
the data:

```
@ MGMT Command: Add Extended Advertising Data (0x0055)
      Advertising data length: 3        (just the flags AD, 02 01 06)
@ MGMT Event: Command Status
      Add Extended Advertising Data (0x0055)
        Status: Invalid Parameters (0x0d)
```

The advertising data is trivially valid (3 bytes, well under the
"Available adv data len: 31" the controller reported one line earlier),
yet the kernel refuses it. The **legacy** MGMT path works on the same
controllers:

```
$ sudo btmgmt add-adv -c 1
      Add Advertising (0x003e) … Status: Success (0x00)
      Own address type: Public (0x00)
```

Reproduced identically on two devices with different controllers — an
Intel 8265 (Bluetooth 4.2) and a Bluetooth 5 controller (20 advertising
instances) — both on Ubuntu 26.04's `7.0.0-28-generic` kernel. Because
it fails on a genuinely extended-advertising-capable controller too, it
is **the kernel's MGMT `Add Extended Advertising Data` handler**, not a
controller/firmware quirk. bluetoothd always prefers the extended MGMT
commands when the kernel exposes them, and there is no BlueZ config to
force the legacy path — so *all* D-Bus advertising is broken on this
kernel, shepherd included. Nothing shepherd (or `bluer`) does can route
around it.

This is a known upstream regression, not shepherd-specific:
[raspberrypi/linux#7473](https://github.com/raspberrypi/linux/issues/7473)
reports the identical signature as a **mainline 6.18** regression (works
on 6.12), a length-accounting bug in the `Add Extended Advertising Data`
mgmt path. On Ubuntu 26.04 the `dpkg` install log brackets it precisely:
`7.0.0-27.27` (installed 2026-06-27) advertises fine, `7.0.0-28.28`
(2026-07-17) is broken — confirmed by booting back to `-27`. The fleet
fix is to pin `7.0.0-27` until Ubuntu ships a corrected kernel; see
<docs/INSTALL.md> "BLE management doesn't advertise" for the exact
pin/hold/GRUB commands.

Verify on any device: `sudo btmgmt add-adv -c 1` (legacy) should
succeed while `bluetoothctl advertise peripheral` (bluetoothd's extended
path) fails with `0x0d`. See <docs/INSTALL.md> "BLE management doesn't
advertise" for the operator-facing version and remediation.

## Dead ends ruled out along the way

- **LL Privacy** (`ll-privacy` in `btmgmt info` current settings): a red
  herring. The working `btmgmt add-adv` path used a Public address and
  succeeded with LL Privacy on; it cannot be disabled via bluetoothd
  `Experimental`/`KernelExperimental` on this kernel anyway.
- **Advertising-instance exhaustion**: ruled out — `SupportedInstances
  20`, `ActiveInstances 0`.
- **Advertisement payload > 31 bytes**: plausible but not the cause here
  — a 3-byte advertisement fails too. See the name-trim below, which is
  a real but *separate* hardening.

## Separate hardening shipped: advertised-name trim

While diagnosing, we found the advertised name could independently
overflow the 31-byte legacy PDU: `device_name` defaults to the system
hostname, and Flags (3) + 128-bit UUID (18) + local name (2 + len) can
exceed 31, which *would* also yield `0x0d` — host-dependent on hostname
length. `crates/shepherd-ble/src/server.rs` now trims the advertised
name to `MAX_ADV_NAME_BYTES` (= 31 − 3 − 18 − 2 = 8) on a UTF-8 boundary
with a trailing ellipsis (`copernicus` → `coper…`) and `warn!`s when it
does. This is defensive hardening (the 8-byte `"shepherd"` default sits
right at the edge); it is **not** what fixed the pairing failure above.

## What actually unblocks pairing

A kernel without the extended-advertising regression. On the affected
devices, pinning `7.0.0-27-generic` (see <docs/INSTALL.md>) restores
advertising — confirmed by re-testing `bluetoothctl advertise
peripheral` after booting `-27`. Unpin once Ubuntu ships a fixed kernel;
track it via the Ubuntu bug
[LP #2161852](https://bugs.launchpad.net/ubuntu/+source/linux/+bug/2161852)
(regression bracket `7.0.0-27.27` → `7.0.0-28.28`, cross-referencing
raspberrypi/linux#7473 and the `btmon` traces). A different Bluetooth
adapter does **not** help: the BT5 controller fails too.
