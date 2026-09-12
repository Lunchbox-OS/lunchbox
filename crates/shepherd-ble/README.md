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
- `admin` — the TOML-persisted `AdminRecord` **list**, the redacted
  `AdminSummary` an admin sees of the others, the factory-reset sentinel
  check that runs at startup, and `PendingUnbondStore`: the on-disk
  retry list of BlueZ bonds still owed a removal. Un-claiming is two
  steps (clear the record, forget the bond) and only the first is
  atomic, so the second is recorded before it's attempted and cleared
  only once it succeeds.
- `claim` — the admin roster, the enrolment handshake a second phone
  goes through, and the per-request authorization gate.
- `agent` — `bluer` pairing agent for Numeric Comparison. Exposes a
  `PairingDisplay` trait so the daemon can plug in its Sway overlay
  without this crate depending on Wayland.
- `server` — `BleServer` lifecycle: advertising, GATT application
  registration, accept loop, per-client task.

The agent + server modules require a running BlueZ daemon and a real
adapter; everything else is unit-testable in isolation.

## More than one phone (issue #149)

The first phone to reach an unclaimed device becomes its administrator on the
spot, because there is nobody to ask. Every phone after that has to be let in
by one that already is: it bonds, calls `claim`, and gets back a six-digit code
and a pending request that an existing admin approves from their own app. That
is the same shape `shepherd-management`'s `webauth` uses to let a browser in
(issue #156), for the same reason — the number is compared across two screens,
so a phone racing the one you meant carries different digits.

TOFU is right for the first phone and wrong for the rest. A BLE pairing's
anchor is standing in front of the TV, and the person who does that most is the
child the device exists to supervise.

Three consequences worth knowing before changing anything here:

- **`authorize` compares addresses now.** It used to allow any peer that
  reached it, on the reasoning that an encrypt-authenticated write proves a
  MITM-protected bond and v1 only ever had one. The second half stopped being
  true — and was never quite true, since nothing stopped a phone from bonding
  and simply not calling `claim`.
- **A peer has two addresses and the difference matters.** A GATT request names
  its peer by D-Bus object path, which for a phone using privacy is the random
  address the link came up on; `Device1.Address` on the same object is the
  identity address the bond is filed under. Records hold the identity
  (`resolve_peer`), `PeerIdentity::matches` accepts either so records written
  before this keep working, and `device_path_for` translates back whenever a
  bond has to be *removed*, because `remove_device` addresses by path.
- **Turn-taking, not concurrency.** `TransportState` is single-session — one
  frame reader, one response outbox, one events outbox — so a second
  administrator writing at the same time is refused with `InProgress` rather
  than interleaved, and non-owners read empty. In practice a peripheral stops
  advertising while connected, so the radio enforces this before the transport
  ever has to; the rule is what keeps that from being load-bearing. Per-peer
  transport state is a separate issue.
- **The roster is not BLE's.** Listing administrators, approving a waiting
  phone and revoking one are `ManagementService` methods, reached through
  `shepherd_management::AdminRoster`, which `ClaimMachine` implements. They are
  dispatched here like any other RPC rather than special-cased alongside
  `claim` and `factory_reset`.

  That is what lets a browser approve a second phone, and it matters for the
  case the feature is for: the parent holding the device when a new phone asks
  is at least as likely to be at a laptop, and requiring the *other phone*
  would have meant a household could only add a caregiver by finding whoever
  already was one. It also means the roster passes exactly the gate every other
  RPC passes on each transport — `authorize` over BLE, the session middleware
  over HTTP — rather than a second copy of it.

  Worth stating plainly, because it is a real consequence: a web session can
  now enrol a phone, and that phone outlives the session. The web credential
  was already administrator-level — it can set the password and rewrite the
  policy — and revocation is offered on the same screen, but the persistence is
  new.

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

That reasoning is why the roster is a property of the machine rather than of a
kiosk user: a device with two children and two parents has one set of bonds, and
`Adapter::remove_device` forgets a bond for the whole machine either way.

Two simultaneous graphical sessions remain untested: the custodian refuses two
sessions for *one* uid, so two uids with one each satisfies it, and two
`shepherdd`s would then contend for one adapter's GATT registration.
