# BLE management: `connect()` stalls forever draining the events outbox

> Follow-on to: `2026-06-28 001 ble-read-poll-replaces-notify.md`,
> `2026-07-09 001 ble-reconnect-poll-spin.md`,
> `2026-07-18 001 ble-clear-outbox-on-disconnect.md`

## Prompt / report

> What might be causing the BLE management to spin and never connect on
> Android, particularly while heavy activity is running? I typically
> don't see anything logged in the journal during the repro; sometimes
> there's an entry showing the acknowledgement and response a few
> *minutes* after the fact — long after the timeout duration.

Same device pair as the poll-spin investigation: `shepherdd` on
`leibniz`, Pixel 10a running `com.armeafamily.shepherd.companion`.

## TL;DR

The failure isn't in the RPC path — it's in `ShepherdConnection.connect()`,
which could block indefinitely draining the device's events outbox.
`REQUEST_TIMEOUT_MS` (15 s) wraps `call()` only, and nothing wrapped
`connect()`, so a stall there had no bound at all. That's why the symptom
outlives the timeout by minutes: the timeout was never armed for that
phase.

Three properties combined into a hang:

1. `events_forwarder` pushes every `ManagementService` event into the
   events outbox for the daemon's whole lifetime, connected or not, so
   the queue is a backlog reservoir that fills while nobody is polling.
2. `EVENTS_OUTBOX_BYTES` was 256 KiB and `Outbox::push` evicted
   oldest-first rather than coalescing, so under activity churn it sat
   *pinned at capacity* with hundreds of superseded `StateChanged`
   snapshots.
3. `drainAndDiscard` had no timeout, no read cap, and no delay, and its
   exit condition was **two consecutive empty reads**.

Reads are capped at 512 bytes (`GATT_MAX_ATTR_VALUE` — Android's
per-read ceiling, from the read-poll doc) and cost a GATT round trip
each, so the drain runs at roughly 10–20 KiB/s. A full 256 KiB backlog is
13–26 seconds *minimum*. Worse, once events arrived faster than about one
per 60 ms, "two consecutive empties" never happened and the drain never
terminated — `connect()` never returned, `ready` never flipped, both poll
loops stayed parked on `ready.first { it }`, and `runConnectionLoop` sat
inside `connect()` on `LinkStatus.Connecting` without ever reaching its
own retry ladder.

## Why the journal looks dead

Every log on the read path is `debug!` (`outbox_read_characteristic`,
`handle_write`'s chunk-arrival line), and there was **no info-level log
for a peer connecting** — `spawn_device_watcher` only logged
`Connected(false)`. A phone that connects and issues thousands of reads
without ever writing produced literally zero info-level output. At info
level the journal genuinely had nothing in it between "advertising
started" and an RPC that might arrive much later.

The "acknowledgement and response minutes later" artifact is the same
story from the other end: when the event rate finally dipped, the drain
converged, `ready` flipped, `refreshAll()` fired, and `dispatch_frame`
logged `BLE RPC received` / `BLE RPC response queued` — long after the
user had given up.

## Why the earlier investigations didn't catch it

`2026-07-09 001` correctly refuted daemon-side CPU starvation (the HTTP
stand-in answered in ~1 ms under full saturation) and fixed a real
client-side busy-spin in `pollLoop`. But it measured the *RPC* path,
which is exactly the path that was healthy. Its own follow-up section
named both halves of the remaining risk:

> subscribe to device-disconnect and clear the outboxes there, **and/or
> cap the events backlog to the latest snapshot**.

`2026-07-18 001` implemented the first half. The second half is this
document. The disconnect-clear alone doesn't help here: it clears at
drop time, and the forwarder immediately starts refilling while the phone
is away — which is precisely the window before the connect that stalls.

The "heavy activity" correlation is real but was mis-attributed the first
time. It isn't the daemon being starved of CPU; it's the daemon emitting
more events (bigger backlog, faster refill during the drain) while the
radio has less headroom to drain them.

## The fixes

### 1. Bound the drain (`ShepherdConnection.drainAndDiscard`)

`DRAIN_BUDGET_MS` (3 s) and `MAX_DRAIN_READS` (192, ≈96 KiB) per
characteristic. Exceeding either throws the new `DrainStalledException`.

`connect()` catches it, calls `peripheral.disconnect()`, and rethrows —
deliberately *not* wrapped as `LinkUnauthenticatedException`, because the
link is fine and it's the backlog that isn't; conflating them would point
recovery at the bond. Dropping the link is the recovery: the daemon
clears both outboxes on peer disconnect (`disconnect_monitor`), so the
caller's retry starts from an empty queue. Proceeding instead would flip
`ready` on a stream we're stranded mid-frame in and trade a bounded retry
for a framing-error loop.

Read *failures* still propagate unchanged — `withTimeoutOrNull` swallows
only its own timeout — so the `LinkUnauthenticatedException` probe on the
reconnect path behaves as before.

### 2. Put a ceiling on `connect()` (`ShepherdViewModel.connectWithin`)

`CONNECT_TIMEOUT_MS = 20_000`, then `disconnect()` and
`ConnectTimeoutException`, which lands in the existing catch and engages
the 1 s/2 s/5 s backoff ladder. A healthy reconnect is well under two
seconds; this is purely a backstop that converts a hang into a retry.

### 3. Collapse the events backlog (`Outbox::push_coalesced`)

New `push_coalesced(framed, key)`: pushing under a key discards every
*queued* message with the same key. `events_forwarder` pushes
`StateChanged` under `COALESCE_STATE_CHANGED` (decided by the new
`coalesce_key_for`, which is unit-tested so a future snapshot-shaped
variant isn't silently forgotten). Snapshots are idempotent — the newest
makes every older one redundant — so an hour of churn now leaves *one*
snapshot to drain instead of hundreds.

A mid-delivery head is never coalesced away: the client is partway
through those bytes and dropping them would jump its stream forward
mid-frame. Oversize frames are rejected *before* coalescing, so a
too-large snapshot can't supersede the queued good one and then be
dropped itself, leaving neither.

`EVENTS_OUTBOX_BYTES` drops 256 KiB → 64 KiB. It's a latency budget, not
a storage budget: it holds one full snapshot (~20 KiB with a few entries
configured) plus ample room for incremental events, and bounds a
worst-case cold drain to a few seconds.

### 4. Make the stall visible

- `spawn_device_watcher` now logs `Connected(true)` at info with both
  outbox depths — the depth at connect *is* the drain latency the
  companion is about to pay, which is the number you want when this
  recurs.
- `Outbox::read` logs `BLE outbox backlog drained` at info once per
  backlog (bytes + reads) when a continuous drain exceeded 8 KiB. One
  line per drain, not per read.

### 5. Re-pair is offered, not imposed

Previously, exhausting the retry ladder while the device was still
advertising caused `runConnectionLoop` to call
`bondManager.removeBond()` automatically and demand a re-pair. The
reasoning was that reachable-but-unusable proves a one-sided bond — but
it doesn't. A congested 2.4 GHz band, a daemon restart mid-connect, and
(as of this investigation) a drain stall all present identically, and all
of them clear up on their own. Auto-removing the bond turned each into a
mandatory trip to the TV.

Now that path sets the new `LinkStatus.RepairSuggested`, which renders a
banner offering **Retry** and **Re-pair**, and leaves the bond intact.
The bond is removed only by `ShepherdViewModel.dropBondAndRepair()`,
reached when the user taps Re-pair. `LinkStatus.NeedsRepair` is unchanged
and still automatic — there the OS bond is provably gone, so there's no
choice to offer.

## Follow-up: the drain budget broke reconnect-after-restart

> this version is now failing to reconnect after the app is restarted
> after the initial pair, regardless of load

Self-inflicted, and the "regardless of load" is the tell: the fault is on
the path that has nothing to do with backlog.

**Reconnecting to a bonded peer produces a GATT link that is not yet
encrypted.** Android establishes encryption from the stored LTK *lazily*,
triggered by the first ATT request that needs it — which is
`drainAndDiscard`'s very first read. That read therefore behaves nothing
like the ones after it: it can take seconds while the handshake runs, and
it can fail outright with insufficient authentication/encryption and
succeed on the immediate retry, all with a perfectly healthy bond.

The initial `DRAIN_BUDGET_MS = 3_000` started its clock on that read. So
a first read that was merely slow tripped the budget and raised
`DrainStalledException` → disconnect → retry → same thing, forever; and
one that failed raised `LinkUnauthenticatedException`, pointing recovery
at the bond. The old unbounded loop simply waited however long encryption
took and carried on, which is why this only appeared now.

Why it showed up specifically after the initial pair: during pairing the
link is established pre-bond and bonded *in place*, then `adopt()` reuses
that already-encrypted connection. The first app restart is the first
time the app ever makes a *fresh* connection to an already-bonded peer —
the only situation that exercises lazy encryption setup.

The fix separates the two concerns. `readAfterSettle` performs the
session's first read on each characteristic under its own
`LINK_SETTLE_TIMEOUT_MS` (5 s) with `LINK_SETTLE_ATTEMPTS` (3) retries;
`DRAIN_BUDGET_MS` (raised to 5 s) is measured from after that read lands,
so it only ever bounds steady-state draining. `CONNECT_TIMEOUT_MS` goes
to 30 s — above the sum of connect()'s internal budgets, so a stall
surfaces as the specific failure that caused it instead of being masked
by the outer cap.

Two general lessons worth keeping:

- **A timeout is only correct over a homogeneous operation.** "Read the
  characteristic" is two different operations here — warm the link, then
  drain the queue — with budgets that differ by an order of magnitude.
  One clock over both was always going to misfire.
- **Removing an automatic recovery exposes everything it was hiding.**
  The old give-up path removed the bond and forced a re-pair, which would
  have papered over exactly this failure (re-pair → pairing path → link
  encrypted in place → works). Making re-pair optional was right, but it
  means genuine link faults now have to be *fixed* rather than
  unbonded-around. The `LinkStatus.Disconnected` banner grew a Re-pair
  button for the same reason: with nothing auto-unbonding, the user needs
  a way out of a one-sided bond even when the scan probe can't run.

## Follow-up: the two stale-bond gaps behind all of this

Both are the same shape — one side forgets a bond and the other doesn't —
and both were previously survivable only because the give-up path
auto-unbonded.

### Bond removals were promised, not kept

`BleServer::new` loads the admin record before clearing it and hands the
address to `run`, which calls `adapter.remove_device`. That part works.
What didn't: the removal was best-effort in memory, under a comment
saying "we tolerate failures: the next restart will retry". **It
wouldn't.** By then the admin record was already cleared and the sentinel
already consumed, so nothing on the next boot knew which address to
forget. A failed removal — adapter not ready, BlueZ hiccup, daemon killed
between the two steps — stranded the peer bonded to a device with no
admin: every reconnect accepted at the link layer, rejected with
`not_claimed`, and unfixable by re-pairing because the bond already
exists.

(An earlier note in `2026-07-18 001` described this as
`persisted_admin_for_unbond` "still a stub returning `None`". No such
function exists in the tree; the load-before-clear has been there for a
while. The durability of the *removal*, not the capture, was the actual
hole.)

New `PendingUnbondStore` in `admin.rs` is a TOML list of addresses beside
the admin record. An entry is written *before* the removal is attempted
and deleted only once BlueZ confirms the peer is gone, so any failure is
retried at the next startup. `drain_pending_unbonds` runs once in `run`
and covers both producers — the sentinel reset and the `factory_reset`
RPC. A peer BlueZ no longer knows counts as settled, since
`remove_device` errors on an unknown address and treating that as failure
would keep the entry and its warning forever.

### `ensureBonded` trusted a bond it hadn't verified

`BOND_BONDED` on the phone says nothing about whether the *device* still
has its half. After a factory reset the device calls BlueZ
`remove_device` while Android keeps listing the peer as bonded — and
because `ensureBonded` short-circuits on that, re-pairing never started a
fresh bond. The user would re-pair, the app would skip straight to
`claim` over a link that couldn't encrypt, and pairing failed citing
something unrelated. `BondManager.removeBond`'s own doc comment described
this exact trap; nothing acted on it.

The pairing flow now calls `ensureFreshBond`, which drops any existing
bond (waiting for `BOND_NONE` via `awaitUnbonded`, since `createBond` is
rejected while the old one tears down) before bonding. Safe because we
only reach it when the user is deliberately pairing *and* the device has
already reported itself Unclaimed — an Unclaimed device has no admin, so
any bond we're holding is stale or about to be superseded. If the
platform blocks the reflection-based `removeBond` we proceed with the
existing bond rather than dead-ending.

`ensureBonded` stays for callers that genuinely just want "bonded, don't
care how", with a doc note pointing at the distinction.

## Tests / checks

- New `outbox` unit tests: coalescing supersedes same-key messages
  without reordering survivors, spares a mid-delivery head, and keeps the
  queue at one message across 500 snapshot pushes.
- New `server` unit test `only_whole_state_snapshots_coalesce`.
- `cargo test -p shepherd-ble` — 49 passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all` — applied.
- `:app:compileDebugKotlin`, `:app:testDebugUnitTest` — pass.

The client-side drain and connect budgets aren't unit-testable without a
fake Kable `Peripheral` (the constructor is private and takes a real
one), so they're on the manual on-device smoke test alongside the rest of
the GATT lifecycle.

## Still open

- **On-device validation of the actual repro** is not done: reconnect
  after a long heavy-activity session, with `journalctl` tailing for the
  new `BLE peer connected` line and its outbox depths. That line landing
  with a small depth is the confirmation; a large depth followed by
  `BLE outbox backlog drained` says the coalescing isn't catching a
  producer it should.
- **The response outbox is still 256 KiB and uncoalesced.** That's
  correct — responses are emitted at the rate the companion requests
  them, so it doesn't accumulate the same way — but it's the same
  unbounded-backlog shape if a future path ever queues responses without
  a reader.
- **Two pollers plus writes share one GATT operation queue.** Android
  serializes per connection, so a long events drain competes with the
  response poller and with `call()`'s writes for the whole link. With the
  backlog bounded this is now a small effect; if RPC latency under event
  churn ever becomes the complaint, this is where to look next.
- The bigger structural point from `2026-06-28 001` stands: read-poll
  needs continuous bidirectional traffic to work at all, which makes it
  sensitive to exactly the radio conditions a heavy activity creates.
  Nothing here changes that.
