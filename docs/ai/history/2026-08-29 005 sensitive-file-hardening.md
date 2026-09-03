# Sensitive file hardening — investigating the third door (issue #157)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/157>
> Builds on: `2026-08-29 002 shepherdd-socket-peer-credentials-verified.md`,
> whose "What this does not close" section named this as one of the two
> remaining doors at a shared uid. To be implemented **on top of** #158.
> Related: #144/#148 (the two sockets), #156 (HTTP auth), #105 (per-activity
> filesystems).

#144 closed the compositor's socket and #158 closed shepherd's own. Both are
about *runtime* effects. This one is about the state those effects operate on:
policy, quota, usage, and the BLE admin record are ordinary files in the child
user's home, owned by the uid every activity runs as.

Measured on `shepherd-26.04-2` (Ubuntu 26.04.1, kernel 7.0.0-30, systemd 259),
the shape a device runs in.

## Verdict

* **The exposure is worse than "modify some files".** Config is *hot-reloaded*
  from a directory watch, so a write by an activity takes effect within seconds
  with no restart and no RPC. Observed: an activity appended an entry with
  `max_run_seconds = 0` and launched it under supervision with no deadline.
* **Landlock is out.** It works, and it is unprivileged and irrevocable — but it
  requires `no_new_privs`, which breaks `snap-confine` (file capabilities) and
  `pkexec` (the firewall-helper launch path), and it is escaped outright by
  asking `systemd --user` to start a process. All three measured below.
* **AppArmor closes both doors** — the files *and* the `systemd --user` escape —
  and costs no `no_new_privs`. But the single rule that closes the escape
  (`deny … StartTransientUnit`) also breaks `snap run`, which uses that call for
  its own scope. Measured.
* **Every per-process confinement has the same shape of hole**: at a shared uid,
  any unconfined same-uid service that will start a process on request hands the
  attacker a fresh, unrestricted one. Closing them is a **deny-list of escape
  routes**, which is the shape #158 deliberately rejected for the socket.
* **The allow-list answer is to move the state across a uid boundary** and gate
  it with the peer check #158 already built. That is the smallest slice of uid
  separation that actually closes #157, and it needs no policy about
  third-party software at all.
* **The cgroup boundary it depends on is now measured, not assumed** — on an
  installed kiosk on this machine, not reasoned from a dev shell. shepherdd and
  every trusted component sit in the root-owned logind session scope; activities
  sit in `app.slice` or `system.slice`; the activity could neither join the
  session scope nor manufacture one. This also settles #158's own open hardware
  question: both hardenings armed with no diagnostic raised.

## What is actually exposed

All at `shepherd-dev:shepherd-dev`, mode 0644/0664 — the uid activities run as.

| file | contents | what a write buys |
| --- | --- | --- |
| `~/.config/shepherd/config.toml` | every entry, limit, availability window, firewall spec, `management_api.auth_token` | total policy control, **applied live** |
| `<data_dir>/shepherdd.db` | `usage`, `token_balances`, `cooldowns`, `snapshot`, `daily_overrides`, `audio_outputs`, `settings`, `audit_log` | reset today's usage, mint tokens, erase the audit trail |
| `<data_dir>/admin.toml` | BLE admin record + the minted HTTP token (`policy.rs:452`) | admin on the HTTP surface as any remote caller |
| `<data_dir>/.factory-reset-ble` | factory-reset sentinel | forge/suppress a reset |
| `~/.local/state/shepherdd/` | daemon log, `sessions/` child logs | erase evidence |

`<data_dir>` defaults to `~/.local/share/shepherdd` (`shepherd-util/src/paths.rs`).

**A sixth way in, and it does not need write access to any of the files above.**
`2026-08-29 004`'s finding 1 shows that `pam_env`'s `user_readenv=1` — enabled on
every GDM service on stock 26.04 — lets an activity write `~/.pam_environment`
and choose the *next* kiosk session's whole environment. `paths.rs` reads
`SHEPHERD_DATA_DIR` and `SHEPHERD_SOCKET` from that environment with no gate, so
an activity can simply *move* the state rather than edit it.

The canonical write-up is now the *"the environment is a second door to the same
state"* subsection of `2026-08-29 002`, added by `d445c98`, which assigns the
residual to this issue — a pointer rather than a copy, so the two cannot drift.
Two things to add to it from re-measuring on the installed kiosk after #144's
fixes landed:

* It is not only that usage restarts from zero. **`admin.toml` and the
  reset sentinel default to `<data_dir>/…` too**, so the same variable decides
  where the BLE admin record and the minted HTTP token are written — into a
  directory the activity named, and can therefore read and pre-create.
* `shepherd harden apply` strips `user_readenv=1`, so a hardened device is not
  exposed this way; an unhardened one is, and hardening is still documented as
  optional.

Any fix for this issue has to pin the protected paths as well as protect them.

### The config watch makes it immediate

`shepherdd/src/main.rs:984-1010` installs a `notify` watcher on the config
file's **parent directory** and reloads on any `Modify`/`Create` naming the
config path. There is no reload RPC to gate and no restart to wait for.

## Observed, not inferred

Against a hand-started `shepherdd` (own config, own data dir, HTTP API on
loopback), acting as a plain process at the same uid — no socket, no
credential, no root:

```
# 1. usage and the audit trail
usage rows before: [('sleeper', '2026-08-29', 6)]
   sqlite3: update usage set duration_secs = 0
usage rows after : [('sleeper', '2026-08-29', 0)]
   sqlite3: delete from audit_log        # audit_log: 6 rows -> 0
shepherdd reports: [{"date":"2026-08-29","duration_seconds":0,"entry_id":"sleeper",…}]

# 2. policy, via a plain append to config.toml
   >> [[entries]] id = "attacker-shell"  command = "/bin/sh"  max_run_seconds = 0
shepherdd (3s later, unprompted):
   INFO shepherd_core::engine: Policy reloaded entry_count=2
   INFO shepherdd: Config reloaded entry_count=2

# 3. launch it
   launch {"id":"attacker-shell"}
   -> {"Approved":{"deadline":null,"session_id":"86c573af-…"}}
   129065 /bin/sh -c sleep 9999
```

`deadline: null` — supervised by shepherd, with no time limit, from a file
write. This is the whole issue in three commands.

## The kernel this has to work on

| | |
| --- | --- |
| active LSMs (`/sys/kernel/security/lsm`) | `lockdown,capability,landlock,yama,apparmor,ima,evm` |
| Landlock ABI | **8** |
| `CONFIG_BPF_LSM` | `y` — but **`bpf` is not in the active LSM list** |
| `kernel.apparmor_restrict_unprivileged_userns` | `1` |
| `kernel.yama.ptrace_scope` | `1` |

Two consequences worth recording before any design:

* **BPF LSM is not available** without a kernel command-line change (`lsm=…,bpf`).
  A cgroup-keyed `lsm/file_open` program would have composed beautifully with
  #158 — it is off the table for an appliance we do not want to reboot into a
  custom cmdline.
* **yama = 1 already stops the obvious sibling attack**: an activity cannot
  `ptrace` shepherdd to read the token out of its memory. Only descendants are
  traceable, and shepherdd is the ancestor, not the descendant.

## Landlock: works, and still cannot be used

Landlock is the mechanism that *should* fit — unprivileged, inherited across
fork and exec, irrevocable, no install-time root step. A probe that grants the
whole filesystem except one subtree (`landlock_create_ruleset` + a rule per
sibling along the excluded path, then `landlock_restrict_self`):

```
-- read excluded:   Permission denied
-- write excluded:  Permission denied
-- list excluded:   Permission denied
-- normal read /etc/os-release:   ok
-- normal write in home:          ok
-- exec a program:                ok
```

Three findings killed it anyway.

### 1. `no_new_privs` breaks snap and pkexec

`landlock_restrict_self` requires `PR_SET_NO_NEW_PRIVS`. Under it:

| | result |
| --- | --- |
| `pkexec` | `pkexec must be setuid root` — **the firewall-helper launch path** |
| `snap run firefox` | `snap-confine is packaged without necessary permissions … required permitted capability cap_dac_override not found` |
| `bwrap --ro-bind / /` | works (user namespaces, not file capabilities) |

`snap-confine` on 26.04 is not setuid — it carries file capabilities
(`cap_dac_override,cap_sys_admin,…=p`), which `no_new_privs` neuters just the
same. Firefox ships as a snap on Ubuntu; a Snap-kind entry is a first-class
entry kind. `bwrap` surviving suggests flatpak and Steam's pressure-vessel are
fine, but flatpak is not installed here and that is **unverified**.

`pkexec` is recoverable — apply Landlock *after* the privileged part of the
chain, inside the helper — but snap is not.

### 2. The `systemd --user` escape defeats it

The hole #158 found in its own floor is fatal here rather than merely awkward.
From inside the Landlock domain:

| route | result |
| --- | --- |
| `systemd-run --user --scope -- cat <protected>` | **denied** — `--scope` execs in the caller's own process, so the domain is inherited |
| `systemd-run --user -- cat <protected>` (service) | **`SECRET-DB-CONTENT`** — the manager forks a fresh, unrestricted process |
| raw `StartTransientUnit` over the session bus, `ExecStart=/bin/cp <protected> /tmp/…` | **file copied out** |

Landlock has no hook that can stop this: the escape is a D-Bus method call, and
the process that reads the file was never in the domain. `XDG_RUNTIME_DIR` is in
`INHERITED_ENV_VARS` (`process.rs:294`) and must be, for a Wayland client.

### 3. The exclusion is expressed by enumerating siblings

Landlock rules only grant; there is no deny. "Everything except `~/.config/shepherd`"
means granting each sibling of `shepherd` under `~/.config`, and so on up the
path — so `~/.config` itself is never granted, and an activity can no longer
create `~/.config/<newapp>` on first run. That is a real behaviour change in the
directory with the most churn in a home directory. (Also: granting a *regular
file* with directory-only rights returns `EINVAL`; a real implementation has to
mask rights per target type. One exclusion under `/tmp` produced 56 rules.)

This third point is an argument for consolidating shepherd's state into one
short, dedicated path regardless of which mechanism wins.

## AppArmor: closes both doors, and collides with snap

No `no_new_privs`, real `deny` rules, and — decisively — it mediates D-Bus.
A deliberately permissive profile (`capability, network, unix, dbus, userns,
/** mrwlkix,`) with two denies, loaded with `apparmor_parser` and entered with
`aa-exec -p shepherd-activity`:

```
-- am i confined:                shepherd-activity//&unconfined (enforce)
-- read protected:               Permission denied
-- write protected:              Permission denied
-- normal read / normal write:   ok
-- exec keeps profile:           shepherd-activity//&unconfined (enforce)
-- systemd-run --user (service): Failed to start transient service unit: Access denied
-- raw StartTransientUnit:       Call failed: Access denied
-- pkexec:                       works
```

Both Landlock failures are answered: the escape is blocked at the D-Bus layer,
and `pkexec` still works because nothing set `no_new_privs`. Exec inherits the
profile (`ix`), so the confinement is sticky through the activity's own children.

**But `snap run` uses `StartTransientUnit` too**, to create its own scope:

```
error: cannot create transient scope: DBus error "…AccessDenied":
  An AppArmor policy prevents this sender from sending this message …
  label="shepherd-activity//&unconfined (enforce)"
  member="StartTransientUnit" destination="org.freedesktop.systemd1"
```

AppArmor's D-Bus rules match bus, path, interface, member and peer label — not
arguments. There is no rule that permits "a scope for yourself" and refuses "a
service running a command of your choosing", because the difference is in the
call's arguments.

There is a way out, and it is worth stating because it is cheap: **strict snaps
are already confined by their own profile.** From inside the firefox snap,
`$HOME` is remapped and the host config directory is simply not there:

```
$ snap run --shell firefox -c 'cat ~/.config/shepherd/probe-config.toml'
cat: /home/shepherd-dev/snap/firefox/common/.config/shepherd/…: No such file or directory
```

So Snap-kind entries could take a laxer profile and lean on snapd's, while
everything else takes the strict one. Classic-confinement snaps remain a gap.

### The other doors this design would then owe an answer for

Once the boundary is "confine the activity, and deny the routes that leave the
confinement", every such route has to be enumerated. Present on this box:

* `cron` is **active** and `crontab` is installed — a user crontab is a job run
  by a root daemon, unconfined. Closable in the profile (deny the binary and the
  spool), but it has to be *remembered*.
* Session D-Bus activation: `io.snapcraft.Launcher`, portals, `dconf`, GameMode.
  Activation goes through `systemd --user` on 26.04, so the same rule covers it —
  which is a nice property, and also a reminder that the coverage is incidental.
* The firewall helper is an "exec my own argv, as my own uid, in a fresh
  **system** scope" door for any activity (it is careful: `--uid` must equal
  `$PKEXEC_UID`, uid 0 refused — `shepherd-firewall-helper/README.md`). Harmless
  to #158's allow-list and to file access, but it is a spawn route, and a
  deny-list design has to keep noticing routes like it.

That list is the problem. **This is a deny-list**, in exactly the sense #158's
write-up used when it explained why "refuse peers I recognise as activities"
fails open. It fails open on the route nobody thought of, and the last two
releases have each turned up one.

## Mechanisms ruled out on the way

* **File permissions, ownership, ACLs.** shepherdd runs at the uid it would be
  defending against. There is nothing to express.
* **A setgid group** (files `root:shepherd-state 0660`, group dropped before
  exec'ing an activity). `newgrp`/`sg` do not exist on 26.04, so a dropped
  supplementary group is not trivially regained — but the group has to come from
  *somewhere*, and the only candidate is a setgid `shepherdd`. An activity can
  exec that binary too, and `shepherdd --data-dir <real>` with a crafted config
  is a write primitive. A setgid binary that is also the thing being protected is
  not a boundary.
* **A mount namespace hiding the paths.** Unprivileged user namespaces are
  restricted by AppArmor here, the mounts would have to be made before privilege
  is dropped, and — decisively — the `systemd --user` escape lands in the *host*
  mount namespace, where the files are exactly where they always were.
* **BPF LSM keyed on cgroup.** Not in the active LSM list; needs a kernel
  cmdline change.
* **Integrity checking rather than prevention.** Any key shepherdd can read at
  that uid, an activity can read at that uid. This is the bearer-token argument
  again.

## The production cgroup shape, measured

The recommendation below rests on a claim #158 reasoned about but never booted:
that shepherdd lands in a cgroup no activity can join. That is now measured, on
an **installed** kiosk on this machine — `shepherd install all --user kiosk`,
release binaries at `/usr/local/bin`, `/etc/sway/shepherd.conf` with its
production flags (no `--no-harden-sway-ipc`, no `--no-restrict-ipc-peers`), the
kiosk user's own `~/.config/shepherd/config.toml`.

The one deviation from a device is that the session was started with
`su - kiosk` running sway on the headless wlroots backend rather than by GDM on
a seat. That changes the session's *class* (`background` instead of `user`) and
nothing about its *placement*: GDM's own wayland session on this box sits at
`/user.slice/user-1000.slice/session-2.scope`, and its whole descendant tree
(`gdm-session-worker` → `gdm-wayland-session` → the session) stays there.

| process | cgroup |
| --- | --- |
| sway, **shepherdd**, launcher, HUD, `swayidle`, `swaybg` | `/user.slice/user-1001.slice/session-c7.scope` |
| plain activity | `/user.slice/user-1001.slice/user@1001.service/app.slice/shepherd-<session-id>.scope` |
| firewalled activity | `/system.slice/shepherd-<session-id>.scope` |
| the user manager | `/user.slice/user-1001.slice/user@1001.service/init.scope` |

The session scope's directory and `cgroup.procs` are `root:root` — `0755`/`0644`.
Every trusted component shares it; no activity is in it.

### Both existing hardenings armed, unprompted

This also settles #158's "worth confirming on a device". From inside a real
activity, launched by shepherdd through the normal path:

| | |
| --- | --- |
| `service_state` diagnostics | no `ipc_socket_not_hardened`, no `compositor_not_hardened` |
| the management socket | present, `srw-rw---- kiosk kiosk` — and `Connection reset by peer` |
| sway's socket | gone (`no such file`) |

### What the activity could and could not do

| attempt | result |
| --- | --- |
| write its pid into shepherdd's `session-c7.scope/cgroup.procs` | `Permission denied` |
| `mkdir` inside that scope | `Permission denied` |
| write into the `user@1001.service` delegation root | `Permission denied` |
| logind `CreateSession` (manufacture a session scope) | `Access denied` |
| **system** manager `StartTransientUnit` | `Access denied` (polkit, non-interactive) |
| `systemd-run --user` (service) | **worked** → `user@1001.service/app.slice/sh157-escape.service`, read `config.toml`, db writable |
| the firewall helper (`pkexec`, activity is in `shepherd-firewall`) | **worked** → `/system.slice/shepherd-probe-escape.scope`, euid 1001, read `config.toml` |
| `config.toml`, `shepherdd.db` | `r=Y w=Y` |
| create `admin.toml` in the data dir (`drwxrwxr-x kiosk kiosk`) | **wrote it** — the BLE admin record can be forged *before it exists* |
| append an entry to `config.toml`, then `get_entry` | returned it — hot-reloaded, live |

Two of these matter beyond confirming what was already argued.

**The escape routes exist, and both land outside the session scope.** The user
manager gives an activity a fresh unconfined process; the firewall helper gives
it an arbitrary command in a system scope at its own uid. Neither can be aimed
at `session-c7.scope` — a user unit cannot leave `user@.service`'s subtree
(`Slice=session-c7.scope` is refused: "not a slice"), and the helper's scope is
created by the system manager under `system.slice`. So both are *relocation*
doors, not *placement* doors.

**`admin.toml` did not have to exist to be exposed.** The data directory is
group- and world-readable and owner-writable at the activity's own uid, so the
exposure covers files shepherdd has not written yet.

### The mechanism a broker would use is already in the tree

`peer.rs` documents that `PIDFD_GET_INFO`'s `cgroupid` *is* the cgroup
directory's inode number. Confirmed against the live session:

```
shepherdd pid=147548  PIDFD_GET_INFO cgroupid = 52173
session-c7.scope dir inode                    = 52173
```

So a service running as another uid, outside the session, can resolve the
trusted cgroup without guessing: ask logind for the kiosk's session scope,
`stat()` it under `/sys/fs/cgroup`, and compare the inode against
`peer_cgroup_id(fd)` from the existing `crates/shepherd-ipc/src/peer.rs`. No new
mechanism, no `/proc` in the decision path.

## What actually closes it

The one property none of the above has: a boundary the escape routes do not
cross *because they run as the wrong user*, not because they were denied.

### Recommended: move the state across a uid boundary, gate it with #158's check

> Designed in full in
> [`2026-08-30 001 shepherd-state-broker-design.md`](./2026-08-30%20001%20shepherd-state-broker-design.md),
> which settles the open decision below and gives the build order.

A small service — call it `shepherd-stated` — runs as its own uid
(`shepherd-state`, **not root**) under the system manager, owns a single
protected directory (`/var/lib/shepherd`, mode 0700, its own uid), and serves
shepherdd over a Unix socket in `/run/shepherd/`. The socket applies the peer
check from `crates/shepherd-ipc/src/peer.rs`.

Why this is the allow-list version of the same argument:

* Every escape route in this document produces a process at the **child's** uid.
  `/var/lib/shepherd` at mode 0700 owned by another uid refuses all of them by
  DAC, with no policy to maintain and nothing to enumerate.
* The socket is the only way in, and the section above measures the boundary on
  an installed kiosk: the activity could not join the session scope, could not
  manufacture one, and both routes that *did* work put it in `app.slice` or
  `system.slice` instead — outside the trusted cgroup, refused for the right
  reason.
* Snap, flatpak, Steam, Chrome and every future activity keep working
  **unchanged**. There is no third-party-software policy, no profile to update
  per Ubuntu release, no `no_new_privs`.
* It is the first slice of the uid separation that #144's write-up already
  identified as the only complete fix — applied to the asset that matters,
  without having to move Wayland, PipeWire, D-Bus and `$HOME` across the
  boundary at the same time.

Shape of the work, in landable pieces:

1. **Consolidate the protected paths into one directory**, and stop `data_dir`
   being freely configurable for the protected members. Today the five files sit
   across three XDG roots; any mechanism needs one stable name. Mechanism-
   independent, and worth doing even if the rest is deferred.
2. **The broker and the DB.** `Store` is already `Arc<dyn Store>`
   (26 methods, all small value types) — `SqliteStore` moves into the broker and
   shepherdd gets a client implementation. This is the bulk of the work and the
   bulk of the value: usage, tokens, cooldowns, overrides, audit.
3. **config.toml and admin.toml** through the same socket, including the change
   notification that the directory watch provides today. Note that config must
   be *read* through the broker too, not just written — `auth_token` lives in it.
4. **Degrade loudly**, following #144 and #158 exactly: a diagnostic when the
   broker is unreachable or the peer check cannot be a boundary, an opt-out flag
   carried by `sway.conf` and stripped by `shepherd install sway-config`, and the
   e2e harness passing it.

**The one design decision the measurements leave open.** The broker is a
different service from shepherdd, so it cannot compare against *its own* cgroup
the way `peer.rs` does; it needs a rule for which cgroup is the trusted one. The
session scope is the right answer, but the rule must be "**the** session scope
shepherdd is in", not "any session scope" — a second login session for the same
user would get its own `session-<n>.scope` and pass a loose rule. Two things
keep that narrow, and both should be used rather than either alone:

* Resolve the session through logind and require the *graphical* one —
  `Class=user`, `Type=wayland`, `Seat=seat0`, which is what GDM's session on
  this box reports and what a device runs. The `su`-created session in these
  measurements was `Class=background`, and would be refused by that rule.
* `shepherd harden apply --user kiosk` already denies the kiosk user SSH and
  console login, which is what stops a second session existing in the first
  place. That makes hardening load-bearing for this design in a way it was not
  before — worth saying out loud in `INSTALL.md`, where it is currently
  described as optional.

An activity could not manufacture a session scope by any route tried
(`CreateSession` and the system manager both refused), so the residual is
"someone logs the kiosk user in a second way", not "an activity forges one".

### Cheaper interim, if the above is too big for now

Ship the AppArmor profile: strict for everything, lax for Snap-kind entries
(leaning on snapd's own confinement), plus denies for `crontab` and its spool.
It measurably removes the exploit demonstrated at the top of this document, it
fits the install model the project already has (`/etc/sway/`, polkit rules, the
bluetoothd drop-in), and it degrades the same way the other two hardenings do.

It should be described honestly as what it is: a deny-list over spawn routes,
which will need revisiting whenever the desktop stack grows another one.

### The end state

Activities run as a **different uid** from shepherdd. #144 dissolves (the uid
check in `server.rs` would mean what it says), #157 dissolves (0600 is enough),
and it converges with #105's per-activity filesystems, since a separate uid
wants a separate `$HOME` anyway. The cost is everything the shared uid currently
buys for free: the Wayland socket, PipeWire, the session bus, `XDG_RUNTIME_DIR`.
Worth its own issue rather than a paragraph in this one.

## What this does not close

The same caveat #158 carried, unchanged and now the last one standing: the HTTP
management surface ships `enabled = true`, `bind = "0.0.0.0"` with `auth_token`
commented out (#156). Protecting the file the token lives in does not matter
while the API accepts requests without one, and an activity reaches it over
loopback. #157 and #156 have to land for either to be worth much.

## Reproducing any of this

Everything above was produced with three throwaway probes (a Landlock
allow-all-but-path wrapper, a `no_new_privs` exec wrapper, and a permissive
AppArmor profile with two denies) plus a hand-started `shepherdd` on a loopback
port. Nothing was left loaded: the profile was unloaded with `apparmor_parser -R`
and the daemon and its data directory were removed.
