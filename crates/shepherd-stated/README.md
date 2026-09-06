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

## What it is not

* **Not root.** It opens a SQLite file it owns, reads TOML it owns, and asks
  logind a read-only question. A parser bug yields `shepherd-state`, which holds
  exactly the files this protects and nothing else.
* **Not a policy engine.** It never learns what a limit means or whether a child
  may launch something; that stays in `shepherd-core`. Custody, not judgment.
* **Not a spawner.** No subprocess at all, which keeps the `$PATH`-substitution
  class ([`2026-08-29 004`], finding 1) away from it entirely.

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

With the custodian stopped, the session still comes up and the device reports
`state_not_protected` (`Critical`) carrying the underlying reason — the trade
#144 already makes twice, and never silent.

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
[`2026-08-29 004`]: ../../docs/ai/history/
[`2026-08-29 005`]: ../../docs/ai/history/
