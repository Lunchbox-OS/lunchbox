# shepherd-ble

Bluetooth LE management transport for shepherdd.

This crate exposes `BleServer`, the BLE counterpart to `shepherd-http`'s
`HttpServer`. Both serve the same `shepherd_management::ManagementService`
trait, just over different transports: BLE is designed as the primary
admin path because it works without IP autodiscovery, static IP, or any
network configuration.

See `docs/ai/history/2026-06-20 002 ble-management.md` for the full
design — Numeric Comparison pairing, TOFU single-admin claim model,
unified HTTP+BLE bearer-token identity, filesystem reset sentinel.

## Module map

- `protocol` — GATT service/characteristic UUIDs, JSON-RPC envelope and
  error-code schema. Pure data, no I/O.
- `framing` — length-prefix encoding + chunked reassembly for ATT
  writes/notifies larger than the negotiated MTU.
- `outbox` — the byte queue behind the read-poll Response and Events
  characteristics. Its depth is connect latency, not just memory: the
  companion drains both outboxes to empty inside `connect()` at 512
  bytes per GATT round trip, so `StateChanged` snapshots are pushed
  coalesced and the capacity is kept tight.
- `rpc` — request dispatcher that maps RPC method names onto
  `ManagementService` trait calls.
- `admin` — TOML-persisted `AdminRecord`, the factory-reset sentinel
  check that runs at startup, and `PendingUnbondStore`: the on-disk
  retry list of BlueZ bonds still owed a removal. Un-claiming is two
  steps (clear the record, forget the bond) and only the first is
  atomic, so the second is recorded before it's attempted and cleared
  only once it succeeds.
- `claim` — `Unclaimed → Claimed` state machine and the per-request
  authorization gate.
- `agent` — `bluer` pairing agent for Numeric Comparison. Exposes a
  `PairingDisplay` trait so the daemon can plug in its Sway overlay
  without this crate depending on Wayland.
- `server` — `BleServer` lifecycle: advertising, GATT application
  registration, accept loop, per-client task.

The agent + server modules require a running BlueZ daemon and a real
adapter; everything else is unit-testable in isolation.

## The claim is per-user; the bond is not

Worth knowing before reasoning about a device with more than one kiosk user,
because the two halves of "paired" live at different scopes.

Since issue #157 the admin record is held by the state custodian, which is
templated per user — `shepherd-stated@<user>`, its own
`/var/lib/shepherdd/state/<user>/`, its own `admin.toml`. So the *claim*, and
the HTTP bearer token minted with it, belong to one kiosk user.

The **BlueZ bond does not**. It lives under `/var/lib/bluetooth/<adapter>/`, is
owned by root, and there is one adapter. `Adapter::remove_device` forgets a bond
for the machine.

Three consequences follow, none of them designed — they fall out of the two
scopes rather than being chosen:

- **A phone claimed for one user arrives at the next already bonded.** The
  second user's record is absent, so their device reads `Unclaimed`, and the
  phone — bonded at the link layer — can claim it too. One phone administering
  two children is plausibly what an operator wants, but nothing here decides it.
- **A factory reset for one user unpairs the phone from all of them.** The
  sentinel is per-user and clears that user's record, but the queued
  `remove_device` takes the system bond with it. The other user's record still
  says claimed, with no bond behind it — the mirror of the lockout
  `PendingUnbondStore` exists to prevent, across a boundary the rest of the
  design keeps.
- **`authorize` gates on the claim, and any bonded peer passes it.** See the
  comment on `ClaimMachine::authorize`: an address that does not match the
  record is logged and allowed under the v1 single-admin policy. With one bond
  table and several records, "any bonded peer" is a wider set than it reads as.

Untested, and stated here from the code rather than from a device: nothing in
the tree sets up a second kiosk user. Two simultaneous graphical sessions are a
further open question — the custodian refuses two sessions for *one* uid, so two
uids with one each satisfies it, and two `shepherdd`s would then contend for one
adapter's GATT registration.

This is not issue #149, which is multiple *phones* per device. They meet at the
same fix, though: identify a peer by its resolved identity or IRK rather than by
whichever address BlueZ surfaced, which is what `authorize`'s comment already
says multi-admin will need.
