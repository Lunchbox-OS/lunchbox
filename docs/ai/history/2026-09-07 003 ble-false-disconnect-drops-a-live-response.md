# A false disconnect throws away the response the companion is reading

**Prompt:** `investigate what happened to BLE` → `make a fix on a branch`

Found while reverifying #177's rebase on hardware: after a first pairing the
companion's device screen sat empty, and the app logged

```
dispatch: failed to parse response frame (7456B): {"id":2,"result":{"api_version":1,…
```

That log line is a red herring twice over. It is not a framing bug, and it is
not the app's fault.

## What is actually happening

`Device.Connected` is not a trustworthy account of whether the GATT link still
carries ATT traffic. It arrives over a second behind the reads and writes it
purports to describe, and **`pin_peer_to_bredr` provokes a spurious
`Connected(false)` of its own**: pinning a freshly-paired peer tears down the
kernel's LE auto-connect, BlueZ reports the device disconnected, and never
reports it back — while the companion carries on reading and writing over the
same link for the rest of the session.

The disconnect arm called `reset_session()`, which cleared both outboxes
unconditionally. So the ~7.4 KB `service_state` the companion had just asked for
and was halfway through draining was destroyed underneath it.

Caught byte-for-byte with `RUST_LOG='info,shepherd_ble=debug'`:

```
16:52:34.944  claim (id=1) received, response queued
16:52:35.136  read 255                    <- claim response, delivered whole
16:52:35.480  service_state (id=2) received; response queued  ~7.4 KB
16:52:35.672  read 512  ┐
16:52:35.918  read 512  │
16:52:35.929  Connected(true): clear_if_aligned DECLINED, kept_for_drain=true
16:52:35.930  Pinned peer to the BR/EDR bearer
16:52:36.257  read 512  │   2048 B drained
16:52:36.454  read 512  ┘
16:52:36.532  Connected(false): reset_session() -> clear()   <- takes the other ~5.4 KB
              ... 14 s of empty reads ...
16:52:50.544  list_groups (id=3)          <- the app's 15 s REQUEST_TIMEOUT_MS expired
```

Note what is *absent*: any later `Connected(false)`, and any `RPC id=1; clearing
outboxes for new session`. The link never dropped and the RPC session never
restarted — ids 3, 4, 5… were all answered on the same connection. Only the
bytes were lost.

The companion cannot detect this. A truncated read is indistinguishable from an
idle one, so it polls an empty characteristic until its RPC timeout. After a
first pairing the response in question is the opening `service_state`, so the
first screen a new user sees is empty.

**Two symptoms, one cause**, depending on how much had been drained when the
clear landed:

- *Enough buffered to complete a frame* — the reassembler pads the honest
  7456-byte length out of the following frame's bytes and hands up a spliced
  message with a valid-looking JSON head. That is the `failed to parse response
  frame` line.
- *Not enough* — the frame never completes, nothing is logged at all, and the
  app stalls silently for 15 s. That was both later reproductions.

## The fix

The outboxes now go through `Outbox::clear_if_aligned`, which declines while a
peer holds a partial frame. That is not a new rule — it is the rule
`Outbox::push_inner` and the `Connected(true)` handler already follow, and for
the same stated reason: bytes a peer is in the middle of reading are not ours to
throw away. The disconnect path was the one place still ignoring it.

Nothing leaks as a result. Anything genuinely stale is disposed of twice over,
by the companion's post-connect drain and by the `id == 1` clear in
`handle_request`, and the guard releases as soon as the peer finishes the frame.

The request-side `FrameReader` is still reset unconditionally, and that
asymmetry is deliberate: an orphaned partial *request* has no second chance,
because nothing on this side can resync a byte stream that starts mid-frame, and
the next session's writes would stitch onto the orphan. Requests arrive in a
single ATT write in practice, so there is next to nothing in flight to protect.

The `Connected(false)` log line now says "reported disconnected", because that
is all the property actually tells us.

### Why not fix it at the source

Suppressing the `Connected(false)` that follows our own bearer pin would need a
timing window, and would still leave every other way BlueZ can lie about the
link. Refusing to discard in-flight bytes is correct no matter why the property
is wrong.

## How it was verified

- **Three reproductions before the fix**, two of them with server-side debug
  logging, all showing exactly 2048 bytes drained and then the clear.
- **Four unit tests in `server.rs`**, and they discriminate — reverting
  `reset_session` to the unconditional clear fails
  `a_reported_disconnect_spares_a_response_the_peer_is_still_reading` and
  `a_reported_disconnect_always_drops_a_half_written_request`, while
  `reset_session_wipes_all_session_state` and
  `a_reported_disconnect_clears_an_outbox_no_one_is_mid_frame_on` keep passing.
  That last one is the anti-leak guard: an aligned outbox is still cleared.
- **On the box, with the phone**: factory reset → re-pair (Numeric Comparison,
  bond `LE:Y` `keySize=16`) → `claim` → the full 7458-byte `service_state`
  delivered in 14×512 + 290 with the app moving on 243 ms later rather than
  after a 15 s timeout → all management reads → reconnect on the existing bond,
  `BLE outbox backlog drained bytes=10684 reads=22`.
- `cargo test --workspace --all-targets` (68 binaries), `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo fmt --all` clean.

### What is not yet proven on hardware

In the post-fix runs BlueZ did not emit the spurious `Connected(false)`
mid-delivery — it stopped doing so after both Bluetooth stacks were reset
during the debugging, and it is not something this side can provoke on demand.
So the hardware runs show the change causes no regression and that
`service_state` now lands whole, but they do **not** demonstrate the guard
firing in situ; the unit tests are what pin that. Worth watching for the
`peer reported gone mid-delivery; keeping what it is still reading` line on a
device — that log exists precisely so the next person sees it happen rather
than inferring it.

## A note for the next person

`shepherd-host-linux`'s lib tests flaked once during this work
(151 passed, 1 failed, no name captured) and then passed six runs in a row and
in the full workspace afterwards. It happened while a headless dev session was
live on the box, which those tests share process/sway/volume state with. Not
related to this change, which touches only `shepherd-ble` — but if it recurs,
that is the first thing to rule out.
