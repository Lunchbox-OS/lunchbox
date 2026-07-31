# Re-running the Waydroid integration suite on HEAD (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

## Prompt

> run the waydroid integration suite

The last pre-review item: `tests/waydroid_real.rs` +
`scripts/integration-tests/test-waydroid.sh` had not run since `af4af6a`, which
predates the preboot/native-scale/locktask work and today's loading screen.

## Result

All four tests pass on HEAD against real Waydroid (LineageOS 20 / Android 13
GAPPS, Waydroid 1.6.2, patched hwcomposer present):

```
[OK] preboot: session running, multi_windows enabled                       4.38s
[OK] Android app launched, window present: waydroid.com.android.calculator2 2.13s
[OK] Android session stopped, window gone, Exited emitted
[OK] lock-down active: mDisabled1=0x3210000 (shade/nav disabled)
[OK] locktask: full-UI Waydroid surface up, no per-app toplevel, Lock Task engaged
[OK] locktask: stopped, surface parked off screen, Exited emitted           4.35s
[OK] shutdown: a non-prebooting host left the session alone
[OK] shutdown: the prebooted session was stopped                           0.72s
```

Getting there took five runs, because three separate defects in the *harness and
tests* made correct product code look broken, and one real product bug hid
behind them. In order of discovery:

## 1. Harness: the nested sway config had drifted (fixed)

The locktask test failed with `SpawnFailed("Waydroid full UI did not appear
within 45s")`. The orchestrator's nested sway config was a hand-written copy of
the production window rules, and when the locktask path started **parking** the
full-UI surface before pinning it, production `sway.conf` gained

```
for_window [app_id="Waydroid"] move container to workspace __shepherd_parked, fullscreen enable
```

while the copy kept the old `fullscreen enable`. `wait_for_parked_full_ui` looks
for the surface *on that workspace*, so it never matched.

Fixed by **lifting the rules out of `sway.conf`** with a grep instead of
restating them, and printing what it used. `scripts/lib/headless.sh` was already
immune — it boots the real `sway.conf`, deriving only the `shepherdd` exec line.

## 2. Product: `prop set` silently no-ops without a session (NOT fixed)

The next run failed in test 1 after a 259 s poll: `preboot should leave a running
session with multi_windows=true`. Verified directly on the rig, container up,
no session:

```
$ waydroid prop get persist.waydroid.multi_windows
[22:59:15] WayDroid session is stopped        # no value
$ waydroid prop set persist.waydroid.multi_windows true; echo $?
[22:59:15] WayDroid session is stopped
0                                              # exit 0, nothing set
```

`waydroid prop set` reaches the property service **through the session** and
exits 0 when there is none. `waydroid::set_prop` treats exit 0 as success, and
`preboot_waydroid` sets its props *before* starting the session — so on a cold
container the `multi_windows` and `width`/`height` pins never land, silently.

Production reachability: shepherdd stops the prebooted session at shutdown, so
*every* daemon start begins session-less and the pins never apply at all. It is
invisible while the persisted values already match, and bites when they don't —
a fresh device (`multi_windows` defaults false) or an operator switching
`lock_mode` from `"locktask"` back to `"statusbar"`, after which the statusbar
path has no per-app `waydroid.<pkg>` toplevel to track.

Noted as "a latent bug" in the 2026-07-17 investigation; now demonstrated.

**Proposed fix** (not applied — it belongs in its own reviewed change, inside the
most delicate function on the branch): after `start_session_and_wait` succeeds,
re-read the desired props (now readable), and if any differ, set them and restart
the session once, still holding native scale for that boot. That is the same
"set + restart" the test's own comment anticipates, moved after the boot; it
costs an extra boot only when the props actually differ.

## 3. Harness: the orchestrator hit the same bug, three layers up (fixed)

With the props pre-seeded, locktask failed again — this time `expected Lock Task
LOCKED`, with `Activity class {…dpc/.LaunchActivity} does not exist` in the log.
The cascade, from the container journal:

```
23:04:05  orchestrator: prop set multi_windows=false   ← after `session stop` → no-op
23:04:08  orchestrator: session start                   ← boots with multi_windows TRUE
23:04:18  test's own preboot: stop + restart            ← because locktask needs FALSE
          → session started from the test binary's minimal env
          → third-party DPC activity unresolvable → no pin → never LOCKED
```

The DPC-resolution artifact was already documented at `799680d`; what was new is
that a silent prop no-op *caused* it. Fixed by setting the prop while the
previous session is still up and **verifying it took**, so the failure is one
line ("prop set is a silent no-op without one") instead of three layers away.
The suite now also warns that it exits leaving `multi_windows=false` with no
session to fix it through — which is exactly what poisoned the earlier cold run.

## 4. Test: the post-stop assertion encoded the pre-`593a904` contract (fixed)

Last failure: `the full-UI Waydroid surface should be gone after stop()`. But
`593a904` deliberately changed that — the full-UI surface *is* Android's display
connection, so destroying it took surfaceflinger and zygote down and the next
launch showed the boot animation. `stop_android` parks it off screen and keeps
the client alive.

The assertion now checks the real contract (present, on `HIDDEN_WORKSPACE`) via
`list_windows` — the same query the adapter uses — rather than a substring match
on the sway tree, which cannot see workspaces. That required re-exporting
`list_windows` and `HIDDEN_WORKSPACE` from `shepherd-host-linux`; the constant is
arguably public contract already, since `sway.conf` hardcodes the same literal
with a comment to keep the two in sync.

## Also fixed

The helper's `USAGE` string never gained `display-size` when `24561f3` added the
verb, so an unknown-subcommand error implies the verb doesn't exist — which is
indistinguishable from the real hazard here (a stale `/usr/libexec` helper that
genuinely predates it). This box *had* a Jul 16 helper installed; it was
reinstalled from HEAD before the runs, so the suite exercised current code.

## Running it again

`persist.waydroid.multi_windows` must be `true` before a cold run, and only a
live session can set it (see #2). The suite says so on exit. The helper at
`/usr/libexec` must also match HEAD:
`sudo ./scripts/integration-tests/setup-waydroid-dev.sh`.
