# BLE management API "unresponsive after reconnecting under load"

## Prompt / report

> I'm noticing an issue where the BLE management API becomes unresponsive
> after some amount of time, particularly under some combination of
> connecting and reconnecting several times while a heavy activity is
> running. […] I did see the 15s timeout toast appear.
>
> [follow-up] see if you can reproduce on this device pair — I suspect
> just loading the CPU even from this user will be enough.

Environment: a live `shepherdd` running as `shepherd-kiosk` on `leibniz`,
with a Pixel 10a (companion app `com.armeafamily.shepherd.companion`)
bonded over BLE. `adb` available to the phone; HTTP management API also
enabled on the daemon (same `ManagementService`).

## TL;DR

The 15 s toast is the companion's `REQUEST_TIMEOUT_MS`: a request is
written but its response is never *read back* in time. The root cause is
a **client-side busy-spin** in `ShepherdConnection.pollLoop`. On a read
failure the loop `break`s with no back-off and no state reset, and the
outer loop's `ready.first { it }` returns instantly while `ready` is
still `true`. When kable reports the peripheral as `Connected` while
every `read` throws `NotConnectedException` — exactly what a flaky /
BT-toggle reconnect produces — the loop spins at millions of
iterations/sec on the (main-thread) dispatcher. That pegs a phone core,
floods logcat, and starves both the response poller and the very state
collector that would set `ready = false`, so it never recovers → RPCs
time out at 15 s and the UI wedges.

It is **not** a daemon problem. Under full 4-core CPU saturation the
daemon's management runtime still answered in ~1 ms with zero timeouts.
The "CPU load" correlation the user saw is the *phone's* CPU being
saturated by the spin (and, on the real device, a heavy activity
generating the event churn / reconnects that trigger it).

## Investigation

### Ruled out (daemon side)

- **Controller/firmware wedge** — `journalctl -k` clean; NXP adapter
  healthy, no `command tx timeout`.
- **bluer serializing callbacks** — `dbus-crossroads` spawns each
  `ReadValue`/`WriteValue` as its own task (`run_async_method` →
  spawner), so a slow write can't block reads at the D-Bus layer.
- **Server locks / queues** — `Outbox` and `ClaimMachine` use short
  critical sections; `authorize` is trivial in-memory. No deadlock.
- **Tokio-runtime CPU starvation** — refuted empirically. Using the HTTP
  API (`/api/v1/rpc`, same `ManagementService`, same 4-worker runtime)
  as a scriptable stand-in for BLE:

  | Condition | `ping` p50 | `ping` max | `service_state` p50 | timeouts |
  |---|---|---|---|---|
  | idle | 0.50 ms | 0.66 ms | 0.86 ms | 0/80 |
  | 100% CPU saturation (0.0% idle, all cores) | 0.74 ms | 3.5 ms | 1.34 ms | 0/110 |

  CFS fairness protects the ~2%-CPU daemon; `bluetoothd` (single-thread,
  system.slice, ~0.3% CPU) is likewise not starvable by fair scheduling.

### Reproduced (client side, via adb)

Forcing the transient-reconnect path (BT off→on while the app stays
foregrounded — this reuses the same `ShepherdConnection` via
`runConnectionLoop`) put the companion into a tight busy-spin:

```
I/ShepherdBle: events poller entering main loop
W/ShepherdBle: events read failed after 0 reads (0 empty)
… dozens of iterations per millisecond …
```

→ **691 MB / 5.4 M log lines in ~2 minutes**, a pegged phone core.

## Root cause — `companion-android/.../ble/ShepherdConnection.kt`

`pollLoop`'s inner read loop:

```kotlin
while (isActive && ready.value) {
    val bytes = try { peripheral.read(char) }
        catch (e: Throwable) {
            Log.w(TAG, "$label read failed …", e)
            break                // no delay, no state reset
        }
    …
}
// outer loop: ready.first { it }   // returns INSTANTLY while ready==true
```

The failure path relies entirely on the state-collector coroutine
(`peripheral.state.collect { if (!Connected) ready.value = false }`) to
stop the loop. But (a) kable can report `Connected` while reads throw,
and (b) the zero-delay spin monopolises the main-thread dispatcher and
starves that collector — a self-sustaining hot loop.

## Fix

On a read failure: never hot-loop. Back off with the same adaptive delay
as the idle path (which also *yields* the dispatcher so the state
collector can run), and after `MAX_CONSECUTIVE_READ_FAILURES` (5,
~1.5 s with back-off) force a real `peripheral.disconnect()` so the
reconnect loop rebuilds the session instead of retrying a dead link
forever. Reset the counter on any successful read.

## Verification (on-device, fixed debug APK installed in place)

| | Old build | Fixed build |
|---|---|---|
| Log lines after BT-toggle reconnect | 5.4 M / 691 MB | 47 |
| `read failed` count | millions | 2 |
| App CPU | pegged core | 0.0% |
| Recovery | never (stuck) | clean reconnect |

Clean relaunch after the test: `connect: ready` → `id=1/2/3 ok=true`,
0 read failures, CPU ~6%. `:app:compileDebugKotlin` and
`:app:testDebugUnitTest` both pass.

## Follow-up (defense-in-depth, not required for this bug)

Server-side, the Response/Events `Outbox` is never cleared on BLE
disconnect (the `clear()` doc comment claims it is, but nothing calls it
on disconnect; the only reset is the client's `id == 1` sentinel, which
`runConnectionLoop` reconnects don't re-send). Under a heavy event
backlog this can slow reconnect drains and, if the response outbox ever
fills mid-delivery, silently drop an RPC response. Worth hardening:
subscribe to device-disconnect and clear the outboxes there, and/or cap
the events backlog to the latest snapshot. Tracked separately from the
poll-spin fix.
