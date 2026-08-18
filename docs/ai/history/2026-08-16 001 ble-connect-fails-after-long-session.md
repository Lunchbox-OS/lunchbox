# "Can't reach this device securely" after a long activity session

## Prompt

> After the latest round of BLE fixes, I am still observing the app fail
> to connect. I did the initial pair, launched Minecraft via Prism
> Launcher, played for roughly 20 minutes, and while it was still
> running, attempted to connect the app. It failed with "Can't reach this
> device securely" and never recovered in this session -- even after
> closing Minecraft or restarting shepherdd with a logout/login. See if
> you can reproduce this on your setup -- the phone from before (passcode
> 314159) and dongle should still be available

Reported against `main` at e79d34a (0.3.5), i.e. after
<2026-08-01 001 ble-connect-drain-unbounded.md>,
<2026-07-18 003 companion-ble-connection-audit-and-fixes.md> and the
<2026-08-10 002 ble-management-stress-test.md> follow-ups.

## What the message actually means

`"Can't reach this device securely"` is `LinkStatus.RepairSuggested`
(`ui/components/Components.kt`), and `ShepherdViewModel.runConnectionLoop`
reaches it on exactly one path:

1. `connectWithin()` failed **six times in a row** (initial attempt plus
   the `1s, 2s, 5s, 10s, 20s` backoff ladder — each attempt itself has a
   30 s `CONNECT_TIMEOUT_MS`, so this is minutes, not seconds), **and**
2. Android still lists the peer as `BOND_BONDED`, **and**
3. the 5 s scan probe **still sees the device advertising the shepherd
   service** — i.e. the box is powered, in range, and shepherdd is up.

So the banner is not a claim that the bond is broken; it is "shepherd is
right there, advertising, and I still cannot hold an encrypted link to
it". If the device is *not* advertising at give-up the app shows
`Disconnected` instead.

**The give-up is terminal.** `runConnectionLoop` returns after setting the
banner; nothing retries on its own. Only `Retry`/`Re-pair`, a
device-switch, or a background→foreground cycle (`onForeground` restarts
the loop when `connection == null`) starts it again. This is the
already-recorded Finding 1 of the stress test — "the app never reconnects
on its own" — and it is the half of the report that reproduces trivially:
*whatever* transient fault first exhausts the ladder, the app then sits on
the banner forever, so closing Minecraft, restarting shepherdd, and
logging out/in cannot change anything. The device side is not being asked
to do anything at that point.

## Reproduction attempts (all on the dev box, real phone + dongle)

Setup: headless dev session, `config.example.toml` with the BLE adapter
**pinned to the Realtek dongle** (`8C:68:8B:41:02:DC`) — the box also has
the Qualcomm radio (`DC:56:7B:1F:7D:EA`) that the pairing skill documents
as individually broken (ACL connects, service discovery hangs for the
whole connect budget), and with no `adapter` set shepherdd takes
"whichever BlueZ lists first", which had selected exactly that radio.
Pixel 10a re-paired from scratch; `lofi-beats` given Prism Launcher's
limits and warnings (`max_run 1800`, warnings at 600/120/30 s) so a
20-minute session hits the same warning schedule; six busy loops on the
host for game-like CPU load.

| # | Scenario | Result |
| --- | --- | --- |
| 1 | Event-rate measurement over a running session (IPC `subscribe_events`) | **No flood**: 5 events / 90 s, 4–5 KiB snapshots. The events outbox cannot outrun the connect drain this way. |
| 2 | Cold app start 2 min into the session, under load | **Connects** (~15 s), live state |
| 3 | App foregrounded + connected, screen off, phone forced into deep doze for 18 min while the session runs, then woken | **Recovers by itself** in ~6 s (`events drained 296 stale bytes`, then RPCs) |
| 4 | 75 s device-radio outage (`hciconfig hci2 down/up`) with the app foregrounded | **Recovers by itself** ~7 s after the radio returns (ladder not yet exhausted) |
| 5 | One-sided bond (`bluetoothctl remove` on the device only) | App ends on **`Disconnected`**, not the reported banner — and the device pops an *unattended Numeric Comparison prompt on the TV*, because the phone's stale bond makes its first read start a fresh pairing |
| 6 | The literal report: app force-stopped, phone locked and screen off, 23-minute session under load, then cold start | **Connects** in ~5 s, live state (`6:06 left`) |

None of the device-side scenarios produced a connect failure at all.
Prism Launcher itself is not installed on this box, so scenario 6 used
`shepherd-media` under Prism's limits rather than Minecraft — the session
lifecycle, warning schedule and host load are reproduced, the specific
workload is not.

## Where that leaves the report

The device side of the reported flow does not fail here: a 20-minute
activity session, with warnings firing and the host loaded, leaves the
daemon advertising, the outboxes empty, and the companion connecting in
seconds — cold, warm, or after doze.

What does reproduce is one kind of permanence: once the ladder is
exhausted the app is inert until the user acts on it, so a *foregrounded*
app stays dead long after the fault clears. That is a real defect and it
is fixed below — but see the journal section: it does **not** explain
this report, because the reporter also force-closed the companion, which
starts a fresh loop and bypasses that path entirely.

Scenario 5 also shows a second, real defect worth its own fix: a phone
that kept a bond the device has forgotten silently raises a pairing
prompt on the TV (30 s of "someone wants to pair" with nobody there),
which is both confusing and, on a kiosk, a prompt a child sees.

## The fix

`runConnectionLoop` no longer returns when it gives up. It sets the
banner exactly as before — the user still gets `Retry`/`Re-pair` — and
then parks a slow retry in `sessionJob`
(`RETRY_AFTER_GIVE_UP_MS = 60_000`) that rebuilds the connection and
re-runs the whole ladder. Being in `sessionJob` is what makes it safe:
`teardown()`/`onBackground()` cancel it like any other session work, so
the "no background BLE" rule still holds, and `onForeground` still gets
an immediate attempt on return rather than waiting out the interval. The
`NeedsRepair` branch (the OS bond is provably gone) stays terminal —
retrying genuinely cannot help there.

### Verified on hardware, before and after

Same test both times: companion foregrounded and connected, the serving
controller taken down for **6 minutes** (`hciconfig hci2 down`) — long
enough to outlast the ladder, which a 75 s outage does not — then brought
back with the phone untouched for 3 minutes.

| Build | At give-up | 3 min after the radio returned |
| --- | --- | --- |
| Before | `Disconnected` + Retry/Re-pair | **Nothing.** No app log lines, no connection, no RPC — dead exactly like `copernicus` |
| After | `Reconnecting…` (mid-ladder on a retry cycle) | **Reconnected 2 s after the radio came back**: peer connected, `service_state`/`list_groups`/`get_volume`/`get_brightness` all `ok=true`, live entry list on screen |

### The suspend/resume check could not be run here

The intended final check was a real s2idle cycle on the dev box
(`rtcwake -m mem -s 120`, the same sleep mode `copernicus` used). **Do
not repeat it on this VM.** The box is a KVM/QEMU guest with a
PCI-passthrough USB card; it entered `PM: suspend entry (s2idle)` and
never resumed — the RTC alarm did not wake the guest — and it took a
host-side reboot to recover, which also wiped everything under `/tmp`.
The radio-outage test above exercises the same thing the suspend does to
the companion (the box unreachable for longer than the ladder) without
betting the machine on a resume.

## The journal from the affected box (`copernicus`, same day)

Supplied after the reproduction attempts above. It settles the question:
the trigger is not the game, it is a **suspend/resume**, and after the
resume the phone stops talking to the box entirely.

| Time (EDT) | Event |
| --- | --- |
| 07:35:49 | BLE server up on `hci0` (`D8:B3:2F:E8:47:B2`), `selector="<first listed>"`, advertising started |
| 07:38:46–51 | Numeric Comparison, pairing complete, `claim` recorded (`SM-S911…`, a Galaxy S23) |
| 07:39:14–24 | Companion session: `service_state`, `list_groups`, `get_volume`, `get_brightness`, **`launch`** (Minecraft started *from the app*), `current_session`, … all `ok=true` |
| 07:39:27 | Peer disconnected — the app is backgrounded |
| 07:39:29 | Peer connected again — **and never sends another RPC, all day** |
| 08:31:30 | `Power key pressed short` → `Suspending...` |
| 09:13:59 | `System returned from sleep`; peer disconnected |
| 09:14:00 | Peer connected (outboxes empty) — again **no RPC**, for the 2m50s until logout |
| 09:16:50 | BLE server shutting down (user logs out) |
| 09:17:15 | Fresh session: server starts, **advertising started**, no errors |
| 09:18:25 | `Power key pressed short` → `Suspending...` (second suspend) |
| 09:18:43–44 | Resume; peer disconnected |
| 09:18:44 → 09:22:40 | **No peer connection at all** — nothing, until the session is shut down |

Three things follow.

1. **The device side is healthy throughout.** It advertises, it answers
   every RPC it is asked, it re-registers cleanly on the new session, and
   the outbox depths logged at each connect are `0/0` — no drain backlog,
   no stalled queue. Nothing here resembles the connect-drain failure the
   0.3.5 fixes were about.
2. **After the fresh login the phone never establishes a link.** BlueZ
   reports the peer's `Connected` property false from 09:18:44 to the
   shutdown at 09:22:40 — four minutes of the user trying with nothing to
   show for it.
3. **The journal cannot say why**, and an earlier draft of this note
   claimed it could. `Connected` flips on a link that comes *up*; a
   connect that never gets one leaves no device-side trace at all. So
   silence here is equally consistent with "the app wasn't trying" and
   "the app tried and never reached the air", and only the first of those
   is the terminal give-up.

The reporter settles that: they force-closed the companion and logged out
and back in, and it still never reconnected. A cold start is a new
process, a new `ShepherdViewModel`, and a fresh ladder — it bypasses the
give-up path entirely. **So the give-up fix below is not the explanation
for this report.** It is a real defect, reproduced and fixed, and it would
have kept any *already-open* app dead after the box came back; it is not
what stopped a freshly-launched app from connecting.

What is left has to survive both a companion force-close and a shepherdd
restart, which rules out everything in the daemon's own state. In rough
order of likelihood:

- **Phone-side Bluetooth state.** The pairing skill already records a
  wedge on this hardware that survived a reboot, a Bluetooth toggle and a
  storage wipe, and cleared only via Settings → Reset Bluetooth & Wi-Fi.
  A stale GATT attribute cache is the milder version: Android caches the
  peer's handles per *bond*, so it outlives the app entirely, and a
  daemon restart that re-registers the GATT application can leave the
  phone reading handles that no longer mean anything. That would look
  exactly like 09:14:00 — link up, no RPC, forever.
- **The controller not actually radiating after the resume.** The daemon
  logs "advertising started" once, when `RegisterAdvertisement` returns;
  it is not evidence about the radio 90 minutes and two suspends later,
  and nothing re-checks. Note this did *not* reproduce on the dev box: a
  full `rfkill block`/`unblock` cycle left `LEAdvertisingManager1.ActiveInstances`
  at 1 with no daemon involvement, and the companion reconnected
  afterwards. Different controller, and rfkill is not s2idle, so it is
  weakened rather than excluded.
- **The phone's classic profiles.** The S23 keeps trying A2DP/HFP against
  the box on the cross-transport key (`a2dp.c:auth_cb() Access denied` at
  09:16:59 and 09:17:22, i.e. after the session restart).

Discriminating between these needs the phone, not the box: a
`adb logcat -s ShepherdBle` capture at failure time says directly whether
the app is attempting, and how far it gets ("connect: ready", "drained N
stale bytes", read failures, service-not-found). Worth trying before the
force-close, too: toggle the phone's Bluetooth off and on. If that alone
restores it, the fault is phone-side cache/wedge and the fix belongs in
the app (a `BluetoothGatt.refresh()` on connect, by reflection, the way
`removeBond` is done).

One more caveat on reading these lines: `Device.Connected` is
transport-agnostic, so the connections at 07:39:29 and 09:14:00 that
carry no RPC may be the phone's *classic* profiles rather than the
companion at all — Numeric Comparison over LE on a dual-mode phone mints
a cross-transport key, and the S23 starts trying A2DP/HFP against the box
immediately. Our "BLE peer connected" line fires for those too. It would
be worth logging the transport (and whether the link is encrypted)
alongside it; as it stands the line proves less than its wording
suggests.

## Phone-side evidence: two different failures

Two Android bug reports from the box's phone (a Galaxy S23), taken minutes
apart while the failure was live, turn out to describe *different* faults.

### 16:04 — nothing on air

Ten direct LE connects to the box's address between 16:04:51 and
16:05:41, every one ending

```
le_impl.h:1539 on_create_connection_timeout, address: xx:xx:xx:xx:47:b2
bta_gattc_act.cc:358 bta_gattc_open_fail: Connection timed out after 30 seconds
BluetoothGatt: onClientConnectionState() - status=147 connected=false
```

and the companion's own 5 s scan probe (`scannerId 15`) returned **zero
results** — the box's address appears in no scan result from any app
anywhere in the report. The box's journal for that session explains why:
`shepherdd` started at 16:03:07 and there is not one `shepherd_ble` line
in the whole boot, because `[service.ble_management]` had been switched
off. `Policy::from_raw` drops the section with `.filter(|c| c.enabled)`,
`main.rs` matches `None => (None, None)`, and **nothing is logged either
way** — an intentionally-disabled transport and a broken one look
identical in the journal. `ActiveInstances = 0` from `busctl` confirmed
it independently.

### 16:37 — on air, connected, and never encrypted

With BLE re-enabled and a fresh pairing, one connection worked; after a
sleep/resume of the box the companion showed "Can't reach this device
securely" again, and this time the phone reached the device:

```
ShepherdBle: response: first read did not land while the link warms up (attempt 1/3):
  OnCharacteristicRead(characteristic=8c0c0004-…, status=GATT_INSUFFICIENT_AUTHENTICATION(5))
…47:B2 [DUAL] [ACL BR/EDR:N LE:Y] [Encryption status(BR/EDR): null LE: null]
```

An LE link is up (`LE:Y`) and completely unencrypted (`LE: null`, where a
healthy link reads `EncryptionStatus{keySize=16, algorithm=2}`), so BlueZ
correctly refuses every read of an `encrypt_authenticated_read`
characteristic. `btmon` on the box over 1052 lines shows the same thing
from the other side — endless `Read Request` / `Error: Insufficient
Authentication (0x05)` on handle 0x0021 — and, decisively, **zero SMP
frames, zero `LE Long Term Key Request`, zero `Encryption Change`**.
Neither side ever attempts encryption. The phone's stack decides to
(`btm_ble_link_sec_check … sec_req_act=BTM_BLE_SEC_REQ_ACT_ENCRYPT`, with
`cur_sec_level=0x4` = it still holds the authenticated LTK) and puts
nothing on air; BlueZ answers 0x05 without sending a Security Request.

Neither an adapter power cycle on the box nor a Bluetooth toggle on the
phone cleared it — and the same capture says why.

### The roles are backwards

```
> ACL Data RX  … LE L2CAP: Connection Parameter Update Request   (phone → box)
< ACL Data TX  … LE L2CAP: Connection Parameter Update Response  (box → phone)
< HCI Command: LE Connection Update (0x08|0x0013)                (box → controller)
```

Only a peripheral sends the L2CAP Connection Parameter Update Request;
only the central answers it and issues `HCI LE Connection Update`. Both
exchanges in the capture run that way, so on that link **the box is the
central and the phone is the peripheral**.

Only a central may start encryption. The phone therefore *cannot*: its
stack decides it must (`sec_req_act=BTM_BLE_SEC_REQ_ACT_ENCRYPT`, holding
an authenticated LTK) and has no way to act, because its one lever as
peripheral — an SMP Security Request — is not sent on behalf of its own
GATT client. The box, which could, never does: nothing on our side asks
BlueZ for a secure link, so bluetoothd just answers ATT 0x05 and carries
on. Nobody can break the tie, which is exactly the "no SMP frames at all"
the capture shows.

The role is decided by whoever initiates, so if the box keeps originating
the connection every new link is born broken — which is why a phone
Bluetooth toggle, an adapter power cycle, a shepherdd restart and an app
force-close all reproduced it identically. It also explains the morning
journal's `BLE peer connected` at 09:14:00 with no RPC ever arriving, and
why it started after a resume: on resume BlueZ reconnects devices it
knows, and the box reached out instead of waiting to be reached.

Caveat: the capture starts mid-connection, so "the box initiated" is
inferred from the role rather than observed. `btmon` across a fresh
reconnect would show the `LE Create Connection` directly.

### Mitigation: evict a link the box shouldn't own

`spawn_first_rpc_watchdog` — a bonded peer that connects and sends no RPC
within [`FIRST_RPC_GRACE`] (25 s) gets its link dropped, with a `warn!`
naming the reason. The companion reconnects immediately and, because it
initiates, becomes central and encrypts normally. `last_peer` is the
signal: it is set by the first request write, and that write is itself
`encrypt_authenticated`, so on a deadlocked link it never happens.

This also closes the observability gap that made the fault so expensive
to find: bluetoothd answers those 0x05 reads itself, so our
characteristic callbacks never fire and the daemon previously logged
nothing at all between "peer connected" and an RPC that never came.

Unpaired peers are skipped deliberately — during pairing the companion
holds a link for as long as the Numeric Comparison prompt takes a human
to answer, and sends no RPC until the bond completes.

Verified on hardware in both directions. With the grace temporarily cut
to 3 s, a real connecting companion is caught (`peer connected` 03:01:13
→ `Bonded peer connected but sent no RPC` 03:01:16 → link dropped → the
app reconnects and RPCs flow), which also demonstrates the false positive
a tight value would cause. Back at 25 s, a normal cold start connects and
runs 90 s with zero warnings.

**The root cause is still open**: nothing here stops the box from
originating the connection in the first place. We never set `Trusted`, so
something else on the box is reconnecting the phone — the cross-transport
bond makes it look like an audio device, and the journal shows repeated
A2DP/HFP auth attempts against it.

## The advertisement does not survive a controller power cycle

Found while chasing the above, and reproduced on the dev box: after
`bluetoothctl power off` / `power on`, the companion could not connect at
all and the phone's pairing scan listed nothing — while BlueZ still
reported `Adapter1.Powered = true` **and
`LEAdvertisingManager1.ActiveInstances = 1`**. The device had silently
gone off air, and every property we can query said it was fine. On the
reporter's box this is what made `copernicus` stop appearing in the
pairing list after they ran that power cycle.

`BleServer::run` registered the GATT application and the advertisement
once at startup and then parked on the shutdown watch, so nothing ever
put them back. It now watches the adapter for
`AdapterProperty::Powered(true)` and re-registers both, logging it. A
suspend that resets the controller takes the same path.

Verified on hardware: companion connected, `bluetoothctl power off` /
`power on`, phone untouched → daemon logs "Bluetooth controller powered
back on; re-registering…" 4 s later, the peer reconnects, and RPCs are
flowing 3 s after that (`service_state`, `list_groups`, `get_volume`,
`get_brightness`, all `ok=true`). Note that these RPCs ride the same
`encrypt_authenticated` characteristics, so a power cycle does *not*
break link encryption here — which is another reason the 16:37 failure
above is something else.

## 2026-08-17: still failing after suspend — a *third* failure mode

With all of the above deployed to both the box and the phone, a
suspend/resume still killed it — but not the same way. The device-side
journal:

```
19:42:33  Power key pressed short. / Suspending...
19:43:30  System returned from sleep operation 'suspend'
19:43:30  bluetoothd: Controller resume with wake event 0x0
19:43:30  shepherd_ble::server: BLE peer disconnected; clearing transport session state
          … and then nothing at all, for six minutes
```

No `Powered` transition, so **the power-cycle re-arm never fired**. No
peer connection either, and `btmon -i hci0` over the failure window
captured nothing but its own header — zero HCI traffic.

The phone says why: every attempt ends `status=147` (connection timeout),
27 `on_create_connection_timeout` entries, **zero**
`GATT_INSUFFICIENT_AUTHENTICATION`, and the box's address in **zero**
scan results from any app. So this is "off air", not the encryption
deadlock — the same shape as the 16:04 report, except BLE was enabled and
the daemon believed it was advertising the whole time.

Two of the fixes did behave as designed: the companion retried steadily
for five minutes instead of giving up once (`19:45:08, 19:45:34, 19:45:45,
19:45:57, 19:46:12, 19:48:06, 19:49:43, 19:49:54`), and the first-RPC
watchdog correctly stayed quiet — there was no link to evict.

### The gap, and the second trigger

`go_on_air` was re-armed only by `AdapterProperty::Powered(true)`. That
covers a controller power cycle and misses a suspend that takes the
advertisement off air without moving any property we can see — which is
exactly what this box does.

The run loop now also re-arms on the daemon's own
`EventPayload::SystemResumed`, which arrives through
`ManagementService::subscribe_events` — the stream the events forwarder
already consumes, so no new plumbing crosses the crate boundary. Both
triggers route through the same `go_on_air` call and log a `reason`.

The decisions are split into `rearm_reason_for_adapter_event` and
`rearm_reason_for_service_event` so they are unit-testable without an
adapter; the tests assert both triggers fire and that ordinary events
(including `Powered(false)` and `SystemSuspending`) do not, because
re-arming drops the GATT application and disconnects whoever is on it.

**Verified**: both trigger decisions by unit test, and the shared re-arm
body on hardware via a controller power cycle (`Re-registering the GATT
application and advertisement … reason="the Bluetooth controller powered
back on"`, then the companion reconnects and RPCs flow). **Not verified**:
the resume trigger itself on real hardware — this dev box is a KVM guest
that cannot resume from s2idle, and logind's signal cannot be spoofed
(zbus matches on sender). That one needs a suspend on the affected box.

## 2026-08-17, later: the watchdog broke pairing — now report-only

Deployed to the affected box, the first-RPC watchdog made pairing
impossible. Three attempts, each identical:

```
20:24:42  BLE peer connected …
20:25:07  WARN Bonded peer connected but sent no RPC; dropping the link …   (T+25.0s)
20:25:09  BLE peer disconnected
```

and on the phone, `BOND_NONE → BOND_BONDING` at 20:24:42 followed by
`onClientConnectionState() status=19 connected=false` —
`GATT_CONN_TERMINATE_PEER_USER`, i.e. *we* hung up, 25 seconds into the
window where a human is comparing six digits.

The `is_paired()` gate was supposed to prevent exactly this, and didn't:
the box still held a **stale bond** from the previous pairing, so the
peer read as bonded while the phone was mid-bond with no keys of its own.
The gate tests the wrong side of the relationship, and there is no
reliable "bonding in flight" signal available before SMP reaches the
agent.

**The eviction is removed; the line stays.** Silence on a fresh link is
not specific enough to act on — pairing, a slow drain, and the
role-inversion deadlock are indistinguishable from the daemon's side,
and only one of them wants a disconnect. What the log line buys is the
thing that was actually missing: bluetoothd answers the `0x05` reads
itself, so our callbacks never fire and the failure was invisible.
Eviction can return when the deadlock is reproducible and something
distinguishes it.

Verified after the change: pairing from scratch completes normally
(Numeric Comparison → `Pairing complete` → `claim` → bond at
`keySize=16`), including with the grace forced to 3 s so the timer fires
mid-flow. Note the timer did *not* arm during pairing on the dev box at
all — for a peer BlueZ only learns about at connect time, the device
watcher attaches after the `Connected` transition. On the reporter's box
the peer was already known, so it armed. That asymmetry is worth
remembering when reasoning about this path.

Unrelated flake seen while testing, worth its own look: one session died
at startup with `Failed to register BlueZ pairing agent (D-Bus
NoReply)`, which takes BLE management down for the whole session with no
retry and no advertising.

## A transient agent registration failure no longer kills the transport

Hit while testing the above: one session died at startup with

```
ERROR shepherdd: BLE management server error
  error=Failed to register BlueZ pairing agent (D-Bus error
        org.freedesktop.DBus.Error.NoReply: …)
```

bluetoothd was busy for a moment; `run` returned the error and BLE
management was gone for the whole session — no advertising, no retry, and
that single line as the only evidence. From the outside it is
indistinguishable from every other "the device just isn't there" failure
in this note, which is exactly why it cost time.

Registration now retries (500 ms, 2 s) and, if it still fails, the server
**carries on without an agent** and says so at ERROR. The trade is
deliberate: losing the agent costs pairing — the Numeric Comparison
overlay on the TV — while losing the server costs everything, including a
bonded admin phone that only wanted to reconnect to an already-claimed
device. The re-arm path retries registration whenever it puts the service
back on air, so a box that starts degraded recovers on the next resume or
power cycle rather than needing a session restart.

Verified both ways on hardware with a temporary fault injection: normal
boot registers first try and pairs; with registration forced to fail, the
log shows two retries and the ERROR, advertising still starts, and an
already-paired companion connects and runs a full session
(`service_state`, `list_groups`, `get_volume`, `get_brightness`).

## What to capture on the box when it happens again

The daemon logs every peer-level event, so the journal separates the
three candidate causes without guessing:

shepherdd is `exec`-ed from `sway.conf`, so its output lands wherever the
session's sway log goes (`journalctl --user -b` on a systemd-managed
session, otherwise sway's own log file):

```sh
journalctl --user -b | grep -E \
  'BLE peer (connected|disconnected)|BLE RPC|Numeric Comparison|advertising'
```

- **No `BLE peer connected` at all** while the app retries → the phone
  never got an ACL link up: radio/controller or range, not the daemon.
  Check `dmesg -T | grep -i bluetooth` for `command tx timeout` /
  `Resetting usb device` (this box's Realtek dongle has done exactly that
  before, on 2026-08-14), and try `systemctl restart bluetooth`.
- **`connected` … `disconnected` a few hundred ms later, repeatedly, with
  no RPC** → the link comes up and then fails to encrypt or discover:
  one-sided bond, or the discovery-hang failure mode of a bad controller.
- **`connected` + `Numeric Comparison pairing requested`** → the device
  has forgotten the bond (scenario 5); `Re-pair` is the fix.

Also worth recording: whether the box has more than one Bluetooth
controller and which one `[service.ble_management] adapter` pins. With it
unset, shepherdd takes whatever BlueZ lists first, and that can change
across a re-plug or a differently-enumerated boot.
