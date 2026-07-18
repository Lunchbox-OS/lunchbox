# BLE: actually remove the BlueZ bond on factory reset

> Follow-on to: `2026-06-20 002 ble-management.md` (design: reset calls
> BlueZ `RemoveDevice`), surfaced by the "why do previously-paired admin
> sessions fail to connect" investigation.

## The bug

A factory reset was only *half* a reset: it cleared the app-layer admin
record but left the link-layer BlueZ bond in place. Both reset entry
points were affected.

- **Sentinel path.** `BleServer::new` consumes the reset sentinel and
  clears the admin record, but the bond removal in `run` called
  `persisted_admin_for_unbond`, a stub that **always returned `None`**.
  So the bond was never removed.
- **RPC path.** `handle_factory_reset_rpc` called
  `ClaimMachine::factory_reset` (which clears the record) and discarded
  the returned previous record — no bond removal at all.

Consequences of a bond that outlives its admin record: the phone is
still bonded, so it reconnects and the link comes up
encrypted-authenticated — but the device is now `Unclaimed`, so every
RPC is rejected with `not_claimed`. And because the bond already exists,
re-pairing doesn't fire, so the phone can't recover on its own. From the
user's side this is a previously-paired admin session that "connects but
is dead," exactly the failure class the investigation was chasing.

## The fix

Both reset paths now hand the previously-bonded peer's identity address
to `Adapter::remove_device`.

### Sentinel path

- `BleServer` gains a `pending_unbond: Option<AdminRecord>` field.
- `new` loads the admin record *before* `store.clear()` and stashes it
  in `pending_unbond` (best-effort: a read error is logged, not fatal).
- `run` takes `self.pending_unbond` and, if present, parses the identity
  address and calls `adapter.remove_device`. The dead
  `persisted_admin_for_unbond` stub is removed.

### RPC path

The RPC handler runs deep in the GATT write path with no adapter access,
so it can't call `remove_device` directly. Instead:

- `run` creates an `mpsc::channel::<Address>` and spawns a small
  long-lived task (aborted on shutdown, like the events forwarder) that
  owns a clone of the adapter and calls `remove_device` for each address
  it receives.
- The sender is threaded through
  `build_application → request_characteristic → handle_write →
  dispatch_frame → handle_factory_reset_rpc`.
- On a real reset (`factory_reset` returned `Some(previous)`),
  `request_unbond` parses `previous.identity_address` and sends it. The
  success response is queued *before* the send, but `remove_device`
  disconnects the peer, so delivery of the ok is best-effort — which is
  fine, a factory reset ends the session anyway. An unclaimed-device
  reset is a no-op and queues nothing.

## Scope / relationship to the other BLE fix

This ships alongside `2026-07-18 001 ble-clear-outbox-on-disconnect.md`
(a different reconnect bug). The two were developed on separate branches
and then combined onto `u/albert/ble-reconnect-fixes` as two commits
(outbox-on-disconnect first, this one second). They overlap in
`server.rs`'s `run` / `build_application` / `request_characteristic` /
`handle_write` / `dispatch_frame` — one adds `reader`/`last_peer` + a
disconnect-monitor task, this one adds `unbond_tx` + an unbond task — and
the overlapping signatures were reconciled when combining (both sets of
params/tasks coexist).

## Tests / checks

New unit tests in `crates/shepherd-ble/src/server.rs`:

- `factory_reset_requests_bond_removal` — a claimed device's
  `factory_reset` RPC goes Unclaimed, queues the peer address on the
  unbond channel, and still returns a success response.
- `factory_reset_when_unclaimed_requests_no_removal` — an unclaimed
  reset is a no-op success and queues nothing.
- `sentinel_reset_captures_bond_for_removal` — `new` with the sentinel
  present clears the on-disk record and captures the previous record in
  `pending_unbond`.

- `cargo test -p shepherd-ble` — 43 passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.

The `run`-side BlueZ `remove_device` calls need a real adapter and are
exercised by the manual on-device smoke test, not the unit suite.

## Not addressed here

The reset still requires shell access to drop the sentinel (or the app
to issue the RPC); the design doc's "hardware-button reset" remains
future work. Multi-admin bond identification (tracking the IRK rather
than the resolved address) is likewise still deferred.
