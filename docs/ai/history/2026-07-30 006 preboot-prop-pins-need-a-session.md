# Preboot's prop pins need a running session (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

## Prompt

> try it and see how often a session restart is actually needed

Following the suite re-run
([2026-07-30 005](./2026-07-30%20005%20waydroid-integration-suite-rerun.md)),
which demonstrated the bug and proposed this fix without applying it.

## The bug

`waydroid prop set` reaches the container's property service **through the
session**. Without one it prints "WayDroid session is stopped" and exits **0**,
so `waydroid::set_prop` reports success. `preboot_waydroid` set its pins
(`persist.waydroid.multi_windows`, `width`, `height`) *before* starting the
session, so on a cold container they silently did nothing.

shepherdd stops the prebooted session at shutdown, so every daemon start begins
session-less: the pins never applied at all. Invisible while the persisted values
already matched; permanent when they didn't.

## The fix

Re-apply after the boot, when the props are actually writable, and boot once more
if anything changed — the props are read at session start, so the running session
still holds the old values.

The boot block became `boot_session_at_native_scale`, so both boots get identical
treatment (scale-1 hold, `display_scale` clear, concurrent restore) instead of
the second one being a hand-copied variant. The corrective restart happens inside
the restart-guard window, and `adopted` now accounts for it (`was_running &&
!restarted && !corrected`), so a corrected boot still claims native scale — we
owned it outright.

A prop that disagrees even after its own restart is logged as a warning rather
than looped on: another boot would not fix it.

## How often the extra restart fires

Measured on the rig (software-rendered VM, cold Android boot ≈ 45 s):

| preboot scenario | corrective restart? | wall clock |
|---|---|---|
| props already correct, warm session adopted | no | **3.4 s / 4.4 s** |
| props stale, cold container | **yes** | **91.09 s / 91.16 s** (two boots) |

Two runs each; the correction cost reproduced to within 70 ms.

It fires only when the *desired* props differ from what is persisted at boot:

- a fresh container (`multi_windows` defaults false),
- a `lock_mode` or `multi_window` config change,
- the locktask path having flipped `multi_windows` for its own session.

The values persist, so the very next start finds them correct and skips it. That
is why test 1 runs at 3–4 s immediately before the poisoned case: same code, same
container, no correction needed.

The cost is not new overhead on a working path — the 91 s case previously never
converged at all. On real kiosk hardware a warm session boot was measured at 4–6 s
earlier in this branch, so the correction there is a fraction of the VM figure.

## Regression test

`waydroid_preboot_fixes_stale_props_on_a_cold_container` encodes the bug rather
than the fix: it poisons `multi_windows` through a live session (the only way to
write it — the bug in miniature), stops the session, and asserts preboot still
reaches the right end state from cold. It prints the elapsed time, which is where
the numbers above come from. Wired in as step 1b of the orchestrator, where test
1 has just left a running session to poison through.

## Verification

Full suite, all five tests, against real Waydroid on HEAD:

```
[OK] preboot: session running, multi_windows enabled                        4.36s
[OK] preboot: corrected a stale prop from cold in 91.157330454s            93.69s
[OK] Android app launched, window present: waydroid.com.android.calculator2 2.11s
[OK] Android session stopped, window gone, Exited emitted
[OK] lock-down active: mDisabled1=0x3210000 (shade/nav disabled)
[OK] locktask: full-UI Waydroid surface up, no per-app toplevel, Lock Task engaged
[OK] locktask: stopped, surface parked off screen, Exited emitted           4.39s
[OK] shutdown: a non-prebooting host left the session alone
[OK] shutdown: the prebooted session was stopped                            0.76s
```

Plus `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all --check`.

### Harness note

The first attempt skipped after step 1b: the extra two boots pushed the run to
four sequential Android boots and the VM's WindowManager wedged (the documented
failure — sessions stop becoming ready). `systemctl restart waydroid-container`
cleared it and the full run passed. Worth knowing that adding this test costs two
boots and brings a lean VM closer to that wall.
