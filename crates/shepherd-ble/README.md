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

## The claim is the device's, not a user's

The admin record, the unbond queue and the factory-reset sentinel are held by
the state custodian in `/var/lib/shepherdd/admin/` — one directory for the
machine, shared by every kiosk user on it. The policy and the usage database are
per-user, under `/var/lib/shepherdd/state/<user>/`, because those are facts
about a child.

The split follows the thing being described. A BlueZ bond lives in
`/var/lib/bluetooth` at one adapter, is owned by root, and
`Adapter::remove_device` forgets it for the whole machine. A claim scoped more
narrowly than the bond it names cannot be kept honest: an earlier draft of issue
#157 put the record under the kiosk user, and a two-child device then behaved in
ways nobody had chosen — a phone claimed for one user arriving at the next
already bonded and able to claim it too, and a factory reset for one user
silently unpairing the phone from the others.

`ProtectedFile::scope` is where this is written down, and
`LocalProtectedFiles::scoped` is what routes each file to the right root. A
device *without* the custodian has one directory and no protected root to share,
so both scopes land in the kiosk user's home — which is what a dev stack and the
tests get, and what a pre-custodian device always had.

One thing this does not fix: `authorize` gates on the claim, and any bonded peer
passes it. See the comment on `ClaimMachine::authorize` — an address that does
not match the record is logged and allowed under the v1 single-admin policy.
Issue #149 (multiple companion bonds) is where that gets its answer, and it
wants the resolved identity or the IRK rather than whichever address BlueZ
surfaced.

Two simultaneous graphical sessions remain untested: the custodian refuses two
sessions for *one* uid, so two uids with one each satisfies it, and two
`shepherdd`s would then contend for one adapter's GATT registration.
