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
