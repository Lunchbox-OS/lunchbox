# Activity supervision escapes on open (#135) and on close (#136)

Investigation of two issues filed against `copernicus` (release 0.3.7) on
2026-08-20:

- **#135 — Steam activity supervision escape on open.** A Steam activity times
  out before its window appears. The launcher drops the spinner and returns to
  the grid, but Steam launches the game anyway. shepherdd and the HUD never
  learn about it.
- **#136 — Process activity supervision escape on close.** A process-based
  activity fails to close on time, and shepherdd's state goes inconsistent.
  Observed while using RetroArch as a `process` entry.

Both are **confirmed from the journal** (`journal-2026-08-20.log`) and the
audit log / usage table in `shepherdd.db`. Journal prefixes are local
(UTC-4); the `tracing` timestamps inside each line are UTC. Local time is used
throughout below.

The investigation also turned up **two further bugs** not in either ticket, one
of which is the direct cause of the #136 incident and one of which fires on
every single launch. Both are written up at the end.

## The shared root cause

`CoreEngine` owns exactly one `Option<ActiveSession>` and there is **no state
between "running" and "gone"**. Both transitions out of a session take the
session unconditionally, without ever confirming that the activity actually
started or actually stopped:

- `CoreEngine::stop_current` (`crates/shepherd-core/src/engine.rs:1263`) —
  `self.current_session.take()` first, settle usage/tokens, return
  `StopDecision::Stopped`. The host has not been touched yet.
- `CoreEngine::notify_session_exited` (`engine.rs:1211`) — same `take()`, driven
  by whatever `HostEvent::Exited` arrives, with no check that the event
  describes *this* session.

Nothing anywhere reconciles engine state against the real world afterwards. The
only reconciliation loop in the tree is `shepherdd/src/display.rs`, for outputs.
`LinuxHost::start_monitor` watches processes it already knows about; it never
asks "is something on screen that no session owns?", even though
`host.list_windows()` returns `pid` per window and would answer exactly that.

So the session's lifetime is decoupled from the activity's lifetime in **both**
directions, and each issue is one direction of that same defect.

## #135 — escape on open (confirmed, 19:34–19:41)

`copernicus` runs `launch_timeout_seconds = 60` (measured: spawn 19:34:33.627 →
warn 19:35:34.350).

```
19:34:33.627  Process spawned pid=86592 program=snap            # snap run steam steam://rungameid/1332010
19:34:33.980  Steam launch process exited pid=86592 code=0      # the launch *request* returns immediately
19:35:34.350  WARN Steam game did not launch within timeout      app_id=1332010 pid=86592
19:35:34.358  Session ended def9f3a1 steam-stray duration_secs=60 reason=ProcessExited{75}
19:35:43.457  Process spawned pid=90522                          # user retries
19:35:59      steam: Fossilize INFO: Setting autogroup scheduling # shader precache — the launch IS progressing
19:36:44.052  WARN Steam game did not launch within timeout      app_id=1332010 pid=90522
19:36:44.086  Session ended fae168f7 steam-stray duration_secs=60 reason=ProcessExited{75}
                                    ---- no session active ----
19:36:59      steam: Fossilize INFO: Setting autogroup scheduling
19:37:01      steam: chdir ".../steamapps/common/Stray"
19:37:01      steam: Adding process 91059 for gameID 1332010     # *** the game launches, 17s after we gave up ***
19:37:02      sway/wlr xwm errors                                # its window maps over the launcher
19:37:30.210  Session started 360032ff cluefinders               # child launches something else ON TOP of it
19:37:51.513  Session ended   360032ff cluefinders duration_secs=21
19:38:50.332  Session started 3c5c6188 steam-stray               # re-launch adopts the orphan by accident
19:41:06.986  Session ended   3c5c6188 steam-stray duration_secs=136 reason=ProcessExited{0}
```

### Why supervision is lost

On deadline, `adapter.rs:215-229` does four things:

```rust
kill_steam_game_processes(app_id, SIGKILL);   // no-op: no such pids, by construction
steam_sessions.lock().unwrap().remove(&pid);  // *** the escape ***
processes.lock().unwrap().remove(&pid);
event_tx.send(HostEvent::Exited { .. code 75 });
```

1. **The kill is structurally a no-op.** The loop only reaches the deadline
   check on iterations where `find_steam_game_pids(app_id)` was empty, so
   `kill_steam_game_processes` returns `false` without signalling anything. The
   watchdog cannot cancel a *pending* Steam launch — `steam://rungameid` has no
   cancel, and the client that will honour it is the long-lived preloaded one we
   deliberately keep alive. The journal shows exactly this: no kill, and Steam
   proceeding through Fossilize to `chdir`/`Adding process` on its own schedule.
2. **Removing `steam_sessions[pid]` while `seen_game == false` is the escape.**
   The monitor's Steam tracking (`adapter.rs:410-425`) iterates only over
   `steam_sessions`, so once the entry is gone the game — whenever it appears —
   matches nothing. The `seen_game` latch also means the "activity ended" branch
   (`else if session.seen_game`) can never fire for it.
3. The game's window is not covered by the `move scratchpad` rules in
   `sway.conf:198-202` (those match the Steam client, not games), so it maps
   over the launcher.
4. With `current_session == None`, `request_launch` approves a second activity
   on top of the escaped game — which is what happened at 19:37:30.

### The real consequence: usage accounting is wrong in both directions

The usage table records `steam-stray = 256s` for the day (60 + 60 + 136). The
game actually ran 19:37:01 → 19:41:06 = **245s**. Of that:

- **120s were charged while the game was not running at all** (both timed-out
  launches — the game was still loading).
- **110s of real play went unmetered** (19:37:01 → 19:38:50, orphaned).

`steam-stray` is an unlimited entry here so nothing was denied, but on a
time-limited entry this is a straightforward limit bypass: launch, wait out the
watchdog, play unmetered until someone re-launches the same entry.

There is also a smaller race: up to ~5 s can pass inside the interstitial
dismiss between the top-of-loop `find_steam_game_pids` check and the deadline
check, and the deadline branch never re-checks before giving up.

## #136 — escape on close (confirmed, 19:48:37–19:48:42)

The reproduction is `tetris` (a `process` entry running `retroarch`), closed by
the user, followed by launching `bitwig-studio`. **The escaped activity is
Bitwig, not RetroArch** — RetroArch did eventually die, but its late exit event
was misattributed and it took the *next* session down with it.

```
19:46:27.395  Process spawned pid=93420 program=retroarch                    # tetris
19:48:37.142  Session stopped 4b005e86 reason=UserStop        # engine session GONE at t=0
19:48:37.201  Killed processes by command name command="retroarch" signal="TERM"
19:48:37.219  Killed processes by command name command="retroarch" signal="TERM"
              # ...RetroArch ignores SIGTERM. Launcher grid is up; retroarch still running, unsupervised.
19:48:40.677  Launch requested entry_id=bitwig-studio         # queued behind the blocking 5s stop()
19:48:42.251  Killed processes by command name command="retroarch" signal="KILL"   # t=5.1s
19:48:42.251  Sent SIGKILL via command name (timeout) command=retroarch
19:48:42.278  Session started 4376577d bitwig-studio          # stop() returned Ok(()); launch proceeds
19:48:42.314  Process spawned pid=93536 program=flatpak com.bitwig.BitwigStudio
19:48:42.338  Process exited - sending HostEvent::Exited pid=93420 signaled=true signal=9   # RETROARCH
19:48:42.340  Host process exited - will end session session_id=611e2ff1...   # fabricated handle
19:48:42.345  Session ended 4376577d bitwig-studio duration_secs=0 reason=ProcessExited{None}
```

Audit log agrees: `bitwig-studio` started 19:48:42.277, ended 19:48:42.344,
`duration 0s`, `exit_code: null` — and `null` is precisely
`ExitStatus::signaled(9)` from RetroArch's SIGKILL. Usage table:
`bitwig-studio = 0`.

Bitwig was left running with no session, no `session_info` (that had already
been wiped by `stop()`), and no UI to close it. Three seconds later the user
shut shepherdd down; the shutdown path's "stop all running sessions" found no
current session and stopped nothing.

### Bitwig was not a coincidence — the phantom launcher hijacked the input

Per the reporter: **Bitwig was launched by accident.** They were still trying to
close Tetris. The first close attempt looked like it had failed (Tetris was
still there), so they pressed again — and by then the launcher grid was up,
interactive, and took the press as "launch the entry under the cursor".

The journal shows both presses and where they landed:

```
19:48:37.130  shepherd_hud::app: Requesting end session          # press 1 → HUD close button
19:48:37.142  Session stopped 4b005e86 (tetris) reason=UserStop
19:48:37.161  shepherdd::hidpi: Restored sway output scales      # launcher restored to normal size
19:48:37.201  ...SIGTERM sent to retroarch (40ms AFTER the above)
19:48:37.447  shepherd_hud::app: Applying HUD scale factor=1.0
19:48:40.677  shepherd_launcher::app: Launch requested entry_id=bitwig-studio   # press 2 → grid
```

Note the ordering in `service.rs:401-424`: `SessionEnded` and `StateChanged` are
broadcast, *then* `hidpi.restore()` runs, and only *then* is `host.stop()`
called. The comment on the restore is explicit about the intent —
"Restore output scale / HUD factor **before tearing down the process** so the
launcher reappears at its normal size". So the launcher is *designed* to come
back before the activity is gone. On the launcher side,
`state.rs:99-109` sets `Connecting` on `SessionEnded` and the following
`StateChanged` drops it straight into a fully interactive `Idle { entries }`.
There is no "closing…" state anywhere.

Three things make the window user-visible rather than theoretical:

- **The press was on a gamepad, and gamepad input does not go through the
  compositor at all.** The launcher polls `gilrs` — raw evdev — on a 16ms
  `glib` timer (`shepherd-launcher-ui/src/app.rs`, `setup_gamepad_input`).
  `Button::Mode` calls `request_stop_current`; `South | East | Start` calls
  `grid.launch_selected()`. Neither was gated on window focus *or* on the
  launcher's own state, and `launch_selected` (`grid.rs:225`) only checks that
  a tile is selected and sensitive. So the second press acted on the grid
  regardless of what was on screen or what had focus. This is the operative
  mechanism, and it is why the fix has to be in shepherdd rather than in
  whichever surface happens to be focused.
- **Window teardown and process exit are different events.** RetroArch tears
  down its window early on SIGTERM and then spends seconds flushing save state
  and config before exiting. shepherd keys supervision off the *process*; the
  user keys their perception off the *window*.
- **`hidpi.restore()` at 19:48:37.161 fires 40 ms before SIGTERM is even sent**,
  rescaling the output while RetroArch's XWayland window is still mapped.

Note that `tetris` had **no** gamepad-bridge sidecar — the only bridge in the
window was spawned for Bitwig at 19:48:42.312. That one then leaked: its pid
93535 was still logging gamepad connect/disconnect at 19:49:50, 64 seconds
after shepherdd exited and under a different user's session. `reap_sidecars`
is keyed on the activity pid, and the misattributed exit reaped RetroArch's
pid (93420), never Bitwig's (93536). So the escape also strands a uinput
virtual pointer+keyboard device across sessions.

So the 5-second gap is not just an accounting hole — it is a window in which the
child is looking at an interactive activity grid while the previous activity is
still alive, and **any input in it launches something**. That escalates #136
from "state goes inconsistent" to "a failed close silently starts an unintended
activity", which is how a supervision escape compounds: the misrouted press
started a session, and the stale exit event then killed it.

It also means fixing the pid-matching alone is not sufficient. That would have
kept Bitwig supervised — but Bitwig would still have been launched by mistake,
and the child would still have faced a phantom launcher over a live activity.

### The two defects that combine here

**(a) `stop()` cannot fail and does not wait for the reap.**
`DefaultManagementService::stop_current` (`service.rs:382-429`) ends the engine
session and broadcasts `SessionEnded` **first**, then calls
`host.stop(&h, Graceful { timeout: 5s })` and discards the result (`let _`).
`LinuxHost::stop`'s graceful arm (`adapter.rs:838-932`) sends SIGKILL at the
timeout and `break`s immediately, **without re-checking that it took**, then
unconditionally removes `session_info`, tears down sidecars, and returns
`Ok(())`. It returns while the process it just killed has not yet been reaped —
the monitor's 100 ms poll (`adapter.rs:352`) finds it 87 ms *after* the next
session has already started.

Note also the 5 s window itself: from 19:48:37.14 to 19:48:42.25 the launcher
grid was showing while RetroArch was still running and the engine had no session
for it. That is a supervision hole even when nothing else launches — and
`tetris` was charged 129 s for a 135 s activity.

**(b) `HostEvent::Exited` is never matched to a session.** The monitor builds a
**fabricated** handle — `HostSessionHandle::new(SessionId::new(), Linux { pid,
pgid })` — with the comment "This will be matched by PID / The service should
track the mapping" (`adapter.rs:399-404`). Nothing matches it.
`Service::handle_host_event` (`shepherdd/src/main.rs:904`) logs
`handle.session_id` and calls `engine.notify_session_exited(...)`, which ends
*whatever session is current*. The mismatch is visible all over the journal —
every `Host process exited - will end session session_id=X` is followed by
`Session ended session_id=Y` with `X != Y`. Usually harmless; at 19:48:42 it
killed a live session belonging to a different process.

This is the defect that makes #136 a supervision *escape* rather than just a
slow close. Fixing (a) alone would shrink the window; fixing (b) is what
prevents the misattribution.

### What did *not* cause it

`Process spawned ... program=retroarch` and both
`Killed processes by command name command="retroarch"` lines confirm
`final_argv[0] == "retroarch"`, i.e. this entry was **not** firewalled. The
firewall-path defects below are real but were not implicated here.

## Other bugs found

### Every launch fails to parse its own response

Present on all 9 launches in the window:

```
ERROR shepherd_launcher::app: Launch failed on server
      error=JSON error: data did not match any variant of untagged enum LaunchOutcome
```

The server's `LaunchOutcome` (`shepherd-management/src/types.rs:9`) has no serde
attribute, so it serializes **externally tagged**:
`{"Approved":{"session_id":…,"deadline":…}}`. The IPC client's mirror
(`shepherd-ipc/src/client.rs:262`) is `#[serde(untagged)]`, which expects the
bare inner object. They can never match.
`shepherd-wire-codegen/src/kotlin_types.rs:23` already documents the correct
shape ("externally tagged (`{"Approved": {…}}`)"), so the Kotlin side is right
and the Rust mirror is wrong.

For an approved launch this is masked: `app.rs:235-256` falls into the `Err`
arm, re-fetches state, sees the session, and carries on. For a **denied** launch
it is not masked — `LaunchOutcome::Denied { reasons }` never decodes, so the
`Ok(Denied)` arm at `app.rs:231` is dead code and the launcher silently drops
back to `Idle` with no explanation. A child who taps an entry that is out of
time or in cooldown sees nothing happen. Worth its own issue.

### Firewalled `process` entries have no working teardown

Not implicated in these incidents, but found while ranking candidates:

- `shepherd-firewall-helper` has a **`stop-scope` subcommand that nothing ever
  calls**. The only `systemctl stop <scope>` teardown available is dead code,
  and the scope name (`make_scope_name(session_id)`, `process.rs:159`) is not
  recorded in `SessionInfo`, so it cannot be reconstructed at stop time.
- `ManagedProcess::command_name` is `final_argv[0]`, which under firewall
  enforcement is `"pkexec"`. So `ManagedProcess::terminate()`/`kill()` run
  `pkill -f pkexec` (`process.rs:846`, `process.rs:881`) — useless against the
  activity and able to signal unrelated `pkexec` invocations. (The adapter's own
  `SessionInfo.command_name` is correct; it is computed from the pre-firewall
  argv at `adapter.rs:569-577`. Only `ManagedProcess`'s copy is wrong.)

## The fix

Teardown is now two-phase, and the session stays *current* for the whole of it.

**`shepherd-core`** — `CoreEngine::stop_current` is replaced by
`begin_stop` / `finish_stop`. `begin_stop` marks `ActiveSession::stopping` with
the reason and hands back the host handle, but leaves the session in place;
`finish_stop` settles usage/tokens, writes the audit record and clears it. Since
`request_launch` already denies while `current_session.is_some()`, holding the
session is what refuses a stray press — at the one layer that covers every input
path, gamepad included. `begin_stop` is idempotent, so a child pressing close
twice re-reports the in-flight stop instead of double-settling.

`notify_session_exited` becomes `notify_activity_exited(&handle, …)` and
compares `handle.payload()` against the session's own (`HostHandlePayload` now
derives `PartialEq`). A non-matching exit is logged and dropped. The
unmatched-teardown path that genuinely has no handle — a spawn that never
produced a process — uses `end_current_session` explicitly.

**`shepherd-management`** — `stop_current` reorders to: `begin_stop` →
`host.stop().await` → `hidpi.restore()` → `finish_stop` → broadcast. The host
error is no longer discarded; a survivor surfaces as `ManagementError::Internal`.

**`shepherd-host-linux`** — `LinuxHost::stop` confirms the kill instead of
assuming it, on both the graceful and force paths, and returns
`HostError::StopFailed` if the activity is still there after
`KILL_CONFIRM_WINDOW`. Liveness is now `pid_is_live()`, which reads
`/proc/<pid>/stat` and treats a zombie as gone — the old check consulted the
`processes` map, which only the background monitor prunes, so it reported
"running" forever in any context without one.

For #135, the watchdog re-checks `find_steam_game_pids` immediately before
declaring failure (the interstitial probe can burn 5s), and instead of deleting
its tracking it hands off to `watch_for_orphaned_steam_game`, which keeps
watching for `STEAM_ORPHAN_WATCH` (180s) and kills a game that turns up late.
It stands down the moment a new session for the same app id exists — otherwise
the child relaunching the entry, which is exactly what they did at 19:38:50,
would have their legitimate game killed by the previous attempt's watcher.

Killing rather than adopting is deliberate: the child has already been told the
launch failed, so nothing should be running. Adopting would mean resurrecting a
session they were told had ended, and billing them for however long Steam took.

**`shepherd-launcher-ui`** — `connect_launch` ignores launches unless the state
is `Idle`. Every input path funnels through it, so this covers keyboard, pointer
and the evdev-polled gamepad at once. The gamepad poll additionally skips nav
and buttons other than `Mode` while a session is up, so grid selection cannot
drift under a running activity.

**`shepherd-ipc`** — the `LaunchOutcome` mirror drops `#[serde(untagged)]` to
match the server's external tagging.

### Verification

`crates/shepherd-management/tests/supervision.rs` — five tests, all of which
failed against the code as it was:

- `session_end_is_not_announced_until_teardown_finishes`
- `hidpi_is_restored_after_teardown_not_before`
- `nothing_can_launch_while_the_previous_activity_is_still_being_stopped`
- `stop_reports_failure_when_the_activity_survives`
- `stale_exit_from_previous_activity_does_not_end_the_next_session`

`MockHost` grew `set_unkillable` / `set_late_reap` to model an activity that
shrugs off signals and one whose reap is only noticed after `stop` returns.
`crates/shepherd-management/tests/launch_outcome_wire.rs` serializes the server
`LaunchOutcome` and decodes it with the client mirror, closing the cross-crate
gap that let the two drift. `pid_is_live` has its own tests, including the
zombie case.

End-to-end, via `scripts/shepherd dev headless` against a fixture activity that
traps SIGTERM, unmaps its window and outlives the 5s deadline
(`docs/ai/history` has no fixture; it lived in the session scratchpad). Before:

```
Session stopped (t=0)                  # engine session gone immediately
launcher: Session ended - setting Connecting
Launch requested entry_id=stubborn     # the stray press launched something
Process exited pid=93420 signal=9
Session started …
Session ended … duration_secs=0        # killed by the stale exit
final: activity running, no session    # orphan
```

After:

```
(t=0 stop issued — no SessionEnded, no "setting Connecting")
(t+1.2s press — no "Launch requested" at all: the gate refused it)
t+5.09 Process exited pid=66138 signal=9
t+5.09 Session ended … reason=UserStop
final: nothing running, launcher idle
```

## Follow-up round

The five items originally left open were then closed too, except RetroArch's own
behaviour (handled by its own PR).

### Reconciliation, and two more escapes it exposed

An activity that survives teardown is no longer forgotten. `LinuxHost` keeps an
`escaped` registry — deliberately *keeping* its `SessionInfo`, which is the
recipe for killing it — and the monitor sweeps it every ~2s
(`RECONCILE_EVERY_TICKS`), re-killing and closing any window it still holds via
the compositor. `HostEvent::ActivityEscaped` reaches shepherdd, which writes an
`AuditEventType::ActivityEscaped` record on the way out *and* when the sweep
finally wins, so a caregiver can see that supervision was lost and for how long.
Sidecars are reaped at the moment of escape rather than left behind — that is
the Bitwig gamepad-bridge leak above.

Building it surfaced two further escapes that the journal had not shown:

1. **The spawned process is not always the activity.** A launcher script that
   backgrounds the real program and exits is reaped within milliseconds, and the
   monitor called that an exit — ending the session under a window that is still
   on screen. Reproduced headlessly with a two-line wrapper fixture:
   `current_session` was `null` while `org.shepherd.VictimActivity` was mapped.
   The monitor now holds the session until the whole *process group* is empty
   (`pgid_is_live`), parking the reaped pid in `winding_down` until then.
2. **The only process-group kill lived on `ManagedProcess`** — which is dropped
   the instant the spawned process is reaped, so a surviving descendant became
   unreachable through it. With (1) fixed, stopping that wrapper activity failed
   honestly (`still running after force kill`) but could not actually kill
   anything. `stop` now signals the group straight from the handle's pgid
   (`signal_group`), which fixes it.

Window-based detection is the backstop for orphans nobody predicted:
`report_unowned_windows` flags any surface whose pid is not a tracked process,
sidecar or known escape, and is not shepherd's own furniture. Verified by
starting a GTK window outside shepherd entirely:

```
WARN Window on screen belongs to no tracked activity
     pid=102158 app_id=Some("org.shepherd.VictimActivity") name=Some("Victim Activity")
```

This is deliberately **report-only** for pids shepherd did not spawn. Closing an
unrecognized window is a policy call with real blast radius — a system dialog,
something an admin started on purpose — and getting it wrong on a kiosk a child
depends on is worse than the visibility gap. Windows belonging to a *known*
escaped activity are closed.

### The Closing affordance

`SessionState::Stopping` (wire change; Kotlin and the web UI regenerated/updated)
is reported by `to_session_info` whenever `ActiveSession::stopping` is set. The
launcher renders `LauncherState::Closing` — the session surface, so the grid
stays out of reach — and the HUD reuses its existing `Ending` state.

The state alone was not enough: `stop_current` blocks the daemon's main loop for
the whole teardown, so a `StateChanged` broadcast has to go out *before*
`host.stop().await` or no client ever sees it. Confirmed on screen:

> **Closing Stubborn…** / "Please wait while the activity closes", with the HUD
> showing "Session ending…".

### Failed launches are no longer billed

`SessionEndReason::LaunchFailed` already existed but nothing produced it for a
stalled launch. The Steam watchdog now emits `HostEvent::LaunchFailed` instead of
a synthetic exit-75, shepherdd routes it to `CoreEngine::notify_launch_failed`
(handle-matched, like every other exit), and `end_current_session` skips usage
and token settlement for that reason. The spawn-error path in `launch` uses the
same entry point. The audit record is still written, so a failed attempt stays
visible — it just does not come out of the child's budget.

### Firewalled process teardown

`SessionInfo` now carries `firewall_scope`, and `confirm_stopped` escalates once
to `systemctl stop <scope>` through the helper's `stop-scope` subcommand — which
had never been called — before declaring the activity escaped. The scope lives
in the *system* manager, so emptying its cgroup reaches processes our own signals
may not. No polkit change was needed: the single action gates the helper binary
as a whole.

`ManagedProcess::spawn` takes an explicit `kill_name` so `command_name` is the
activity's own command rather than `final_argv[0]`, which under firewall
enforcement was `"pkexec"` — both useless against the activity and able to signal
unrelated privileged operations.

### Tests added in this round

- `the_session_reports_itself_as_stopping_during_teardown`
- `a_launch_that_never_started_is_not_charged`
- `pid_is_live_treats_a_zombie_as_gone` / `..._sees_a_running_process_and_a_missing_one`
- `pgid_is_live_sees_a_survivor_after_the_group_leader_exits`

The reconciliation sweep — the largest new subsystem, and the one that had only
ever been exercised incidentally — is covered in `adapter.rs`:

- `reconcile_kills_an_escaped_activity_and_reports_when_it_is_gone` drives the
  whole rescue arc against a **real** process: announce, kill, confirm dead,
  announce resolved, and only then release `escaped` and `SessionInfo`.
- `reconcile_announces_an_escape_once_but_keeps_retrying` uses `kthreadd`
  (pid 2) as the survivor. The kernel discards signals to kernel threads, so it
  is inert even under root while being exactly the shape of the real case:
  demonstrably alive, and it does not die when signalled. Three sweeps must
  produce one announcement and three attempts.
- `unowned_windows_are_reported_once_and_forgotten_when_they_close` and
  `scratchpad_windows_are_not_orphans` cover the window half as pure functions
  (`report_unowned_windows` now returns the newly-reported pids so it is
  assertable rather than log-only).

Both sweep tests were mutation-checked rather than trusted:

| Mutation | Result |
| --- | --- |
| `Self::kill_activity(...)` removed from the sweep | `reconcile_kills_...` fails at "the sweep must actually kill the activity, not just log about it" |
| `entry.reported = true` never latched | `reconcile_announces_once...` fails with `[(2,false),(2,false),(2,false)]` vs `[(2,false)]` |

Firewall teardown is covered by `crates/shepherd-host-linux/tests/firewall_teardown.rs`
(`#[ignore]`d and self-skipping, same convention as `firewall_real*`):

- `stop_scope_tears_down_a_firewalled_activity` spawns a **real** firewalled
  Process activity through `pkexec` → helper → `systemd-run --scope`, asserts
  the transient system scope is active, then calls `stop_firewall_scope` — the
  function that until now had no callers anywhere — and asserts the scope is
  gone and its cgroup took the activity with it.
- `stop_scope_reports_failure_for_a_scope_that_does_not_exist` pins that an
  escalation to nothing reports failure rather than looking like it worked.
- `kill_name_overrides_argv0_for_wrapped_launches` (no privileges needed) pins
  the other half: `command_name` must be the activity, not `pkexec`.

Mutation-checked as well — making `stop_firewall_scope` return `true` without
doing anything fails both tests, and leaves the scope visibly running in
`systemctl list-units 'shepherd-*.scope'`.

Setup on a dev host is `sudo ./scripts/integration-tests/setup-firewall-dev.sh`.
Note that polkit's `subject.isInGroup` reads the user's NSS record rather than
the calling process's supplementary groups, so the grant is live as soon as
`usermod -aG` lands — the "log out and back in" note in the installer applies to
shepherdd needing the *group* at runtime, not to the polkit check.

645 tests pass (plus 3 firewall tests behind `--include-ignored`);
`cargo fmt --check` and `clippy -D warnings` are clean.

## Still open

1. **RetroArch's specific SIGTERM behaviour is unverified** — tracked separately
   (its own PR).
   The headless fixture reproduces the *shape* (window down early, process
   lingering past the deadline); that RetroArch does exactly this was inferred
   from the 5s gap in the journal, not measured.
2. **Unowned windows are reported, not closed.** See the reasoning above; this
   is a deliberate stopping point rather than an oversight. If it should become
   active, the hook is `report_unowned_windows`.
3. **No admin UI for orphans.** `list_windows` / `act_on_window` exist over the
   wire but only as debug methods. The audit log now records escapes, which
   covers "was supervision lost?", but not "show me and let me close it".
