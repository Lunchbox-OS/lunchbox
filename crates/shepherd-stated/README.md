# shepherd-stated

Custodian for shepherd's policy and state, at a uid the activities do not have
(issue [#157]).

## The problem it exists for

`shepherdd` runs inside the kiosk session, as the same uid as every activity it
launches — and so do its files. On a stock install:

| file | contents |
| --- | --- |
| `~/.config/shepherd/config.toml` | every entry, limit, availability window, firewall spec |
| `<data_dir>/shepherdd.db` | usage, token balances, cooldowns, daily overrides, the audit log |
| `<data_dir>/admin.toml` | the BLE admin record and the minted HTTP token |
| `<data_dir>/.factory-reset-ble` | the factory-reset sentinel |

All of them are writable by the software the device is supervising. This was
measured on an installed kiosk, not inferred — an activity reset today's usage
to zero, wiped the audit log, appended an entry with `max_run_seconds = 0` to
the policy, and launched it under supervision with no deadline. The config is
watched for changes, so the policy edit took effect in about three seconds with
no restart and no RPC.

Two of those are the *device's* rather than a user's. The admin record and the
reset sentinel — and the unbond queue that shadows the record — live in a shared
`/var/lib/shepherdd/admin/`, while the policy and the database live under
`state/<user>/`. There is one Bluetooth adapter and one BlueZ bond table, so
there is one admin record; a claim scoped more narrowly than the bond it names
cannot be kept honest. `ProtectedFile::scope` is where the rule is written, and
every instance of this daemon runs as the same uid, so they all reach the shared
directory.

File permissions cannot separate them, because `shepherdd` runs at the uid it
would be defending against. Neither can per-process confinement: Landlock needs
`no_new_privs`, which breaks `snap-confine` and `pkexec`, and both it and
AppArmor are routed around by asking `systemd --user` to start a process — which
hands back a fresh, unconfined one at the same uid. All measured; see
[`2026-08-29 005`].

## What it does instead

Moves the files to a uid the activities do not have, and serves them to
`shepherdd` over a socket that admits exactly one cgroup.

```
┌───────────────────── system manager ─────────────────────┐
│ shepherd-stated@kiosk.service     User=shepherd-state    │
│   owns  /var/lib/shepherdd/state/kiosk/   0700           │
│   reads /run/shepherdd/state/kiosk.sock   (fd from PID 1)│
└──────────────────────────▲───────────────────────────────┘
                           │ accepted: the session's cgroup
┌────────── session-<n>.scope (root-owned) ────────────────┐
│ sway → shepherdd → the one connection above              │
└──────────────────────────────────────────────────────────┘
┌────── app.slice / system.slice: every activity ──────────┐
│ no path to the directory, no accepted connection         │
└──────────────────────────────────────────────────────────┘
```

Every route an activity has to get a process somewhere else — `systemd --user`,
the `pkexec` firewall helper — produces a process at the **child's own uid**, so
`0700` on another uid's directory refuses all of them without having to know
they exist. That is the property confinement does not have.

## The peer check

The trusted cgroup is the kiosk's **logind session scope**, resolved from logind
and compared against `SO_PEERPIDFD` + `PIDFD_GET_INFO` — the same one-`u64`
comparison `shepherd-ipc`'s `PeerPolicy` makes for `shepherdd`'s own socket, via
`PeerPolicy::for_cgroup`. What differs is only where the number came from: this
daemon is deliberately outside the session, so it cannot compare against itself.

The filter is `Class=user`, `Type=wayland`, **a seat**, and **not `closing`**.
Measured on a device running GDM, the kiosk uid's sessions are:

| session | class | type | seat | state | |
| --- | --- | --- | --- | --- | --- |
| the kiosk, started by GDM | `user` | `wayland` | `seat0` | `active` | trusted |
| its user manager | `manager` | `unspecified` | — | `active` | refused |
| an SSH login | `user` | `tty` | — | `active` | refused |
| the previous session, after a display-manager restart | `user` | `wayland` | `seat0` | `closing` | refused |

`Class=user` alone would admit the SSH row, which is a second way into the same
uid rather than the session shepherd runs in. The last row is the one only a
device showed: after `systemctl restart gdm` the outgoing session was still
listed, still seated, and still owned a live cgroup, while logind had it as
`closing`. Two sessions matched, the daemon refused to guess — correctly — and
the custodian would not start at all.

`online` is accepted alongside `active`: a kiosk whose VT is switched away is
still the session shepherd runs in.

Refusing is measured too: an activity in
`…/app.slice/shepherd-<session-id>.scope` connects, is refused at accept before
a byte is parsed, and the log names it by its session id.

## Why the service manager owns the socket

[`2026-08-29 004`]'s finding 2 showed an activity can `unlink()` `shepherdd`'s
own management socket and bind an impostor in its place — possible because that
socket lives in a directory at the child's uid, where no file mode helps. Its
conclusion was that preventing it needs the socket created by something other
than the daemon, "i.e. systemd socket activation with root binding it and
passing the fd, which `shepherdd` cannot use while sway `exec`s it".

This daemon can, so it does. `/run/shepherdd/state/` is root-owned; the kiosk
uid cannot take the name. Socket activation also means nothing runs until
`shepherdd` connects.

The socket's own mode is `0666`, and deliberately so: connecting to a Unix
socket needs *write* permission on the inode, and `shepherdd` is at a different
uid. **The peer check is the gate; the file mode is not.**

## It also ends the session when nothing is supervising it (issue [#172])

Every activity runs as the kiosk uid, and so does `shepherdd`. Signal permission
is a uid comparison, so an activity can `kill` its own supervisor — and the
`sh -c` wrapper that turns a dead `shepherdd` into `loginctl terminate-session`
runs at that uid too, so killing it first defeats it. `kill -STOP shepherdd`
defeats it without killing anything at all: a stopped daemon never exits, so the
wrapper's `||` never fires, while the engine that counts a child's time has
stopped.

This daemon is the only part of shepherd that is outside the session, at a uid
nothing inside it can signal, and it already knows which session is the kiosk's.
So it holds the dead man's switch.

```
shepherdd ──Supervise──▶ custodian     "will anything happen if I die?"
          ◀──armed────   (asks polkit once, at startup)
          ──Heartbeat──▶ every 5s, emitted from the engine tick
              …
          ╳ killed       EOF        ─▶ settle 5s  ─▶ TerminateSession
          ╳ SIGSTOPped   no beats   ─▶ deadline 60s ─▶ TerminateSession
                                       still there after 10s ─▶ KillSession
```

**The beat comes from the engine tick**, the same 100 ms loop that decides
whether a child's time is up — not from a timer of its own, which would attest
only that *a thread* is alive. A `shepherdd` whose engine has stopped and whose
runtime has not is exactly the failure a watchdog is for.

**Nothing has to be sent for it to fire.** A killed process closes its
descriptors whether it meant to or not; the heartbeats exist only for the
failures that keep the descriptor open. There is deliberately **no goodbye
message**: every orderly shutdown also ends with `shepherdd` gone and the
session ending anyway, and a disarm message would be a thing to spoof — send it,
then kill the daemon. Every disarm comes from logind, none from the wire.

### What must not fire it

A watchdog that fires when nothing is wrong costs a child their session
mid-activity, for a reason nothing on screen explains. Three cases are handled
rather than left to the deadline:

| | |
| --- | --- |
| **a suspend** | logind's `PrepareForSleep` stands the guard down and re-arms it on resume with a *full* deadline, so the first beat after a resume is never late |
| **a slow boot** | the first beat gets two minutes rather than the deadline's one: this channel opens while `shepherdd` is still starting, and its engine tick runs after that. Nothing is unguarded meanwhile — an activity cannot launch before the engine that would launch it |
| **a reconnect** | losing the connection starts a five-second settle rather than firing, so a client that comes back cancels it by arriving |

A fourth is structural: the guard is never armed against a session that did not
resolve to exactly one id. `resolve` refuses to guess between two, and a
watchdog that guessed during a `switch user` would end whichever session was
next.

Every one of those is a `Guard` unit test, driven as arithmetic against a fake
clock — the state machine has no socket, no bus and no clock of its own, which
is what makes "a suspend that outlasts the deadline" a test rather than a
device.

### The authority, and what it widens

`TerminateSession` from a uid that does not own the session needs polkit's
`org.freedesktop.login1.manage`, granted to `shepherd-state` by
`dist/polkit/50-shepherd-session-guard.rules`. polkit passes no details for that
call, so the grant **cannot be narrowed to one session**: it covers any session
on the machine, an administrator's SSH login included. What keeps it acceptable
is the shape of the thing holding it — no network, no subprocesses,
`ProtectSystem=strict`, one call site, and a session id resolved from logind
rather than taken from the wire.

The daemon asks polkit at startup whether it is allowed, and the answer travels
back on the `Supervise` reply. Without the rule the device still boots and
everything else still works; `shepherdd` raises the `Critical` diagnostic
`session_not_guarded` and the journal names the file. That check exists because
a watchdog that cannot fire is worse than none: it is the shape that looks like
protection.

## What it is not

* **Not root.** It opens a SQLite file it owns, reads TOML it owns, and asks
  logind a read-only question. A parser bug yields `shepherd-state`, which holds
  exactly the files this protects and nothing else.
* **Not a policy engine.** It never learns what a limit means or whether a child
  may launch something; that stays in `shepherd-core`. Custody, not judgment.
* **Not a spawner.** No subprocess at all, which keeps the `$PATH`-substitution
  class ([`2026-08-29 004`], finding 1) away from it entirely — the session
  watchdog ends a session over D-Bus rather than by running `loginctl`, so that
  stays true.
* **Not able to end a session by itself.** It asks logind, and logind asks
  polkit. Remove the rule and the daemon keeps every other job it has.

The unit backs that up with sandboxing — verified applied on 26.04, where the
running service has no capabilities, `NoNewPrivs: 1`, loopback-only networking,
and no view of `/home` at all.

## Running it

Installed and enabled by `shepherd install state --user <kiosk-user>`, which
`shepherd install all` calls. Per-user, as a systemd template:

```sh
systemctl status shepherd-stated@kiosk.socket   # always up, owns the name
systemctl status shepherd-stated@kiosk.service  # runs once shepherdd connects
journalctl -u shepherd-stated@kiosk.service     # what it trusted, and what it refused
```

To run it by hand (development), `--socket` binds a path directly rather than
taking one from the service manager. That is not how a device should run it: the
name is then bound by the daemon, at a uid the kiosk user shares.

A logout ends the trusted session, so the daemon exits and socket activation
starts a fresh one on the next connection, which resolves whatever session
exists then. Nothing keeps serving against a cgroup that no longer exists.

**Both ordinary endings exit 0** — the session went away, or nobody was logged
in — and that matters more than it looks. Exiting non-zero for "no session yet"
was measured taking the *socket* unit down: five quick failures hit systemd's
start limit, after which every connection for the rest of the boot was refused
and `shepherdd` ran on an unprotected local store. Its fallback is a
startup-only decision, so a transient race became a downgrade lasting until the
next reboot. The daemon now waits up to 20s for the session to appear and then
stops quietly, leaving the socket armed.

## Status

**The database, the policy and the BLE admin record are all protected**,
verified on an installed kiosk after a clean boot: `shepherdd` connects to the
custodian, the exploit that opened this issue is refused at every step, and the
device's existing state is migrated rather than abandoned.

That migration is reversible: `shepherd uninstall state --restore-to-home` moves
the database and the admin record back to `~/.local/share/shepherdd/` before
removing the units. Without it a downgrade to a build that predates this daemon
would find an empty home, start from zero usage, and — with no `admin.toml` —
report itself unclaimed, which is a factory reset by another name. The forward
migration moves rather than copies, so the way back has to be a command rather
than an assumption.

```
shepherdd:      State served by the custodian; it is not reachable by activities
activity:       ls /var/lib/shepherdd/state/kiosk/   -> Permission denied
activity:       open the database                    -> unable to open database file
activity:       reset today's usage                  -> unable to open database file
activity:       connect to the state socket          -> Connection reset by peer
```

With the custodian stopped, a device that *has* one refuses to start: its state
has moved here, so the alternative is an empty database and a launcher with no
activities, which looks like an ordinary quiet evening rather than a fault. The
session ends at the greeter and the journal carries the reason.

A device that never had a custodian still comes up — nothing has moved — and
reports `state_not_protected` (`Critical`) carrying the underlying reason. That
is the trade #144 makes twice: an unprotected kiosk beats a dead one *when the
protection was never there to lose*.

### There is one policy, and a signpost

`config.toml` is **moved** into the protected directory, and a signpost takes
its place at `~/.config/shepherd/config.toml` naming where it went and how to
change it. One file decides what a child may do, and it is the one at a uid no
activity has.

The earlier design kept a real copy at the home path — the seed migration read,
and the fallback `shepherdd` reads when the custodian is unreachable — with a
`policy_diverged` diagnostic to notice when the two drifted apart. It worked,
and it was worse: two files that look equally authoritative, only one of which
decides anything, and a whole diagnostic whose job was to report the confusion
the arrangement created. Removing the second copy removed the need for it.

The signpost still has to parse, because the fallback still reads that path. It
is a valid policy with **zero entries**, which is the right thing to grant when
the custodian cannot be reached: a device with no activities and a `Critical`
`state_not_protected` is visibly not working, and that is honest. A stale policy
that still launches games would be a device that looks fine and is not.

Two ways to set a policy, both landing on the same file and reloading within a
second — `sudoedit /var/lib/shepherdd/state/<user>/config.toml`, or
`shepherd install policy --user <user> --source PATH`. The second is what a
hardened device needs: `harden apply` leaves the kiosk user with `nologin` and
no SSH, so there is nothing to `su` into. Both validate first — a policy this
daemon serves and shepherdd cannot parse is fatal at *startup* (tolerated only
on reload), which on a device is a session that ends rather than a message
someone reads.

### What is still open

Nothing in this issue's scope. The daemon's *tracing log* is still in the kiosk
user's home, deliberately — it is diagnostic rather than authoritative, and the
audit trail that matters is the `audit_log` table, which moves with the
database. Tamper-evident logs want journald, which is its own piece of work.

Beyond it: the management HTTP API still ships open by default (#156), and it
reaches every effect this protects. Closing one without the other moves the
adversary one socket to the left.

[#157]: https://git.armeafamily.com/albert/shepherd-launcher/issues/157
[#172]: https://git.armeafamily.com/albert/shepherd-launcher/issues/172
[`2026-08-29 004`]: ../../docs/ai/history/
[`2026-08-29 005`]: ../../docs/ai/history/
