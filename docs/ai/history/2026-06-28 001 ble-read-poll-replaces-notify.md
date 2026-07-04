# BLE management: replace notify with read-poll outbox (#65)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/65>
> Follow-on to: `2026-06-20 002 ble-management.md`,
> `2026-06-21 001 ble-companion-android-spec.md`,
> `2026-06-21 002 ble-companion-android-implementation.md`

## The symptom

The companion paired and worked on first install, but every subsequent
open of the app left the device screen blank with the text "No
activities yet." and (after 15 seconds) a "fetch timed out" toast.
Force-stop + reopen reproduced it reliably; reinstalling the APK
masked it briefly before it came back. The original ticket spelled
this out as "the list of configured activities doesn't appear on
subsequent app opens — I have to manually 'reload config' from the
settings first".

## Prior repair attempts on this branch

Several earlier commits on `u/albert/65/ble-management` chipped at
adjacent failure modes before this investigation:

- `7141458` switched Response from a one-shot mpsc to a `broadcast`
  channel and started pushing an initial `StateChanged` snapshot on
  every Events subscribe, so a reopened companion could populate
  without explicitly calling `service_state`.
- `30a278f` made the companion's `MutableSharedFlow<Event>` have
  `replay = 1` so the late-attaching collector caught the snapshot.
- `f72b5fa` raised the framing cap to `u16::MAX` (a single
  `StateChanged` with a few entries comfortably exceeded 16 KiB) and
  turned framing errors into disconnects instead of crashes.
- `3604c1d` raced `notifier.stopped()` against the broadcast `recv`
  inside the notify task so the task actually exited on peer
  disconnect.

These cleaned up real bugs but the core "list blank on reopen"
symptom persisted, so we went after the root cause.

## Diagnosis: why notify was unfixable here

The ground truth, established by tailing both `journalctl` on the
kiosk and `adb logcat` on a Pixel 10a:

1. **Device side does receive the RPC.** `BLE RPC received
   method=service_state` appears for every reopen.
2. **Device side does emit the notification.** `BLE Response notify
   sending payload payload_len=1108 chunk_size=20` appears once per
   request.
3. **The phone never sees the bytes.** No `onCharacteristicChanged`
   on the Response handle after the very first session.
4. **Subsequent reopens never re-arm the server's notify task.** The
   `BLE Response notify subscribed` log fires exactly once per
   shepherdd lifetime — at the first pair — and never again, despite
   the phone reconnecting and calling Kable's `observe()` (which
   internally does `setCharacteristicNotification` + a CCCD write).

The third and fourth bullets pin the failure to the
notify-subscription machinery. Concretely:

- **bluer's `notify_fn` callback fires on BlueZ's `StartNotify`
  D-Bus method.** BlueZ only calls `StartNotify` on a fresh CCCD
  write that changes the value — not on every subscribe.
- **For bonded clients, BlueZ caches the CCCD value at the bond
  level.** Subsequent CCCD writes from the same client that don't
  change the value are short-circuited before they reach the GATT
  server.
- **Android's stack short-circuits the matching CCCD write.** When
  a bonded peer re-subscribes via `setCharacteristicNotification`,
  Android's BLE stack often skips the descriptor write entirely
  because the bond's cached CCCD is already "enabled".

The result is a one-shot notify task that lives forever as the only
subscriber to the broadcast channel. Its `notifier.notify()`
emissions are PropertiesChanged D-Bus signals on the characteristic
path — they only reach clients whose CCCD is enabled *in BlueZ's
per-connection view*. Subsequent BLE connections from the same
bonded phone don't have that, so the responses go nowhere.

## Dead-end attempt: CCCD bounce

The first hypothesis was that writing CCCD `0x0000` (disable) →
`0x0001` (enable) from the companion on every connect would force
BlueZ through a real state transition, firing `StopNotify` +
`StartNotify` on the server. We tried it with
`com.juul.kable.LazyDescriptor.write` over `BLUETOOTH_GATT_CCCD_UUID`.

It didn't work. The device's `BLE Response notify subscribed` log
sometimes fired but `BLE Response notify session ended` followed
within milliseconds, and the actual response bytes still never
reached the phone. Either:

- The two writes were racing on the wire and BlueZ collapsed them,
- Kable's own `observe()` was issuing competing writes on a
  different code path, or
- BlueZ's per-connection CCCD bookkeeping was confused by the
  back-to-back changes.

We could probably have unwound this by going to raw `BluetoothGatt`
instead of Kable, but at that point notify-based transport was
costing more than it was worth — every fix discovered a new edge.

## The actual fix: read-poll outbox

The simpler design: drop notify entirely, make Response and Events
**read** characteristics, have the companion poll them. The
server-side state machine that was broken (CCCD ↔ StartNotify ↔
notify task lifecycle ↔ per-connection PropertiesChanged routing)
isn't needed at all when reads do the same job.

### Server: `Outbox`

New `crates/shepherd-ble/src/outbox.rs` holds a FIFO of
length-prefixed framed messages plus a `head_offset` tracking how
much of the front message has been drained by reads. Each
`outbox.read(max_chunk)` slices up to `max_chunk` bytes from the
head, advances the offset, and pops the front message when it's
exhausted. Crucially **frames never tear across reads** — bytes are
delivered in order, byte-for-byte, with the client's existing
length-prefix reassembler handling boundaries.

Push semantics evict oldest *whole* messages when over capacity but
refuse to evict an in-progress head (popping mid-frame would jump the
client's byte stream forward and desync the assembler).

### Server: read characteristics

`outbox_read_characteristic` wires a `CharacteristicRead` with
`encrypt_authenticated_read: true` whose handler drains from the
outbox. The Response and Events characteristic UUIDs are unchanged
(wire-compatible with the old layout), only the `read`/`notify`
flags flip. A long-lived `events_forwarder` task subscribes to
`ManagementService::subscribe_events` for the entire server
lifetime and pushes each event onto the events outbox.

### Client: poll loops

`ShepherdConnection.start()` launches two coroutines that poll the
Response and Events characteristics. Adaptive backoff:
`INITIAL_POLL_DELAY_MS=25` after data, doubling on empty reads up
to `MAX_POLL_DELAY_MS=300`. A `Channel<Unit>(Channel.CONFLATED)`
lets `call()` short-circuit the response poller's backoff sleep
the instant an RPC is dispatched, so latency is sub-50ms for the
common case.

`connect()` itself is the synchronisation point: it does the
BLE connect, drains any stale bytes the server may have buffered,
and only then flips a `ready: MutableStateFlow<Boolean>` that the
pollers gate on. Without that gate, the pollers were racing the
very first RPC's response — drain consumed the response bytes
intended for the user's `service_state` call.

## Two follow-on bugs caught during validation

The above fixed "list blank on reopen" *almost*. Two further bugs
surfaced under live hardware testing:

### Bug 1: 5-second session-boundary heuristic was wrong

To prevent stale bytes from a previous BLE session corrupting the
next response, the server initially used a write-gap heuristic: a
write arriving more than 5 s after the previous one was treated as
a new session and triggered an outbox wipe.

Live log analysis showed why this is wrong:

```
00:16:18.730 BLE RPC received id=2 method=service_state
00:16:18.731 BLE RPC response queued id=2 (1108B in outbox)
00:16:30.140 New session detected; clearing outboxes ← wiped mid-delivery
00:16:30.140 BLE RPC received id=3 method=get_volume
```

The 11.4 s gap between `service_state` (id=2) and `get_volume`
(id=3) is exactly the companion's 15-second per-RPC timeout firing
because the phone wasn't reading fast enough. The "session
boundary" wipe then ate the response the phone was *currently*
draining, finishing the death spiral.

The fix is deterministic instead of heuristic: every
`ShepherdConnection` on the companion starts its RPC id counter at
1, so `id == 1` is an unambiguous "first RPC of a fresh BLE
session" marker. `dispatch_frame` checks the parsed request id
before queueing the response and wipes the outboxes only on id=1.

### Bug 2: Android silently truncates reads to 512 bytes

With the session-boundary fix in, the next reopen still corrupted
the frame. Server-side per-read logging revealed the smoking gun:

```
01:23:17.674 BLE outbox read len=516   (server returned)
01:23:16.707 response read returned 512B   (phone received)
```

4 bytes per read silently disappeared between the server and the
companion. With MTU 517, an ATT read response can legitimately
carry 516 bytes on the wire. But Android's GATT stack enforces the
spec's `GATT_MAX_ATTRIBUTE_VALUE = 512` bytes ceiling on a single
`BluetoothGatt.readCharacteristic` delivery — bytes 513–516 of every
read were dropped in the OS, not on the wire.

The 4-byte-per-chunk loss meant the phone's reassembler:

1. Got `512 + 512 + 115 = 1139` bytes from what should have been a
   `516 + 516 + 115 = 1147`-byte response.
2. Read the first two bytes as the frame length prefix (1145).
3. Waited for the missing 6 bytes.
4. Eventually read 177 bytes from the *next* response (`get_volume`).
5. Emitted a Frankenstein 1145-byte frame stitched from
   `service_state` payload + 6 bytes of `get_volume` JSON.
6. Failed to parse the resulting garbage as valid JSON.

The fix is a `GATT_MAX_ATTR_VALUE: usize = 512` cap on the
server-side `max_chunk`. The outbox now hands out at most 512
bytes per read regardless of negotiated MTU.

## Tangential fix: authorize() over-strict

`ClaimMachine::authorize` was rejecting every reconnect because
BlueZ reports the post-bond *resolved identity address* on the
write request, but at claim time during pairing it presented the
random private address still in use mid-handshake. Those never
match, so the strict address comparison broke every reopen.

Relaxed for the v1 single-admin policy: if the device is claimed,
any peer that managed to reach us over the encrypted-authenticated
GATT link is the admin. The link itself is the security boundary,
and TOFU guarantees there's only one bond. Address-drift is logged
at debug level so it remains visible. When multi-admin lands the
right answer is to track the IRK or the resolved identity in the
admin record.

## What the new transport looks like end-to-end

```
phone ────write────▶ Request char (chunked, length-prefixed)
                          │
                          ▼
                   handle_write (id==1? clear outboxes)
                          │
                          ▼
                   dispatch_frame ──▶ push_response ──▶ Response outbox
                                                              ▲
phone ◀───read──── Response char ◀── outbox.read(≤512) ───────┘

         (separate, identical path for Events)
```

No CCCDs. No notify tasks. No per-connection routing state. The
companion polls every 25 ms after a successful read and backs off
to 300 ms when idle; `call()` wakes the response poller via a
conflated channel so latency stays sub-50 ms.

## What landed (commit `e7ba977`)

- `crates/shepherd-ble/src/outbox.rs` — new `Outbox` helper +
  unit tests.
- `crates/shepherd-ble/src/server.rs` — Response and Events become
  read characteristics, long-lived events forwarder task,
  id==1-driven session-boundary clear, 512-byte read cap.
- `crates/shepherd-ble/src/framing.rs` — factored `encode_frame`
  out of `chunk_payload` so the outbox stores already-framed
  bytes.
- `crates/shepherd-ble/src/claim.rs` — relaxed `authorize` for
  v1 single-admin (encrypted link is the boundary; address
  comparison broke on BlueZ identity drift).
- `companion-android/.../ble/ShepherdConnection.kt` — replaced
  Kable `observe()` collectors with poll loops, added `ready`
  gate + `responseWake` conflated channel, drain-on-connect.

## What we'd do differently next time

- **Skip the CCCD bounce.** Easy in hindsight: it was treating the
  symptom (notify subscriptions not re-arming) instead of the
  problem (BLE notify is fragile for bonded reconnects with this
  particular Android/BlueZ stack pair). One round of "what is the
  notify path actually buying us?" would have led to read-poll
  immediately.
- **Add server-side per-read logging from day one.** The
  Android-truncates-at-512 bug took an hour of pattern matching
  against phone-side logs alone. The moment server-side reads
  showed `len=516` while phone-side showed `512B`, the cause was
  obvious.
- **Don't use a write-gap heuristic for session boundaries.**
  The natural cadence of "user-initiated RPCs with 15s timeouts"
  exceeds any reasonable threshold. The protocol already has a
  cleaner signal (id=1).

## Open work

- The companion now polls at 25 ms after fresh data and 300 ms
  idle. That's fine for foreground use but worth revisiting if we
  ever add background sync — sustained 3 polls/sec for hours
  isn't free on battery.
- Long reads (offsets > 0) are explicitly answered empty, so any
  single response chunk has to fit in the 512-byte ceiling.
  Larger payloads (e.g. usage history dumps) work today only
  because we chunk at the outbox level, but if a single message
  ever needs more than 512 bytes per fragment we'll need real
  blob-read support.
- We carry the IRK in the BlueZ bond, not in the admin record.
  When multi-admin lands and authorize() needs to distinguish
  bonds, tracking the IRK explicitly will be cleaner than
  address comparison.
