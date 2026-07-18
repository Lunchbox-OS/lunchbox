# BLE: clear transport session state on peer disconnect

> Follow-on to: `2026-06-28 001 ble-read-poll-replaces-notify.md`,
> `2026-07-09 001 ble-reconnect-poll-spin.md`

## Prompt

While enumerating "what else could cause a previously-paired BLE admin
session to fail to connect" (beyond the already-fixed client-side
poll-spin), the read-poll transport's outbox handling stood out: the
server never clears the outboxes on disconnect. This implements that
fix.

## Background — the gap

The Response/Events transport is read-poll: each is an [`Outbox`] byte
queue the companion drains with GATT reads (see the read-poll doc for
why notify was abandoned). Both the outboxes and the Request-side
`FrameReader` carry a *per-session* byte stream — a length-prefixed
sequence the client's reassembler tracks position in.

The **only** reset of that state was the client's `id == 1` sentinel in
`dispatch_frame` (server.rs), which wipes both outboxes at the start of
a fresh session. A companion sends `id == 1` only when it builds a
*brand-new* `ShepherdConnection`.

The hole: on a **transient BLE drop** — BT toggle, brief out-of-range —
the companion keeps the *same* `ShepherdConnection` (the
`runConnectionLoop` path) and resumes its RPC id counter mid-sequence
(…5, 6, 7…). No `id == 1` is ever sent, so nothing wipes the queues.
Any bytes left from before the drop — an unread response, a half-written
request frame, or events that piled up while nothing was polling — stay
in place and desync the reassembler on the reused link. Symptom:
reconnect "succeeds" but RPCs return corrupt/garbage frames and time
out. This was flagged as open defense-in-depth work in the poll-spin
doc (`2026-07-09 001`, "Follow-up").

## The fix

Subscribe to BlueZ device-disconnect and wipe all per-session transport
state when a peer drops. New in `crates/shepherd-ble/src/server.rs`:

- **`disconnect_monitor`** — a long-lived task (spawned in `run`
  alongside the events forwarder, aborted on shutdown) that watches
  `adapter.events()` for `DeviceAdded`/`DeviceRemoved` and attaches a
  per-device watcher to every device BlueZ knows about, including those
  already bonded at startup (`adapter.device_addresses()`), since a
  persistent bond won't arrive as a later `DeviceAdded`.
- **`spawn_device_watcher`** — per-device task on `device.events()`;
  on `DeviceProperty::Connected(false)` it calls the reset. The stream
  lives as long as the device object exists, spanning many
  connect/disconnect cycles for a bonded peer.
- **`reset_transport_session`** — clears both outboxes
  (`Outbox::clear`, which already existed and already handles a
  mid-delivery head), resets the `FrameReader`, and clears the
  `last_peer` marker.

To share the reader/`last_peer` with the monitor, they're now created in
`run` and passed into both `request_characteristic` (via
`build_application`) and `disconnect_monitor`, rather than being created
inside the characteristic closure.

### Why clearing on *any* disconnect is safe

v1 is single-admin, single-connection (TOFU; subsequent claims are
rejected). There is never a second live session whose in-flight bytes a
wipe could disturb, so we don't need to identify *which* peer dropped.

### Why it doesn't race a fresh response into oblivion

`Connected(false)` is emitted at drop time. On the reused-connection
path the next write can't land until the link is back
(`Connected(true)`, delivered strictly after `false` on the same D-Bus
object) and encryption re-established — hundreds of ms later. The wipe
therefore lands well before any post-reconnect response is queued.

## What this does and doesn't cover

- Covers the reused-connection transient-reconnect path (the actual
  gap) *and* hardens the fresh-connection path (belt-and-suspenders on
  top of `id == 1`).
- Does **not** touch the separate BlueZ-bond / encryption failure modes
  (asymmetric bond, half-completed sentinel reset that clears the admin
  record but leaves the bond — `persisted_admin_for_unbond` is still a
  stub returning `None`). Those are tracked separately; this change is
  scoped to the outbox/reader stale-bytes bug.

## Tests / checks

- New unit test `reset_transport_session_wipes_all_session_state`:
  seeds both outboxes with stale frames and the reader with a partial
  frame head, resets, and asserts everything is cleared and a
  subsequently-pushed full frame parses as itself (no stitched prefix).
- `cargo test -p shepherd-ble` — 41 passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.

The GATT/advertising/bonding lifecycle (including the disconnect event
plumbing itself) still needs a real adapter and is exercised by the
manual on-device smoke test, not the unit suite — see the
`reference_ble_ondevice_testing` notes (BT-toggle while foregrounded
forces exactly this reused-connection reconnect path).
