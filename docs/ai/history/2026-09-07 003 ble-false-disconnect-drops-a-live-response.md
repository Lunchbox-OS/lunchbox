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

BlueZ is not being spurious. It is being precise, and the daemon was reading it
imprecisely.

BlueZ 5.85 started with `Experimental` — which shepherd *requires*, because the
dial-out fix needs `PreferredBearer` — puts three interfaces on one device
object: `org.bluez.Device1`, `org.bluez.Bearer.LE1` and
`org.bluez.Bearer.BREDR1`. **All three have a `Connected` property.**

`bluer::Device::events()` throws the interface away. It matches
`Event::PropertiesChanged { changed, .. }` — the `interface` field is carried
all the way to that point and then discarded — and maps the payload by property
*name*. So a `Connected` change on `Bearer.BREDR1` is delivered to the daemon as
`DeviceProperty::Connected`, indistinguishable from the device's own.

A dual-mode phone brings its classic profiles up and down on its own schedule
after LE bonding. Captured on the system bus during a pairing:

```
17:25:27.685  InterfacesAdded dev_53_73_F6_74_4E_DA
              Device1  Address="B8:F4:A4:E5:20:F1"  Bonded=true  PreferredBearer="last-used"
17:25:31.609  org.bluez.Bearer.BREDR1   Connected=true      <- the phone's classic profiles
17:25:31.611  org.bluez.Device1         PreferredBearer="bredr"   <- our pin
17:25:31.861  InterfacesAdded: org.bluez.Network1, org.bluez.MediaControl1
              Device1  Modalias + UUIDs = A2DP 110a/110c/110e, AVRCP 110e,
                                          PBAP 1112, HFP 111f …   <- BR/EDR SDP
17:25:32.194  Device1  ServicesResolved=false
17:25:32.194  org.bluez.Bearer.BREDR1   Connected=false
17:25:32.194  org.bluez.Bearer.BREDR1.Disconnected  "org.bluez.Reason.Unknown"
```

`org.bluez.Device1.Connected` **never changed**. Across the whole capture, every
genuine LE disconnect emitted `Bearer.LE1.Connected=false` *and*
`Device1.Connected=false` together; this event emitted only the BR/EDR pair. The
LE ACL carrying the GATT service was up the entire time, which is exactly why
the companion's RPCs kept working either side of it.

So the daemon saw a *classic-audio* bearer flap and treated it as its GATT peer
connecting and then vanishing — and on the vanishing it wiped both outboxes,
taking the ~7.4 KB `service_state` the companion was mid-drain of.

### It is the dial-out bug's sibling

`pin_peer_to_bredr` is the fix for the reverse dial-out failure: probing an
`auto_connect` profile on a bonded phone made bluetoothd call
`device_set_auto_connect(TRUE)` → `MGMT_OP_ADD_DEVICE action=0x02`, after which
*the kernel* dialled the phone, the box took the central role, and — since only
a central may start encryption — every read of an `encrypt_authenticated`
characteristic came back `Insufficient Authentication` forever.

Both bugs are the same confusion: **one bond, two bearers, and code that says
"the device" when it means "the LE link".** The dial-out bug was the kernel
using the wrong bearer; this one is the daemon listening to the wrong bearer.
They are even coupled through `-E`: the experimental flag the dial-out fix needs
for `PreferredBearer` is the same flag that surfaces the per-bearer interfaces
this bug rides in on. A stock bluetoothd has neither.

The pin is *not* the cause, though it sits suspiciously close in the log. The
BR/EDR bearer connected 2 ms **before** the `PreferredBearer` write, and the flap
reproduces at the same 500-600 ms width with the pin left in place.

## The fix, in two layers

**Read the property back instead of trusting the event.** `is_connected()` asks
for `Device1`'s own `Connected`, so it answers for the device rather than for
whichever bearer last twitched. One D-Bus round trip per event turns "something
about connectivity changed" into the fact. That is the root cause, and it also
stops the daemon arming watchdogs and pinning bearers off classic-profile churn.

**And do not discard bytes a peer is mid-read anyway.** The outboxes now go
through `Outbox::clear_if_aligned`, which declines while a peer holds a partial
frame. That is not a new rule — it is the rule
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

### Why keep the second layer

The read-back is the fix; the alignment guard is the seatbelt. `Device1.Connected`
can still go false for real while the companion is mid-frame — a genuine drop
during a large response — and the guard is what keeps that from being silently
destructive rather than merely a disconnect. Its cost is one `if`.

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

### Caught in the act, both times

The trigger was reproduced with the fix in place, so this is no longer inferred:

- **Alignment guard, on hardware.** BR/EDR bearer up at 17:25:31.609, pin, down
  at 17:25:32.194 — and the daemon logged `peer reported gone mid-delivery;
  keeping what it is still reading outbox="response"`. The companion went from
  `service_state` to its next RPC in **4.1 s** (the natural drain) instead of the
  15 s RPC timeout, and the device screen populated immediately.
- **Read-back, on hardware.** Same flap, same width (17:32:08.576 true →
  17:32:09.099 false, 523 ms), `Device1.Connected` unchanged — and the disconnect
  arm did not run at all. The connect arm ran once, correctly, because the device
  really was connected.

## A note for the next person

`shepherd-host-linux`'s lib tests flaked once during this work
(151 passed, 1 failed, no name captured) and then passed six runs in a row and
in the full workspace afterwards. It happened while a headless dev session was
live on the box, which those tests share process/sway/volume state with. Not
related to this change, which touches only `shepherd-ble` — but if it recurs,
that is the first thing to rule out.
