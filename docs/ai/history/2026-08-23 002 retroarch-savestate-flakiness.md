# Save states were being lost about a fifth of the time

Reported during the review of #129: "some flakiness in whether the savestates
are actually saved". Reproduced, root-caused, fixed, and re-measured on
2026-08-23 against a real game (Pokémon FireRed on `mgba`) rather than the
`gba-tests` cartridge, because the test ROM never writes SRAM and its state
barely changes — neither of which exercises what was broken.

## Harness

`shepherd dev headless --gpu` with a one-entry fixture (`capture_child_output`
on, `args = ["--verbose"]`), then per iteration: `launch` over the IPC socket,
wait for the RetroArch window, play 3–7s (varied, to probe whether stopping
early mattered — it does not), `stop_current` graceful, wait for the window to
go, then hash `…/states/mGBA/pokemon-firered.state.auto` and grep that session's
log for `[State] Auto save state`, `[SRAM] Saving RAM`, `Content unloading`.

A hash that does not change *and* a log with no save markers is a real loss: the
child's progress since the previous stop, gone.

## Result

| | saved | lost (`STALE`) | RetroArch crashed at launch |
| --- | --- | --- | --- |
| before | 7/10 | 2 | 1 |
| after (SIGTERM fix) | 10/12 | **0** | 2 |
| after (both fixes) | 6/6 | **0** | 0 |

The third row is a shorter sanity pass, run to confirm the rewritten
`kill_by_command` did not disturb an ordinary stop; the before/after evidence
for the save loss is the first two rows.

## Cause 1: two SIGTERMs, so RetroArch `exit(1)`s without saving

`adapter.rs` sent the group signal twice on the graceful path:

```rust
signal_group(pgid, SIGTERM);          // kill(-pgid, SIGTERM)
if let Some(p) = procs.get(&pid) {
    let _ = p.terminate();            // kill(-pgid, SIGTERM) -- the same call
}
```

`ManagedProcess::terminate` and `signal_group` are the same syscall, and
`pgid == pid` after `setsid`, so both land on the same group microseconds apart.
Upstream's `frontend_unix_sighandler` (`frontend/drivers/platform_unix.c:3782`)
counts them:

```c
unix_sighandler_quit++;
if (unix_sighandler_quit == 2) exit(1);
if (unix_sighandler_quit >= 3) abort();
```

The second delivery exits from inside the handler, skipping the SRAM flush and
the auto save state entirely. It is *intermittent* rather than constant because
standard signals do not queue: when the second `kill` lands while the first is
still pending the kernel folds them into one and the shutdown runs normally.
Whether it does depends on scheduling — hence "flaky".

The tell in shepherdd's own log is the exit status: `code: Some(1)` on a lost
save, `code: Some(0)` after the fix.

This was a regression, not an original defect. `705cb76` (2026-08-15) removed
the extra signals for exactly this reason; `a0ac780` (2026-08-21, on main) added
the unconditional `signal_group` back while fixing something else, framed as
covering the case where the reaped process's map entry is gone. The fix keeps
that intent and the single signal: use the tracked process when its entry
exists, fall back to the handle's pgid only when it does not.

**Why the existing test missed it.** `graceful_stop_lets_the_activity_finish_saving`
drives a shell stand-in, and a shell trap folds two rapid signals into one
invocation — the same coalescing that makes the bug intermittent makes the test
pass. Its own doc comment already flagged that limitation. The stress loop above
is what catches this class; a unit test cannot.

## Cause 2 (latent): `pkill -f <command>` ignores session boundaries

Not what produced the losses above, but found while chasing them.
`kill_activity` — run every two seconds by the reconciliation sweep for as long
as an activity refuses to die — ended with `kill_by_command(command_name,
SIGKILL)`, i.e. `pkill -f retroarch`. That reaches *every* RetroArch the user
owns, including a game the child started seconds later in a different session,
and a `SIGKILL` runs no shutdown path at all. The same unscoped call sat in the
graceful-timeout escalation, the Force path, and `ManagedProcess::kill`.

Same mistake `705cb76` removed from the graceful SIGTERM path ("matching by
command line also reached unrelated copies of the same program running outside
the session"), left in the SIGKILL paths.

Fixed by scoping: `pgrep -f`, resolve each match's process group, skip any pgid
belonging to a session shepherd is still tracking (`LinuxHost::tracked_pgids`),
signal the rest. One group id per session suffices — `setsid` at spawn means
every descendant shares it. `ManagedProcess::kill` loses the by-name kill
outright: it duplicated the adapter's and could never know what to spare.

`kill_by_command_spares_a_tracked_session` covers it, and was mutation-checked —
emptying the protected set fails it on the intended assertion.

## Not a shepherd bug: RetroArch segfaults at launch

The remaining failures in both columns are RetroArch crashing ~400 ms into
startup, before its window maps:

```
Activity exited pid=888083 ExitStatus { code: None, signaled: true, signal: Some(11) }
retroarch[888083]: segfault at fffffffffffff000 ip ... in retroarch[30ec94,...]
```

Three in `dmesg` across the runs, at two different fault addresses, with no
`SIGKILL`/timeout lines anywhere in shepherdd's log — nothing of ours signalled
them. An earlier reading of this pinned it on the timeout `pkill` from the
previous stop, on timing alone; the exit status disproved that.

Unverified on real hardware, and this box is headless GLES2 on llvmpipe, which
the device is not. Worth re-checking there before treating it as anything
shepherd needs to handle.
