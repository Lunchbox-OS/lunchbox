# A session that outlives its supervisor (issue #172): proposed approach

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/172>
> Depends on: #161 (`crates/shepherd-stated`), which is merged and is what makes
> this possible at all. Ancestors: #144 and its #147/#148/#158, #157.
> Prompt: "suggest an approach for #172", then "build it".
> Status: **built**, and this document is the design it was built from. The one
> load-bearing assumption — that a non-root system user can be granted
> `TerminateSession` — was measured on this host before any of it was written
> (below). What the code does differently from the first draft is recorded in
> "As built" at the end; nothing above it was reversed.

## The defect, stated exactly

Every activity runs as the kiosk uid. So does `shepherdd`. Signal permission is
a uid comparison, so **any activity can `kill` the daemon that supervises it**,
and there is nothing in the kernel's answer that distinguishes a game from its
supervisor.

#161 made the ordinary case safe: sway `exec`s
`sh -c 'shepherdd … || loginctl terminate-session "$XDG_SESSION_ID"'`, so a
shepherdd that *exits* — crash, config error, a casual `pkill` — ends the
session at the greeter. What it did not close, and what #172 is:

* the `sh -c` wrapper is at the kiosk uid too. Kill it *first*, then kill
  shepherdd, and nothing is left to run the fallback. The session stays up with
  no supervisor: no time accounting, no bedtime, no stop-current, no audit.
* `SIGSTOP` does the same thing without killing anything. A stopped shepherdd
  holds all its file descriptors, never exits, and never ticks. The wrapper's
  `||` never fires, because nothing exited.

Both are one line from any activity that can run a shell. The fallback is a
crash handler, not a defence.

## What the fix has to be

The thing that reacts cannot be at the kiosk uid, because everything at the
kiosk uid is killable by everything else at the kiosk uid. It also cannot be
*inside* the session, for the same reason. So: **something outside the session,
at another uid, that notices supervision has stopped and ends the session.**

That is the same conclusion #157 reached about state, one layer over, and it has
the same answer: `shepherd-stated`. It already

* runs as `shepherd-state`, a uid no activity has and nothing in the session can
  signal;
* is started by the system manager on a root-owned socket the kiosk uid cannot
  unlink or rebind;
* has already resolved *which* logind session is the kiosk's, by the
  `class=user` / `type=wayland` / seated / not-`closing` filter, and holds its
  id and scope in `TrustedSession`;
* already admits exactly one cgroup — that session's scope — and refuses every
  other peer at accept;
* already watches `SessionRemoved` for that session and exits when it fires.

Everything needed to notice is there except the noticing. Adding a second
daemon would duplicate all six lines above and give the two of them a way to
disagree about which session is the kiosk's.

## The shape: a dead-man's switch on a connection the child cannot forge

### 1. shepherdd holds a supervision channel

A third connection to the custodian, alongside the store connection and the
`WatchConfig` one, opened immediately after the `Hello` handshake in
`Service::open_state`. A new `StateRequest::Supervise` turns the connection into
a liveness channel the way `WatchConfig` turns one into a change feed — after it
is sent, the client only writes.

**The heartbeat comes from the engine tick, not from a bare timer.** `main.rs`
already runs a 100 ms `tick_timer` that calls `engine.tick`; the heartbeat is
emitted from that loop, rate-limited to one every 5 s. A naked
`tokio::time::interval` task would attest that *a thread* is alive, which is not
the property anyone wants; sending it from the tick attests that the loop
deciding whether a child's time is up is still running. A shepherdd whose engine
has wedged but whose runtime has not is exactly the failure a watchdog is for.

### 2. The custodian arms on the first heartbeat, and nothing on the wire disarms it

Two ways to lose supervision, and they deserve different latencies; two more that
look like losing it and are not (2a):

| observation | meaning | action |
| --- | --- | --- |
| the connection reaches EOF | the process is gone | terminate after a short settle (5 s) |
| no heartbeat within the deadline (60 s) | stopped, wedged, or not ticking | terminate |
| logind removes the trusted session | the session ended on its own | disarm; the daemon already exits here |
| the machine suspends | nobody is ticking, by design | re-arm on resume, below |
| the session did not resolve to exactly one id | there is nothing safe to terminate | stay disarmed; never guess |

**There is deliberately no goodbye message.** An orderly shutdown — SIGTERM,
`Mod4+Shift+Escape`, sway exiting — also ends with shepherdd gone, and in every
one of those cases the session *should* end; that is what the existing `||`
fallback already does, so terminating there is the current behaviour and not a
regression. A disarm message would buy nothing except a thing to spoof: anything
that could send it could send it and then kill shepherdd. Every disarm comes
from **logind** — the session's own end, and a suspend — and none from the wire.

The consequence to write down: **on a device you cannot restart shepherdd in
place.** Killing it ends the session, by design. That is already true through the
wrapper.

### 2a. What must not fire it

The watchdog cannot tell "stopped on purpose" from "did not get scheduled", and
the cost of getting that wrong lands on a child: returned to the greeter
mid-activity, losing whatever the game had not saved, for a reason nothing on
screen explains. That is the failure mode to design against, and it is the
argument for tuning the deadline **loose** rather than tight.

**Suspend and resume.** A sleeping machine is not a wedged one. Both sides would
be on `Instant`/`CLOCK_MONOTONIC`, which on Linux excludes suspended time, so a
night's sleep probably does not blow the deadline by itself — but *probably* is
not a property to ship on a device that sleeps every night, and the two processes
do not get scheduled again at the same moment. So the custodian subscribes to
logind's `PrepareForSleep` and treats it explicitly: **disarm on `start = true`,
re-arm on resume with a full fresh deadline**, so the first heartbeat after a
resume is never late. It is already on the system bus, and `shepherdd`'s
`system_events.rs:173` already does exactly this subscription for the activity
clock (#155) — the same signal, read for a second purpose, not a new dependency.

**An unresolved session.** `resolve` refuses to guess when two sessions match the
filter, and #161 made that refusal cheap — it waits the ambiguity out and stops
cleanly rather than failing. The watchdog inherits that refusal rather than
softening it: **it arms only against a session that resolved to exactly one id,
and terminates only that id.** A `switch user` or an overlapping login is
transient, and a watchdog that guessed during one would log out whoever was next.
No resolution, no arming — the session is simply unsupervised for those seconds,
which is what it already is today.

**A stalled machine.** Swap thrash or a stuck GPU reset can outlast a deadline
with nothing wrong. Nothing distinguishes it from a wedge, so the answer is
margin plus visibility: a deadline generous enough that ordinary stalls stay well
inside it, and a log line for every *near*-miss, so the real margin on a real
device is observable before it costs someone a session rather than after.

### 3. Terminating

`org.freedesktop.login1.Manager.TerminateSession(session_id)` — the resolved id,
never a guess, and the same call `install.sh` already rewrites the sway fallback
to. It needs no compositor socket and no subprocess, so the custodian's "not a
spawner" property survives intact (`clippy.toml` denies bare `Command::new`
workspace-wide; this is a D-Bus call on the connection the daemon already holds).

Escalation, because a terminate can be ignored: call it, wait ~10 s, and if this
process is still alive to notice — it exits when the session goes — call
`KillUser(uid, SIGKILL)` once.

**`KillUser`, not the `KillSession` this first said.** The session scope holds
sway, the launcher, the HUD and swayidle; the activities are elsewhere, and
`2026-08-29 003` measured where: `shepherd-<id>.scope` under the user manager's
`app.slice` for most of them, a snap's or flatpak's own scope for those. So
`KillSession` in the one case the escalation exists for would kill the
compositor and leave the child's game running. `KillUser` reaches both, under
the polkit action the terminate already needs — no second grant, no wider rule.
It does not reach a firewalled Process entry, which the `pkexec` helper puts in
a *system* manager scope; see "Firewalled activities" below. Log each step with the session id and
the reason it fired. Then let the process exit the way it already does when the
session goes away.

### 4. The authority, and its cost

`TerminateSession` from a uid that does not own the session needs polkit's
`org.freedesktop.login1.manage`. Measured on this host (systemd 259, polkit 127),
because the whole approach rests on it:

```
$ sudo -u nobody sh -c 'pkcheck --action-id org.freedesktop.login1.manage --process $$'
Authorization requires authentication and -u wasn't passed.      # exit 2

# with /etc/polkit-1/rules.d/99-probe.rules granting subject.user === "nobody":
polkit\56result=yes                                              # exit 0
```

So it works, and it needs a shipped rules file — the same shape as
`dist/polkit/50-shepherd-firewall.rules`, which is precedent for both the file
and the install path:

```js
// dist/polkit/50-shepherd-session-guard.rules
polkit.addRule(function(action, subject) {
    if (action.id === "org.freedesktop.login1.manage" &&
        subject.user === "shepherd-state") {
        return polkit.Result.YES;
    }
});
```

**Be honest about what this widens.** polkit passes no details for
`TerminateSession`, so the rule cannot be narrowed to one session or one uid: it
grants `shepherd-state` the right to terminate *any* session on the machine,
including an administrator's SSH login. What keeps that acceptable is the shape
of the daemon holding it — no network, no subprocesses, `ProtectSystem=strict`,
one hard-coded call site, and a session id it resolved from logind rather than
took from the wire. It is worth stating in the unit's comments, because it is the
first question a reviewer will ask.

The alternative, if that widening is unacceptable: a second, root-owned daemon
whose entire protocol is "bytes arrived on this connection", parsing nothing.
That keeps the custodian non-root at the price of a second unit, a second socket,
a second copy of the session resolution, and a root process — which is a worse
trade than the rule, but it is the fallback if the rule is refused.

### 5. Say so when the authority is missing

A watchdog that cannot fire is worse than none, because it looks like one. At
startup the custodian should ask polkit `CheckAuthorization` with
`AllowInteraction=false` for its own subject and report the answer — in the
journal, and back to shepherdd in the `Supervise` acknowledgement, so shepherdd
can raise a `Critical` diagnostic beside `StateNotProtected`
(`SessionGuardUnauthorized`, remedy: install the polkit rule). shepherdd is alive
at that moment and has a diagnostics channel; the moment the watchdog is needed,
it does not.

## Firewalled activities

The one kind of activity neither step reaches, scoped out here because it is the
next piece of work rather than a gap to leave unwritten.

### Why it is out of reach

A firewalled Process entry does not run where the others run.
`IPAddressDeny=`/`IPAddressAllow=` are backed by `cgroup_skb` BPF programs, and
attaching those needs `CAP_NET_ADMIN`, so the launch goes
`pkexec → shepherd-firewall-helper → systemd-run --scope` in the **system**
manager. The helper builds this argv (`shepherd-firewall-helper/src/main.rs`):

```
systemd-run --scope --collect --quiet --unit=shepherd-<session-id>.scope \
            --uid=<kiosk> --gid=<kiosk> --property=IPAddressDeny=any … -- <command>
```

The process runs as the kiosk uid, but the *unit* belongs to the system manager
and sits outside `user-<uid>.slice`. So:

| | reaches it? |
| --- | --- |
| `TerminateSession` — stops `session-<n>.scope` | no |
| logind's user GC — stops `user@<uid>.service` and `user-<uid>.slice` | no |
| `KillUser` — kills the user's slice | no |
| `stop_firewall_scope` — `pkexec … stop-scope`, shepherdd's own teardown | **yes, and it is the only thing that does** |

Which is the problem: that last row runs from inside `shepherdd`, and this whole
issue is about `shepherdd` not being there to run anything. A killed daemon
leaves a firewalled activity in a system scope that nothing stops. In practice
it loses the compositor when the session ends and most GUI clients exit on that
— but "most" is not "all", and a non-graphical one simply keeps running, with
its firewall rules and no supervisor.

Worth being precise about the blast radius: this is **pre-existing**, not
something the watchdog introduces. An unclean shepherdd death always left these
behind; before #172 it left the whole session behind with them.

### The fix belongs at creation, not at kill time — and that is what was built

The tempting shape is to give the custodian the authority to stop those units —
and it is the wrong one. `org.freedesktop.systemd1.manage-units` is a far wider
grant than `login1.manage` (stop *any* unit on the machine, not just end a
session), and it would put the scope-naming convention inside a daemon that has
no business knowing what an activity is. Custody, not judgment, and this would
be judgment.

The unit's lifetime should instead be tied to the session's when it is created,
so that the existing `TerminateSession` finishes the job and nothing new has any
authority at all. Two ways, in increasing order of strength:

**A. Put the scope in the user's slice.** `--slice=user-<uid>.slice` on the
helper's `systemd-run`. The helper already has `--uid`, so this is derived, not
passed — nothing new crosses the trust boundary. Then logind's user GC stops it
with the rest of the user's units, and `KillUser` reaches it too, because it is
in the slice `KillUser` kills.

**B. Bind it to the session scope.** `--property=BindsTo=session-<n>.scope` plus
`--property=After=session-<n>.scope`. Stronger: the scope goes when *that
session* goes, rather than when the user's last session goes, so it is also
correct on a device with two kiosk users. The session name has to come from
somewhere, and it must not be an argument — the helper is reachable by anything
in the `shepherd-firewall` group, which is the kiosk user, so an argument is
attacker-chosen. It should be derived from the caller: `sd_pid_get_session()` on
the `pkexec` caller's pid, or the same logind filter the custodian already uses
for the uid it was given. (A caller that *lies* can only weaken its own
activity's lifetime — `BindsTo` is one-way — but deriving it costs little and
removes the question.)

Both landed, in `shepherd-firewall-helper::lifetime_args`. The session for B is
**derived, never passed**: the polkit rule admits the `shepherd-firewall` group,
which is the kiosk user, so an argument would be attacker-chosen. It comes from
the helper's own cgroup — the caller's, inherited through `pkexec`, read before
`systemd-run` moves anything — so a caller can only name the session it is
actually in, and a value that is not `session-<alnum>.scope` is treated as "no
session" rather than guessed at. A caller that could lie would in any case only
shorten its own activity's life; `BindsTo` is one-way.

Measured on this host rather than read, because the whole thing rests on a
system-manager unit being placeable in a user's slice:

```
$ sudo systemd-run --scope --unit=probe.scope --uid=1000 --gid=1000 \
      --slice=user-1000.slice --property=BindsTo=session-2.scope \
      --property=After=session-2.scope -- sleep 30
$ systemctl show probe.scope -p Slice -p BindsTo -p After -p ControlGroup
BindsTo=session-2.scope
After=user-1000.slice session-2.scope
Slice=user-1000.slice
ControlGroup=/user.slice/user-1000.slice/probe.scope     ← inside the user's slice
```

And the dependency that makes the slice do any work is implicit, so nothing has
to declare it:

```
$ systemctl show user@1000.service -p Requires -p Slice
Requires=user-1000.slice sysinit.target
Slice=user-1000.slice
```

A unit `Requires=` its slice, and a stopped slice stops what requires it.

`shepherd-host-linux` needed nothing: the session is derived inside the helper,
so no new argument crosses the `pkexec` boundary and `firewall_helper_argv_prefix`
is unchanged. `stop_firewall_scope` stays as it is — the clean path should still
stop a scope immediately rather than wait for the session to end.

Four unit tests in the helper: both properties present with a session, the slice
alone without one (naming a unit that does not exist would fail the scope's
start, which is an activity that will not launch for the sake of defence in
depth), the session parsed out of a real v2 cgroup line, and every shape that is
not a session scope — an `app.slice` scope, a system service, `session-.scope`,
one with a shell metacharacter in the id — reading as "no session".

### What is left on a device

The teardown itself, which is the half a unit test cannot reach: launch a
firewalled activity, end the session, and confirm the scope is **gone** rather
than parentless. It shares the two systemd behaviours the watchdog itself rests
on — that logind's user GC is prompt with no lingering, and that stopping a slice
stops what is in it — so one device session answers all of it. The `Requires=`
half of that is measured above; the promptness is not.

## What this does not close

* **A device without the custodian.** `--no-state-custodian`, or a packaged
  device where `setup-user` never ran, has no watchdog — same premise as #157,
  and the existing `StateNotProtected` diagnostic already says so.
* **Code already inside the session scope.** The heartbeat is trusted exactly as
  far as the custodian's peer check, which is `#158`'s cgroup compare: anything
  in the kiosk's session scope could keep the watchdog fed after killing
  shepherdd. The launcher, HUD and swayidle live there; an activity does not,
  and getting code into that scope is the same break that would already let it
  read the policy. No new trust, but it is now load-bearing for a second thing.
* **A firewalled activity on a device that has not been verified.** The scope is
  now created inside `user-<uid>.slice` and bound to the session (above), which
  is what makes the terminate and the escalation reach it — but only the
  properties are measured, not the teardown.
* **A shepherdd that ticks but supervises nothing.** The heartbeat rides the
  engine tick, which is a real attestation, but not a proof that the host
  adapter still launches or stops anything. #135's supervision escapes are their
  own tracker.
* **The window before the first heartbeat.** shepherdd opens the channel before
  the launcher can start anything, so there is no activity alive to exploit it,
  but the window exists and should be logged rather than pretended away.

## Order of work

1. `StateRequest::Supervise` + the acknowledgement carrying the polkit answer, in
   `shepherd-state-proto`. Wire-level, testable in `round_trip.rs`.
2. Custodian side: the arm/deadline state machine, behind a `Terminator` trait so
   a test can assert "would have terminated" with no logind. Unit tests for EOF,
   timeout, session-removed, re-arm after a reconnect, **suspend/resume** (a
   deadline that would have expired across a sleep must not fire), and **an
   unresolved session** (never arms, never terminates).
3. The logind implementation of `Terminator`, plus the escalation to
   `KillSession`, and the `PrepareForSleep` subscription that drives 2a.
4. shepherdd side: emit from the tick, reconnect with backoff if the custodian
   restarts (a tight reconnect loop can trip the socket unit's `StartLimitBurst`,
   which #161 already had to widen once).
5. `dist/polkit/50-shepherd-session-guard.rules`, installed by `install.sh` and
   the `.deb` beside the firewall rule; `shepherd install state` verifies it the
   way `install sway-config` verifies its flag strip.
6. The diagnostic, and `docs/INSTALL.md`'s custodian section.
7. **Device verification, which is the only place this is real.** From a plain
   activity: (a) `kill` the `sh -c` wrapper, then `kill shepherdd` → session ends
   at the greeter; (b) `kill -STOP shepherdd` → session ends within the deadline;
   (c) ordinary logout → terminated once, no double-fire, no restart budget
   spent; (d) rule removed → device still boots, diagnostic raised;
   (e) **suspend with an activity running, resume after longer than the
   deadline** → the session survives and the activity is still there, which is
   the false positive that would otherwise reach a child first; (f) **a second
   session** (`switch user`, or a login overlapping a logout) → nothing is
   terminated, and the journal says the session did not resolve. Record the
   measurements here, as #157 did.

   Two of those turn on systemd behaviour that has been *read* rather than
   measured, and both change what (a) and (b) actually kill:

   * **`TerminateSession` kills the session scope even though this host reports
     `KillUserProcesses=false`.** `method_terminate_session` passes `force`, so
     it should stop the scope rather than abandon it — but the property is right
     there on the manager saying the opposite-sounding thing, and it decides
     whether sway dies or is merely orphaned. Check with an activity running:
     the compositor should go, not linger.
   * **logind stops `user@<uid>.service` when the last session goes**, which is
     what actually reaches the activities in `app.slice` — the terminate does
     not touch them. No lingering is enabled anywhere in this tree, so the
     garbage collection should be prompt; confirm the scopes are gone and not
     merely parentless, and confirm what `KillUser` adds when the terminate is
     ignored.

## Two smaller things worth doing with it

* The `Mod4+Shift+Escape` binding and the `||` fallback stay. They are the fast
  path for the common case and cost nothing; the watchdog is the backstop for
  the deliberate one.
* `sway.conf`'s long note ending "Closing that needs a watcher the child cannot
  signal" is the design summary of this document, and should point at it once
  this lands.

## As built

Everything above is what landed. Four things the writing did not know, all in
the "must not fire it" direction:

**A slow boot needed its own grace.** `shepherdd` opens the supervision channel
in `Service::open_state`, partway through a startup that still has a database to
open, a GATT service to register and an HTTP server to bring up — and the engine
tick, which is what a beat attests, runs after all of it. Held to the ordinary
deadline, a slow boot would have been indistinguishable from a killed daemon.
The first beat gets `FIRST_BEAT_GRACE` (120 s) instead, and the client's beat
thread stays silent rather than warning while the tick has never run. Nothing is
unguarded during it: an activity cannot launch before the engine that would
launch it. `a_slow_startup_is_not_a_dead_daemon` and
`a_startup_that_dies_still_ends_the_session` are the pair — the grace is about
*silence*, and an EOF still fires in five seconds.

**The deadline is 60 s, not the ~20 s first sketched**, for the reason §2a
argues and then twice over: the cost of being early lands on a child
mid-activity, and the cost of being late is a minute of a session that is ending
anyway. It is only ever paid by the failures that keep a descriptor open — a
`SIGSTOP`, a wedge — because a kill is an EOF and settles in five seconds, which
is the case an activity would actually reach for. `shepherdd` beats at a quarter
of whatever the custodian says (`SuperviseReply::deadline`, capped at 5 s), so
the constant lives in one crate and the other honours it: 12 missed beats before
it acts.

**`Closed` is a settle, not a fire**, which turned out to do double duty: it is
the reconnect tolerance too. The client reconnects with backoff if its
connection breaks while it is alive (a custodian restart, a write timeout), and
arriving inside the settle cancels it. Without that a transient socket error
would have been a logout.

**The polkit answer travels on the wire.** `SuperviseReply` carries `armed` and
an optional `reason`, and `reason` is present in two cases rather than one:
polkit said no (`armed: false`, `Critical`), or polkit could not be asked
(`armed: true` with a caveat, `Warning`). Not knowing is not the same as knowing
it will fail, and a watchdog that stood down because its self-check did not
answer would be worse than one that tries and logs.

### Where it lives

| | |
| --- | --- |
| `crates/shepherd-state-proto/src/lib.rs` | `Supervise`, `Heartbeat`, `SuperviseReply` |
| `crates/shepherd-state-proto/src/supervise.rs` | the client: one thread, one atomic, beats from the engine tick |
| `crates/shepherd-stated/src/guard.rs` | the pure state machine, its driver, and `Terminator` |
| `crates/shepherd-stated/src/polkit.rs` | the one question, asked once, at startup |
| `crates/shepherd-stated/src/session.rs` | `LogindTerminator`, and `PrepareForSleep` forwarded as events |
| `crates/shepherdd/src/main.rs` | `SessionGuard`, the beat in the tick, `session_not_guarded` |
| `dist/polkit/50-shepherd-session-guard.rules` | the authority, and a comment saying what it widens |

Tests: eleven `Guard` cases as arithmetic against a fake clock, a wire round trip
that ends by *dropping* the client so the server sees the EOF the watchdog turns
on, the polkit rule checked against `STATE_USER` in
`units_match_the_constants.rs` (a fourth fact that cannot be stated once, since
polkit cannot call Rust either), and the two diagnostic contracts in `shepherdd`.

### Still to do on a device

Step 7's measurements. None of the above has run on real hardware yet: a
development stack passes `--no-state-custodian`, so it has no custodian to hold
the other end, and every case that matters — the kill, the `SIGSTOP`, the
suspend, the second session, the missing rule — is a property of a device with
one installed.
