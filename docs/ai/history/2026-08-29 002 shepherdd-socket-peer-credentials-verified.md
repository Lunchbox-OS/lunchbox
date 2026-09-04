# Hardening shepherdd's own IPC socket — verifying the design (issue #144)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/144>
> Picks up from: `2026-08-29 001 shepherdd-socket-peer-credentials.md`, which
> scoped the design and left three assumptions unverified.
> Related: #147 (sway IPC), #151/#152 (per-entry firewall cgroups), #143
> (diagnostics).

The scoping note ended with three questions it reasoned from but had not
observed, and asked for them to be settled on a real device before committing
to the design. This note settles them, on `shepherd-26.04-2` (Ubuntu 26.04,
kernel 7.0.0-30, systemd 259), with the firewall helper installed and polkit
granting non-interactively — the shape a device runs in.

**The design survives, but not in the form the earlier note wrote it down.**
Two things it got right were confirmed exactly; one thing it got wrong is the
deny-list phrasing that slipped into its final paragraph, and there is a
deployment precondition it never states which the whole scheme rests on.

## Verdict

* **Q1 — yes, and completely.** A firewalled activity lands in
  `/system.slice/shepherd-<session-id>.scope`; shepherdd and a non-firewalled
  activity share one cgroup string, character for character.
* **Q2 — no.** Every escape from the system-manager scope by direct cgroup
  write is refused, including ones the note thought might be permitted.
* **Q3 — the whole chain is a single pid.** `pkexec` → helper → `systemd-run
  --scope` → activity all `exec` in place. The pid shepherdd already records
  *is* the activity, sitting directly in the scope.
* **New — the user manager is a hole in Q2's floor.** An activity cannot move
  *itself* out of its scope, but it can ask `systemd --user` to *start a fresh
  process for it* anywhere under `user@1000.service`. Confirmed working.
* **New — the design has an unstated deployment precondition.** shepherdd must
  not itself live under `user@1000.service`. In the delegated subtree, any
  process at that uid can join any other cgroup outright. Production satisfies
  this; **the headless dev harness does not**.
* **Better mechanism than the note proposed.** `SO_PEERPIDFD` +
  `PIDFD_GET_INFO` returns the peer's `cgroupid` directly. No `/proc` read, no
  pid-reuse race to reason about, one `u64` compare.
* **Bounded value.** The check covers the local socket only. The HTTP/BLE
  surface is a superset of it and open by default (#156), and policy/usage
  state is file-backed at the same uid (#157). See
  [What this does not close](#what-this-does-not-close-the-other-doors-at-the-same-uid).

## Q1: does a firewalled activity's cgroup differ from the launcher's?

Yes — and the two halves of the answer matter separately.

Booted `shepherd dev headless` against a two-entry fixture (identical
`sleep 600` commands, one with `[entries.firewall]`, one without), launched
each over IPC, and read `/proc/<pid>/cgroup`:

| | cgroup |
| --- | --- |
| shepherdd | `/user.slice/user-1000.slice/user@1000.service/app.slice/ptyxis-spawn-….scope` |
| activity, no firewall | `/user.slice/user-1000.slice/user@1000.service/app.slice/ptyxis-spawn-….scope` |
| activity, firewalled | `/system.slice/shepherd-a8010137-9b15-4727-9b53-cc47c4a7b44f.scope` |

So the firewalled case is not merely distinguishable, it is distinguishable at
the first path segment: `system.slice` versus `user.slice`. And the
non-firewalled case is exactly as bad as the note feared — not "hard to tell
apart" but *the same string as shepherdd's own*. The prerequisite (every
activity gets a scope) is load-bearing, not tidiness.

One bonus the note did not anticipate: `make_scope_name` builds the scope out
of the session id, so the cgroup path *is* the session id. A rejected peer can
be named in the #143 diagnostic as a specific activity rather than "some
process" — no extra bookkeeping needed.

## Q2: can an activity write another scope's `cgroup.procs`?

No, by two independent mechanisms, and the second is the interesting one.

Ran a probe as the activity inside `shepherd-probe-a.scope` and tried to write
its pid into six cgroups. All six refused:

| target | result |
| --- | --- |
| another shepherd scope (`system.slice/shepherd-probe-b.scope`) | `EACCES` |
| shepherdd's own cgroup (`…/app.slice/ptyxis-spawn-….scope`) | `EACCES` |
| `…/user@1000.service` (delegation root) | `EACCES` |
| `…/user@1000.service/app.slice` | `EACCES` |
| the root cgroup | `EACCES` |
| `system.slice` | `EACCES` |

`mkdir` inside its own scope also refused. The scope directory and its
`cgroup.procs` are `root:root 0755/0644`, so **ownership alone** stops the
first, second and last two.

The third and fourth are the ones worth understanding, because those files
*are* `shepherd-dev:shepherd-dev 0644` — writable by the activity's own uid —
and they still refused. That is cgroup v2 **delegation containment**: migrating
a process requires write access to the destination's `cgroup.procs` *and* to
the `cgroup.procs` of the common ancestor of source and destination. Source is
under `system.slice`, destination under `user.slice`, so the common ancestor is
the root cgroup, which is root-owned. **Being placed in a system-manager scope
is a one-way door**, and it is the kernel holding it shut, not file modes.

`setsid` was confirmed to do nothing here, as the note predicted: new session,
new pgid, same cgroup.

### …but the user manager will start a process for it

This is the finding that changes the design, and it is not an exception to Q2 —
the activity still never moves itself. It gets a *new* process created on its
behalf, in a cgroup it chooses:

```
start:  /system.slice/shepherd-probe-c.scope
systemd-run --user --scope …  ->  /user.slice/user-1000.slice/user@1000.service/app.slice/shepherd-escape-a.scope
systemd-run --user …          ->  /user.slice/user-1000.slice/user@1000.service/app.slice/shepherd-escape-b.service
```

It works because the user manager forks the new process inside its own subtree
and moves it within that subtree — common ancestor `user@1000.service`, which
it owns. Containment is never violated; it is simply routed around. All the
activity needs is `XDG_RUNTIME_DIR`, which `INHERITED_ENV_VARS` passes to every
activity and which it must have to be a Wayland client at all.

The *system* manager refuses the same request (`StartTransientUnit` over the
system bus needs `org.freedesktop.systemd1.manage-units`, which polkit denies
without interactive auth), so the escape only ever lands in the user subtree.

This is the same shape as a hazard already documented for the firewall — see
the `terminal` entry's comment in `config.example.toml` and "Firewall caveats"
in `crates/shepherd-config/README.md`, where a GApplication terminal
D-Bus-delegates its window to a long-lived primary outside the scope, making
the firewall a silent no-op. Same mechanism, second consequence.

**What it means for the design.** The note's closing paragraph asks:

> Is the peer's cgroup at or below any scope shepherd created for an activity?
> → reject.

That is a deny-list, which the earlier note's own "The inversion:
allow-list, not deny-list" section rules out —
and this escape is exactly the thing it fails open on. The escaped process is
in *no* shepherd scope, so it passes. The test has to be the allow-list the
rest of the note argues for:

> Is the peer's cgroup **the** cgroup shepherdd itself is in? → accept.
> Anything else, including anything it cannot determine → reject.

Under that test the escape is refused: it lands in a *sibling* of shepherdd's
cgroup, never in it.

## The precondition nobody wrote down: where shepherdd itself lives

The allow-list above is only worth anything if an activity cannot get a process
into shepherdd's *own* cgroup. That is a property of how the session is
started, and it is not the same in production and in dev.

Inside the delegated subtree there is no boundary at all. From a process in
`…/app.slice/attacker.scope`, writing its pid into a *different* existing scope
in `app.slice` succeeds:

```
start: /user.slice/user-1000.slice/user@1000.service/app.slice/shepherd-attacker4.scope
JOINED VICTIM
now:   /user.slice/user-1000.slice/user@1000.service/app.slice/shepherd-victim4.scope
```

Common ancestor is `app.slice`, uid-1000-owned, so containment is satisfied.
Within `user@1000.service`, cgroup identity between processes of the same uid
is **not** a security boundary — it is only a boundary across the
system/user split.

The two deployments differ exactly here:

* **Production** starts the kiosk from a display-manager session
  (`share/wayland-sessions/shepherd.desktop`), so sway — and shepherdd, the
  launcher, the HUD and the CLI one-shots under it — run in the logind session
  scope. On this machine that is `/user.slice/user-1000.slice/session-2.scope`,
  `root:root`, created by the *system* manager and not delegated. Confirmed: a
  uid-1000 process can neither write its `cgroup.procs` nor `mkdir` inside it.
  The allow-list holds.
* **`shepherd dev headless`** starts sway from the invoking shell, so the whole
  stack inherits that shell's scope under `user@1000.service`. Confirmed above.
  There the allow-list is trivially defeated.

So whoever implements this must know two things. The check needs a
production-shaped session to be meaningful, which means an e2e test for it
cannot ride on `dev headless` as it stands — either the harness has to boot the
session into a system-manager scope, or the check needs an explicit dev opt-out
in the same spirit as `--no-harden-sway-ipc`. And a deployment that ever moves
shepherdd into a `systemd --user` unit silently removes the boundary, with
nothing failing to signal it. That is worth an assertion at startup: shepherdd
can read its own cgroup, and can refuse to arm the check (or raise a #143
diagnostic) when it finds itself under `user@…​.service`.

### The interlock with #147

Worth stating because it is easy to undo by accident: sway IPC's `exec` command
spawns its child **in sway's cgroup**, which under the allow-list is the
trusted cgroup. It is a cgroup-forging primitive pointed straight at the one
value that matters. #147's unlink is what takes it away. The peer-credential
check therefore *depends* on the sway hardening rather than merely
complementing it, and re-opening the sway socket re-opens this too.

## Q3: what does the process tree look like through the helper?

It is not a tree. `pkexec` is setuid and `exec`s its target in place, the
helper `exec`s `systemd-run`, and `systemd-run --scope` puts *itself* in the
new scope and `exec`s the command. Four names, one pid, start to finish:

```
bash recorded child pid (what shepherdd's Child holds) = 93471
  PID  PPID  USER      COMMAND
93471 93469  shepher+  /bin/sleep 8
cgroup.procs of shepherd-probe-pid2.scope: 93471
cgroup of 93471: /system.slice/shepherd-probe-pid2.scope
```

Two consequences. The pid in `ManagedProcess` is the activity's real pid and is
already in the scope — no reparenting to pid 1, no `--scope` bookkeeping
process to skip past, and nothing to correlate. And from the activity's side,
its `PPid` is shepherdd, so a PPID walk would work *for a cooperative child* —
which is precisely why it must not be used: it is the first thing a
double-fork breaks, and the cgroup is inherited where the PPID chain is not.

## The mechanism: read the cgroup off the pidfd, not out of /proc

The earlier note proposed `SO_PEERPIDFD` to pin the peer while reading
`/proc/<pid>/cgroup`, and flagged `PIDFD_GET_INFO` as version-dependent and
unverified. It is available here, and it removes the `/proc` read entirely.

`/usr/include/linux/pidfd.h` on 26.04 publishes `PIDFD_INFO_SIZE_VER2` (80
bytes) with a `cgroupid` field, and the ioctl magic is `0xFF`
(`PIDFS_IOCTL_MAGIC`) — not `'p'`, which is the mistake that makes it look
unsupported (`ENOTTY`). Verified over a real `AF_UNIX` peer:

```
PIDFD_GET_INFO ok: mask=0x7 pid=97234 ppid=97232 ruid=1000 cgroupid=11545
  peer cgroup /user.slice/…/ptyxis-spawn-….scope -> st_ino = 11545   (match)
```

`cgroupid` is the cgroup directory's inode number, so the comparison is
`stat("/sys/fs/cgroup" + own_cgroup).st_ino == info.cgroupid` — one `u64`,
cached at startup. Nothing is parsed, nothing is looked up by pid, and the
pid-reuse race the earlier note spends a section on does not arise, because no
step ever goes through a pid.

`nix 0.29` (the workspace pin) has neither `SO_PEERPIDFD` nor this ioctl, so
this is a small `libc`-level addition in `shepherd-ipc`.

### It fails closed on a peer that has already exited

Worth knowing before it looks like a bug. If the peer exits before the check
runs, `SO_PEERCRED` still hands back its (now meaningless) pid, but
`PIDFD_GET_INFO` returns `ESRCH`:

```
live peer:            mask=0x7 pid=97285 cgroupid=11545
exited peer (reaped): PIDFD_GET_INFO failed: ESRCH   (SO_PEERCRED pid=97286, /proc gone)
```

That is the correct direction — an unclassifiable peer is refused — and it is
also the concrete reason not to fall back to `/proc`: the fallback is precisely
the pid-reuse race. But it constrains clients: **any client that connects,
writes and exits without waiting for its reply will be rejected
non-deterministically.** Every `shepherd-launcher-ui` one-shot blocks on its
response today, so this is currently fine, and it is a real invariant to write
down rather than a hypothetical.

## What this does not close: the other doors at the same uid

The check protects the *local socket*. It is one of three mechanisms that have
to compose, and on the default configuration it is not the one that is leaking
hardest. None of this is an argument against doing it — it is an argument
against describing it as "the management surface is now protected". The other
two doors are tracked as #156 and #157 and are deliberately **out of scope for
the #144 implementation**.

### The HTTP surface is a superset of the socket, and open by default (#156)

`config.example.toml:152-164` ships the management API with `enabled = true`,
`bind = "0.0.0.0"`, and `auth_token` commented out, and
`crates/shepherd-http/src/auth.rs`'s `AuthSources::is_open` passes every
request straight through when there is neither a static token nor a claimed
BLE admin. That surface covers daily overrides, token adjustments and config
reload — more than the local socket exposes. An activity reaches it with
`curl http://127.0.0.1:8080/…` and never touches the socket the cgroup check
guards.

Authentication does not by itself fix this at a shared uid. Once a BLE admin
claims the device, the minted HTTP token is persisted to `<data_dir>/admin.toml`
(`crates/shepherd-config/src/policy.rs:452`); a static `auth_token` lives in
`config.toml`. Both are files owned by the uid the activities run as, so a local
activity reads the credential and presents it as any remote caller would.

This is the counterexample to the scoping note's parenthetical that `http_token`
"authenticates *remote* HTTP and BLE callers, who are not at this uid and have
no `/proc` access". That reasoning is correct for callers who really are remote,
and does not hold for an activity that can read the token off disk and then dial
loopback. The same argument applies to the BLE transport's admin record.

What actually keeps an activity off that port is the per-entry firewall
(#151/#152) — and the example config's firewalled entry hands it straight back
with `allow = ["127.0.0.0/8"]` (`config.example.toml:1014-1020`), while most
entries carry no firewall at all. Worth revisiting when #156 lands: the loopback
allow is what makes the local HTTP port reachable from inside a filtered
activity.

### Policy and usage state are files at that uid (#157)

Quota and usage live in `<data_dir>/shepherdd.db` (`crates/shepherdd/src/main.rs:175`),
policy in `config.toml`, both writable by the activity's own uid. Gating
`adjust_tokens` and `upsert_override` on the socket while the database behind
them is writable is the bearer-token argument turned around: **at a shared uid,
file-backed state is exactly as exposed as a secret.**

File permissions cannot separate them, for the same reason a token file cannot:
shepherdd runs at that uid and must write those files. The only mechanism that
closes this class is separating shepherdd's uid from the activities' — which
would also make #144 itself dissolve, since the existing uid check would then
mean what it says. That is #157's territory, not this one's.

#### Added 2026-09-02: the environment is a second door to the same state

Found while auditing what an activity can set in shepherdd's environment (see
`2026-08-29 004`, which closed the #144-shaped half of that). These are recorded
here rather than fixed there, because they are the same problem #157 already
describes and they dissolve under the same fix — with separate uids an activity
cannot set shepherdd's environment at all.

- **`SHEPHERD_DATA_DIR`** points the daemon at a different store. Verified live:
  the daemon creates and uses an env-supplied data dir. So it is not only that
  `shepherdd.db` is *writable* by the activity's uid — the activity can hand the
  daemon a fresh one, and usage accounting starts from zero. Same effect as
  deleting the database, by a route that needs no write access to it.
- **`SHEPHERD_SOCKET`** moves the management socket. Less interesting since
  clients now verify the daemon's cgroup, but it is the same shape.

Both resisted the #144 treatment for a reason worth writing down: unlike the
development switches, these are not clap-only bindings. `shepherd_util::paths`
(`default_socket_path`, `default_data_dir`) reads them directly, so they are
honoured by the daemon *and* every client, and `run-dev` and the e2e harness
both `export` them. Deleting the `env =` attribute from shepherdd's args would
have made them look closed while leaving them live — worse than leaving them
documented. Closing them means changing how every binary resolves its paths and
deciding what a dev session and `shepherd-admin` do instead.

Also on this surface, lower severity: **`SHEPHERD_LIBRETRO_DIR`** and
**`SHEPHERD_RETROARCH_CONFIG_DIR`**. Both document a production use ("installs
that put cores/config somewhere unusual"), so unlike their sibling
`SHEPHERD_RETROARCH_ROOT` they could not simply be gated — they want to become
`config.toml` settings, which is this issue's surface too, since that file is
writable at the same uid.

### So what does the socket check uniquely buy?

The runtime effects that have no on-disk or HTTP equivalent —  `stop_current`,
`logout`, `set_screen_power`, brightness and volume — plus defence in depth on
everything that overlaps with #156 once that surface is closed. It removes a
one-line exploit available to every activity today, and it is a prerequisite for
the other two being worth anything: closing HTTP while the socket stays open
would just move the adversary one socket to the left.

## What changes in the design, in one list

1. The peer test is `peer_cgroupid == shepherdd_own_cgroupid`, cached at
   startup. Not "is it in an activity scope".
2. Get `cgroupid` from `SO_PEERPIDFD` + `PIDFD_GET_INFO`, not from `/proc`.
   Refuse on any error.
3. Keep the prerequisite exactly as scoped: every activity gets a
   system-manager scope, whatever its kind, whatever its firewall config.
   Without it, activities sit in the accepted cgroup.
4. Add a startup check that shepherdd's own cgroup is outside
   `user@<uid>.service`, and refuse to arm (or raise a #143 diagnostic) if it
   is not — otherwise dev and any future user-unit deployment look hardened
   and are not.
5. Give the check a dev opt-out, or teach `dev headless` to boot into a
   system-manager scope. As it stands the harness cannot exercise it.

## What was implemented

Built on top of these measurements, in the same change. #156 (HTTP hardening by
default) and #157 (everything else at the shared uid) are deliberately left
alone — see [What this does not close](#what-this-does-not-close-the-other-doors-at-the-same-uid).

**The check** (`crates/shepherd-ipc/src/peer.rs`). `SO_PEERPIDFD` +
`PIDFD_GET_INFO` for the peer's `cgroupid`, compared against shepherdd's own,
read the same way at startup. No `/proc` in the decision path. Refusal happens
at accept and just drops the connection — a refusal that answers is a refusal
that can be probed. The verdict is an allow-list, so the user-manager escape
above is refused for the right reason: it is not *in shepherdd's cgroup*, which
is a different question from whether it is in a scope shepherd made.

Root is accepted from any cgroup. Without that, `sudo shepherd …` from an
operator's own login session would be locked out — their session scope is never
shepherdd's.

**The prerequisite** (`process.rs`, `adapter.rs`). Every activity now launches
outside shepherdd's cgroup. The measurements made this much cheaper than the
scoping note assumed: snap and flatpak already land in
`user@<uid>.service/app.slice` (which is what `apply_firewall_to_existing_scope`
has always relied on), Steam is preloaded as a snap and its games inherit that,
and firewalled Process entries already get the helper's system-manager scope.
That left plain Process entries, which now go through
`systemd-run --user --scope --collect` — **unprivileged**, no helper and no
polkit, because all this has to achieve is "not shepherd's cgroup", not the BPF
attach the firewall needs. Confirmed on a live session: a plain entry lands in
`…/app.slice/shepherd-<session>.scope` and a firewalled one still lands in
`/system.slice/shepherd-<session>.scope`.

**The precondition, checked rather than assumed.** At startup shepherdd reads
its own cgroup path and reports `IpcSocketNotHardened` (`Critical`) if it is
inside `user@<uid>.service`, where the check cannot be a boundary. Verified by
running the daemon from a shell: it says so. The same diagnostic covers a host
where activities cannot be isolated, since an activity sharing shepherdd's
cgroup is accepted by the allow-list — one condition, two causes, because the
consequence for the device is identical.

**A refused peer is reported**, not just logged: `IpcPeerRejected` names the
peer's cgroup, read best-effort from `/proc` *after* the decision. Since a scope
is named after its session id, that names the activity that went looking.

**The dev opt-out** is `--no-restrict-ipc-peers`, following
`--no-harden-sway-ipc` exactly: carried by `sway.conf`, stripped by
`shepherd install sway-config` with a check that fails the install if it
survives, asserted present by `headless.sh`, and passed by the e2e harness.

### What was deliberately not done

**No per-method role tiers.** The scoping note wanted a session-control tier
below admin. `ClientRole` turns out to be consulted nowhere — `can_launch` and
friends are dead code — and, more to the point, once the allow-list is in place
every accepted peer is either root or shepherd's own code. A tier split would
have nothing to defend against and would be the sort of protection that reads as
one without being one. `ClientRole` still rides on `ClientInfo` for the audit
log, unchanged.

**Failures leave the session up.** Both new failure paths (cannot read our own
cgroup; cannot isolate an activity) launch anyway and raise a diagnostic, which
is the trade `harden_compositor_socket` already makes and the opposite of
#152's. The difference is what is at stake: an unfiltered activity is a hole in
the network policy, while an unisolated one is a hole in a control surface that
is already reachable over HTTP (#156). Losing every activity on a host with no
user manager is not worth it.

### Testing

Unit tests in `shepherd-ipc` cover accept, refusal, root, and the disarmed
path — they can put a peer in a foreign cgroup without a session — plus the
ioctl request number and struct size, which are the kind of constant whose
mistake shows up as "unsupported kernel" rather than as a compile error. The
shepherdd tests cover which outcomes report and what the reports say.

End to end, against a hand-started daemon with the check armed:

| peer | verdict |
| --- | --- |
| same cgroup as shepherdd (launcher / HUD / one-shot) | accepted |
| `systemd-run --user --scope` (a plain activity, post-change) | refused |
| helper's system scope (a firewalled activity) | refused |
| root, from a foreign cgroup (`sudo`) | accepted |

The harness cannot host this test: `dev headless` puts the whole stack in the
launching shell's cgroup, so it can neither pass the check meaningfully nor fail
it honestly. Recorded in `CONTRIBUTING.md` and the `headless-dev` skill rather
than left for the next person to rediscover.

## Reproducing

Everything above is a shell probe against the installed helper; nothing needed
a device-specific setup beyond `scripts/integration-tests/setup-firewall-dev.sh`
having been run. The shape was:

```sh
# an "activity", exactly as shepherdd spawns one
pkexec --keep-cwd /usr/libexec/shepherd-firewall-helper apply-process \
  --scope-name shepherd-probe.scope --uid 1000 --gid 1000 --default allow \
  --env "XDG_RUNTIME_DIR=/run/user/1000" -- /path/to/probe.sh
```

and, for the premise itself — reaching the socket from inside that scope and
getting served:

```
caller cgroup: 0::/system.slice/shepherd-probe-ipc.scope
  health        -> {"result":{"ok":{"host_adapter_ok":true,"live":true,…}}}
  service_state -> {"result":{"ok":{"api_version":1,…}}}
```

The role derived at accept is `Admin` and is never consulted. That was read out
of the code in the earlier note; it is now observed.
