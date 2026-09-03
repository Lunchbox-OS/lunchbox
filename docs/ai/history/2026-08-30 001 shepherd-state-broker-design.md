# `shepherd-stated`: moving policy and state across a uid boundary (issue #157)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/157>
> Investigation this implements: `2026-08-29 005 sensitive-file-hardening.md`,
> which measured the exposure, ruled out Landlock and AppArmor, and measured the
> boundary this design rests on.
> Depends on: #158 (`crates/shepherd-ipc/src/peer.rs`). To be built on top of it.
> Related: #144/#148 (the two sockets), #156 (HTTP auth), #105 (per-activity
> filesystems).

The investigation ended with a recommendation and one open decision. This is the
design: what gets built, in what order, and what each piece is allowed to assume.

## The one-sentence version

A small service running as its own uid owns shepherd's policy and state files in
a directory the child's uid cannot open at all, and hands them to shepherdd over
a socket that admits exactly one cgroup — the logind session scope, which #157's
measurements showed an activity can neither join nor manufacture.

## Why this shape and not confinement

Recorded here because it is the question a reviewer will ask first, and the long
answer is in `2026-08-29 005`:

* Every escape route measured in a real kiosk — `systemd --user` service mode,
  the `pkexec` firewall helper — produces a process at the **child's own uid**.
  Confinement (Landlock, AppArmor) has to deny each route by name and fails open
  on the next one; a uid boundary refuses all of them without knowing they exist.
* Landlock additionally cannot be used at all: it requires `no_new_privs`, which
  breaks `snap-confine` and `pkexec`.
* AppArmor can close the routes, but the one rule that closes the `systemd --user`
  escape (`deny … StartTransientUnit`) also breaks `snap run`, which uses the
  same call for its own scope.
* This design needs **no policy about third-party software**. Snap, flatpak,
  Steam, Chrome and everything added later keep working untouched.

It is also the first slice of the uid separation that #144's write-up called the
only complete fix — applied to the asset that matters, without moving Wayland,
PipeWire, D-Bus and `$HOME` at the same time.

## The boundary, as measured

From `2026-08-29 005`, on an installed kiosk (`shepherd install all --user kiosk`,
production sway config, real logind session):

| process | cgroup |
| --- | --- |
| sway, **shepherdd**, launcher, HUD, `swayidle`, `swaybg` | `/user.slice/user-1001.slice/session-<n>.scope` |
| plain activity | `…/user@1001.service/app.slice/shepherd-<session-id>.scope` |
| firewalled activity | `/system.slice/shepherd-<session-id>.scope` |

The session scope's directory and `cgroup.procs` are `root:root`. An activity
could not write into it, could not `mkdir` in it, could not reach it through the
user manager (`Slice=session-<n>.scope` — "not a slice"), and could not
manufacture a session of its own (logind `CreateSession` → `Access denied`; the
system manager's `StartTransientUnit` → polkit refusal).

**Everything below depends on those five facts and nothing else.**

Two later commits on #158's branch make the first row sturdier than it was when
those measurements were taken, and both are worth knowing:

* `7463f1b` closed the two cases where something shepherd started was *not*
  outside shepherd's cgroup — the Steam preload, and `yt-dlp`, which runs on a
  background timer parsing whatever a remote host returns. "Nothing shepherd
  starts for an activity is ever in shepherd's cgroup" now holds because of what
  `adapter.rs` does rather than because snapd happened to move the process fast
  enough.
* `crates/shepherd-ipc/README.md` now enumerates the trusted set in full, which
  is the set this broker inherits: sway, shepherdd, the launcher, the HUD,
  `swayidle`, sway's keybinding one-shots, the input-compat sidecars,
  `wl-mirror`, the pairing overlay, and the short-lived query commands
  (`wpctl`/`pactl`/`amixer`, `pw-dump`, `brightnessctl`, `pgrep`, `pkcheck`).

That second point is a real granularity choice and should be made deliberately.
The broker serves **exactly one client** — shepherdd — where the management
socket has to serve the launcher, the HUD and the one-shots, so the broker
*could* be narrower than the cgroup: with the peer's pid pinned by the pidfd,
`/proc/<pid>/exe` is authoritative and unforgeable after exec, so "in the session
scope **and** running the installed `shepherdd`" is expressible.

Recommended: **do not**, at least not in the first cut. It puts a `/proc` read
back into a decision path `peer.rs` deliberately keeps free of one, and it buys
little now that everything else in that cgroup is shepherd's own code resolved
from root-owned directories (`d096a53`). Worth a comment naming it as the
available tightening if the trusted set ever grows something third-party.

## Components

```
┌────────────────────────── system manager ──────────────────────────┐
│ shepherd-stated.service        User=shepherd-state                 │
│   owns  /var/lib/shepherdd/state/<user>/  0700 shepherd-state      │
│   binds /run/shepherdd/state/<user>.sock                           │
│   admits only peers whose cgroup id == the session scope's         │
└────────────────────────────────────────────────────────────────────┘
┌────────────────── session-<n>.scope (root-owned) ──────────────────┐
│ sway → shepherdd → the one connection above                        │
│ launcher   HUD   swayidle   sidecars   query one-shots             │
└────────────────────────────────────────────────────────────────────┘
┌───────── app.slice / system.slice: every activity ─────────┐
│ no path to /var/lib/shepherdd, and no                      │
│ accepted connection to the state socket                    │
└────────────────────────────────────────────────────────────┘
```

A new crate `shepherd-stated` (the binary) and a new `shepherd-state-proto`
(the wire types + client), or the client folded into `shepherd-store` behind a
feature — see [Where the code goes](#where-the-code-goes).

### The uid

`shepherd-state`, a system user with no login shell and no home, created by
`shepherd install` the way `shepherd-firewall` already is. **Not root.** Nothing
this service does needs privilege: it opens a SQLite file it owns, reads a TOML
file it owns, and asks logind a read-only question. Making it root would add the
one thing this design is otherwise free of — a privileged process parsing
attacker-adjacent input.

### The directory

`/var/lib/shepherdd/state/<user>/`, mode `0700`, owned by `shepherd-state`.
Per-user because a device could in principle run two kiosk users, and because the
trusted cgroup is resolved per user anyway.

`/var/lib` rather than the child's home for the obvious reason: DAC then does the
work for every uid on the box except the one that owns it, and the child's uid is
not that one.

`/var/lib/shepherdd/` specifically, rather than a new `/var/lib/shepherd/`,
because the tree already uses it: `harden.sh` keeps its rollback state in
`/var/lib/shepherdd/hardening/<user>/`. A sibling directory differing by one `d`
would be a permanent source of confusion.

**And because the documentation already says that is where state lives.** Both
`crates/shepherdd/README.md` ("State is persisted to SQLite: `/var/lib/shepherdd/`")
and `docs/INSTALL.md` describe it. They are wrong today — the shipped
`config.example.toml` leaves `data_dir` commented out, so an installed device
actually keeps everything under `~/<user>/.local/share/shepherdd/`, and
`/var/lib/shepherdd/` does not exist at all. That is worth knowing for two
reasons:

* **`INSTALL.md`'s factory-reset recovery is broken on a stock install.** It says
  `sudo touch /var/lib/shepherdd/.factory-reset-ble`, but the sentinel defaults to
  `<data_dir>/.factory-reset-ble`. Following it creates a directory nothing reads
  and reboots into an unchanged device — during the one procedure that exists for
  when no phone can administer it. Verified on the kiosk installed for
  `2026-08-29 005`: no `/var/lib/shepherdd`, database in the home directory.
  Fixable either way (correct the doc, or set `data_dir`), and it is a decision
  the maintainer should make rather than something this design should quietly
  settle.
* This design makes the documented layout true. That is a point in its favour,
  not an accident: `/var/lib` is where this state was always described as living.

### The socket

`/run/shepherdd/state/<user>.sock`, created by the service, `0666`, in a directory
owned by `shepherd-state` and mode `0755`.

The permissive *socket* mode is deliberate and should be commented as such:
**the peer check is the gate, and the file mode is not.** A restrictive mode
would have to be either `shepherd-state`-owned (locking shepherdd out) or
group-shared with the child's uid (which every activity also has, making it
decorative). Pretending otherwise would be the same mistake as the pre-#144 uid
check.

The *directory* mode is doing real work, though, and it is worth saying why —
because `2026-08-29 004`'s finding 2 shows what happens without it. An activity
can `unlink()` shepherdd's own management socket and `bind()` an impostor in its
place, because that socket lives in a directory at the child's own uid, and
neither a root-owned directory (shepherdd could not bind in it) nor the sticky
bit (an activity *is* the owner) can prevent it. That finding's conclusion was
"no file mode can help while the daemon and the activities share a uid".

This socket is the case where they do not. `/run/shepherdd/state/` belongs to
`shepherd-state`, which is not a uid any activity has, so nothing at the child's
uid can unlink or rebind the name. The name-takeover class does not exist here,
and it does not exist for the same reason the file exposure does not: a different
uid owns the directory.

**shepherdd should still verify what it connected to.** `IpcClient::connect` now
identifies its server with `classify_server` (added in `f953a44`) and refuses one
it cannot place in its own cgroup. The state client wants the same discipline
with a different question — the broker is deliberately *not* in shepherdd's
cgroup, so the check is `SO_PEERCRED`'s uid against the `shepherd-state` uid.
Cheap, and it means a misconfigured directory shows up as a refusal rather than
as shepherdd trusting whatever answered.

## The peer rule

The open decision from the investigation, settled.

`peer.rs` already has everything except the source of the trusted id.
`PeerPolicy::restricted()` compares against `own_cgroup_id()`; the broker is not
in the session, so it needs a third constructor:

```rust
/// Accept only peers in a specific cgroup — the graphical session scope of the
/// user this broker serves. Unlike `restricted()`, the trusted cgroup is not
/// our own: we are outside the session by design.
pub fn for_cgroup(id: u64) -> Self
```

`classify()` is unchanged: `peer_cgroup_id(fd)` from `SO_PEERPIDFD` +
`PIDFD_GET_INFO`, one `u64` compare, no `/proc` in the decision path. Root is
still accepted, for the same reason (`sudo shepherd-admin` must work).

### Resolving the trusted cgroup

`cgroupid` is the cgroup directory's inode number — `peer.rs` documents it, and
`2026-08-29 005` confirmed it against a live session (`52173` from both
`PIDFD_GET_INFO` and `stat`). So:

1. Ask logind (`org.freedesktop.login1.Manager.ListSessions`) for the sessions of
   the configured user.
2. Keep the one that is **the graphical session**: `Class=user`, `Type=wayland`,
   and a seat. Refuse to guess if there are zero or more than one.
3. Read its `Scope` (e.g. `session-7.scope`), build
   `/sys/fs/cgroup/user.slice/user-<uid>.slice/<scope>`, `stat()` it, take
   `st_ino`.

Resolved lazily and re-resolved when the session changes (logind signals
`SessionNew`/`SessionRemoved`), because a logout/login gives a new scope. Cheap
either way: shepherdd holds one connection for the life of the session, so this
runs about once per boot.

**Why `Class=user` and not "any session scope".** A second login for the same
user — SSH, a TTY, another `su` — gets its own `session-<n>.scope` and would pass
a loose rule. The `su`-created session used for the measurements in
`2026-08-29 005` was `Class=background`, and *would be refused* by this rule.
That is the intended behaviour, and it means the measurement harness has to
opt out rather than accidentally pass.

**Why not `GetSessionByPID`.** logind can map the peer's pid to a session
directly, and the pid is pinned while we hold the pidfd, so it is not the racy
lookup it looks like. It is still rejected: it makes the decision depend on
logind's bookkeeping and a D-Bus round trip *per connection* rather than on one
integer compare, and it diverges from the mechanism `peer.rs` already justifies
and tests. Worth keeping in the comments as the considered alternative.

**Why not trust-on-first-use.** "Pin whichever session scope connects first" is
racy at boot in exactly the wrong direction — an activity cannot connect before
shepherdd in practice, but the design should not depend on "in practice".

### What this makes load-bearing

`shepherd harden apply --user kiosk` denies the kiosk user SSH and console login,
which is what stops a second session existing for the peer rule to be narrow
about. The residual is therefore "somebody logs the kiosk user in a second way",
not "an activity forges a session" — the measurements are clear that an activity
cannot do the latter.

But hardening is *already* load-bearing, and not because of this design.
`352b6f2` put the fix for `2026-08-29 004`'s finding 1 — stripping
`user_readenv=1` out of `/etc/pam.d`, so PAM stops handing the session an
environment the child wrote — inside `apply_global_hardening`. That is the only
place it can live (the configuration that reads the file is root-owned; the file
itself is in a directory the child owns and can restore), and it means an
unhardened device today still lets an activity choose the next session's
environment.

**`INSTALL.md` still calls hardening optional**, in two places
(`Kiosk hardening (optional)` and "Kiosk hardening is still optional" in the
install walkthrough). That was defensible when hardening was about login
surfaces. It is not defensible now that it carries a #144 fix, and it will be
less so with this design on top. Changing that framing belongs in whichever
branch lands first; this one should not assume the other did it.

## The environment is attacker-chosen, so no protected path may come from it

This is a hard requirement rather than a nicety, and it comes from
`2026-08-29 004`'s finding 1: `pam_env`'s `user_readenv=1` is enabled on every
GDM service on stock 26.04, so an activity that writes `~/.pam_environment`
chooses the **next kiosk session's whole environment** — shepherdd's included.
Measured on hardware there, not inferred.

Everything of that shape that #144 could close has been closed: the helper
binaries behind `env_override`, the three hardening switches, `--sway-ipc-alias`,
and `SHEPHERD_BROWSER_ROOT` — all now behind `--trust-environment` or gone
entirely. The rule those commits settled is the one this design inherits:

> A flag can be stripped from the installed config and verified; an environment
> variable can be neither.

**What is left was explicitly handed to #157** (`d445c98`), so this design owns
it. `crates/shepherdd/src/main.rs` keeps `env = "SHEPHERD_SOCKET"` and
`env = "SHEPHERD_DATA_DIR"`, and — the reason they could not get the #144
treatment — `shepherd-util`'s `paths.rs` reads both *directly*, so deleting the
clap attribute "would have made them look closed while leaving them live". The
same is true of `XDG_DATA_HOME`, `XDG_STATE_HOME` and `HOME`; the installed sway
config launches the daemon as `shepherdd -c ~/.config/shepherd/config.toml`,
where `~` is the shell expanding an attacker-settable `HOME`.

Re-measured on the installed kiosk after the fixes landed, with
`SHEPHERD_DATA_DIR=/tmp/evil-data` in the session environment:

```
shepherdd: Store initialized db_path=/tmp/evil-data/shepherdd.db
/tmp/evil-data:  drwxrwxr-x kiosk kiosk   shepherdd.db  (fresh, 77824 bytes)
```

`2026-08-29 002` records the effect as "usage accounting starts from zero", which
is the floor rather than the ceiling. **`admin.toml` and the reset sentinel
default to `<data_dir>/…` as well**, so the same variable also decides where the
BLE admin record and the minted HTTP token get written — into a directory the
activity named, and can therefore read and pre-create. That is the credential
half of #156 handed over by an environment variable, with no write to any
existing file.

So a design that protected the files but let the environment say *which* files
would protect nothing: an activity would point the next session at a data
directory it owns and the broker would sit there holding the real one, unused
and unread.

Three consequences, all of which this design has to carry:

* **The decision to use the broker, and the broker's socket path, come from
  compiled-in constants and CLI flags only — never the environment.** Same rule
  as `env_override`, applied to paths, and it should reuse that gate rather than
  grow a second one.
* **`SHEPHERD_DATA_DIR` must stop selecting the protected assets.** It can go on
  choosing the unprotected ones (child logs, caches) for development, but once
  the database, `admin.toml` and the sentinel live behind the broker there is no
  path left for it to name. Most of this falls out for free: shepherdd stops
  opening any of them by a `HOME`-derived path and asks the broker, which knows
  where they live because its unit says so.
* **`shepherd_util::paths` has to change, and that is the part that is not
  free.** This is what `ab4d218` identified as "a design change rather than an
  attribute deletion", and it is the reason the residual was handed here rather
  than fixed there: `default_data_dir` and `default_socket_path` are shared by
  the daemon *and every client*, and `run-dev` and the e2e harness both export
  the variables. So the change has to answer what a dev session and
  `shepherd-admin` do instead — which is the same question this design already
  answers for the store, and should be answered once for both rather than twice.

  Concretely: the protected paths stop coming from `paths.rs` at all (the broker
  owns them); `SHEPHERD_DATA_DIR` keeps working for the unprotected remainder
  behind `--trust-environment`, which the e2e harness already passes; and
  `run-dev` gains the flag it is already morally using.

Worth noting for whoever picks this up: `shepherd harden apply` now strips
`user_readenv=1` from `/etc/pam.d`, which closes the vector at its source — but
only for devices that ran it. See [What this makes load-bearing](#what-this-makes-load-bearing).

## What moves, and what deliberately does not

| file | today | after |
| --- | --- | --- |
| `shepherdd.db` | `~/.local/share/shepherdd/` | broker-owned; reached through `Store` |
| `config.toml` | `~/.config/shepherd/` | broker-owned; read + change-notified over the socket |
| `admin.toml` (BLE admin + HTTP token) | `<data_dir>/` | broker-owned; `AdminStore` load/save/clear over the socket |
| `.factory-reset-ble` sentinel | `<data_dir>/` | broker-owned |
| the queued-unbond list | `<data_dir>/` | broker-owned |
| daemon tracing log | `~/.local/state/shepherdd/` | **stays** |
| per-session child logs | `~/.local/state/shepherdd/sessions/` | **stays** |
| media cache, browser profiles | `~/.cache/…` | **stays** |

The three that stay are not oversights:

* **The child logs are activity stdout.** An activity can already write whatever
  it likes there by being the process whose output is captured. Protecting them
  protects nothing.
* **The media cache and browser profiles must be writable by activities** —
  `shepherd-media` and the browser *are* activities. This is why the protected
  set has to be enumerated rather than "everything shepherd owns".
* **The daemon's tracing log is diagnostic, not authoritative.** The audit trail
  that matters is the `audit_log` table, which moves with the database. Piping
  every log line over the socket to protect the rest would be a lot of machinery
  for evidence that is already duplicated. If tamper-evident logs are wanted
  later, the answer is journald, not this socket — worth an issue, not this one.

Note what the first row of the table quietly buys: with `config.toml` behind the
broker, `service.management_api.auth_token` is no longer readable by an activity.
That does not close #156 (the API still ships open by default), but it removes
the "read the token off disk and dial loopback" path the investigation called out.

### The config editor still works

The config editor writes `config.toml` **from the browser** (File System Access
API, or a download) — there is no in-daemon config write path today, and this
design does not add one. Operators get the file onto the device the way they
already do, and **root can still write into `/var/lib/shepherdd`** because root
bypasses DAC. `shepherd install config --user kiosk` and `sudo cp` keep working
unchanged; only the child's uid is locked out.

## The wire protocol

A private, versioned, Rust-only protocol. NDJSON over the socket, same framing
discipline as `shepherd-ipc`, and a `serde` enum per direction:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "op")]
enum StateRequest {
    Hello { proto: u32 },
    // one variant per Store method
    GetUsage { entry_id: EntryId, day: NaiveDate },
    AddUsage { entry_id: EntryId, day: NaiveDate, duration: Duration },
    // …plus the non-Store assets
    ReadConfig,
    LoadAdminRecord,
    SaveAdminRecord { record: AdminRecord },
    // …
}
```

**Not** the `shepherd-management-macros` / `rpc_codegen` machinery. That exists to
keep TypeScript and Kotlin mirrors honest for an API that leaves the box; this
wire has two Rust ends, shipped in the same package, upgraded together. Running it
through the codegen would buy drift protection against a drift that cannot happen
and put an internal protocol into `docs/rpc-schema.json`, where it would read as
public API.

Versioning is a `Hello { proto }` handshake that **refuses** on mismatch rather
than negotiating. A mismatch means a half-finished upgrade — the two binaries
ship together — and the honest response to that is a loud failure and a
diagnostic, not a compatibility shim that has to be maintained forever.

### Types that need serde

Most already have it (`EntryId`, `SessionId`, `AuditEvent`, `AuditEventType`,
`DailyOverride`, `AudioOutputRecord`, `AudioOutput`, `StateSnapshot`). Two do
not and will need derives added: **`LimitSubject`** (which already has a string
form, since it is the `subject_key` column) and **`TokenState`**. Small, but
worth knowing before estimating.

## The client: keeping `Store` as it is

`Store` is a 26-method **synchronous** trait behind `Arc<dyn Store>`, consumed by
`shepherd-core`, `shepherd-management`, `shepherd-http` and `shepherdd`. Nothing
outside the daemon uses it — `shepherd-media` has its own unrelated `ResumeStore`,
so the trait's whole blast radius is inside one process.

`RemoteStore` implements the same trait over the socket. `SqliteStore` stays
exactly as it is and moves into the broker. No consumer changes.

### About blocking

The trait is sync and is already called **inline from async contexts** — there is
no `spawn_blocking` around store access today. So this design does not introduce
blocking-in-async; it changes what is being blocked on.

Rough arithmetic, worth confirming rather than trusting:

* A unix-socket round trip is tens of microseconds. A SQLite read from page cache
  is comparable. A SQLite *write* with the default journal mode is an fsync —
  milliseconds. So writes plausibly get *faster*, and reads get a few times
  slower in relative terms and stay negligible in absolute ones.
* The hot path is `list_entries` → `evaluate_entry` per entry, several store
  reads each. Twenty entries is on the order of eighty calls, so a few
  milliseconds per refresh.
* Contention does not change: `SqliteStore` already serialises everything behind
  a `Mutex<Connection>`, and the broker serves one client.

**If a few milliseconds per refresh turns out to matter**, the fix is a batched
read — the store already has `list_daily_overrides(date)` and
`get_all_usage_for_date(date)`, so one `evaluation_snapshot(day)` call collapses
the whole grid refresh into a single round trip, and `RemoteStore` serves the
per-entry trait methods from it. That is a *fallback*, deliberately not in the
first cut: it adds a cache with an invalidation rule, and it should be paid for
by a measurement rather than a guess.

### The failure mode that is new

A hung broker blocks a tokio worker where a hung SQLite effectively never
happened. So the client needs a read/write timeout on every call, mapping to
`StoreError::Database` — which every consumer already handles, because SQLite
could always fail.

## Config reading and change notification

shepherdd currently installs a `notify` watcher on the config file's **parent
directory** and reloads on `Modify`/`Create`. It cannot watch a directory it
cannot open, so the watch moves into the broker, which pushes a
`ConfigChanged` message on the same connection. `handle_config_reload` then runs
unchanged.

This is a small behavioural upgrade as well as a necessity: the watch is now on a
directory only root and the broker can write, so a reload can no longer be
triggered by the child's uid at all — which is the specific exploit
`2026-08-29 005` demonstrated end to end.

## Startup, ordering, and degradation

`shepherd-stated.service` is a system service, `WantedBy=multi-user.target`, up
before any session. shepherdd connects at startup and holds the connection.

Following #144 and #158 exactly — **an unhardened kiosk beats a child staring at
a dead screen** — a broker that cannot be reached does not stop the session:

| at | condition | behaviour |
| --- | --- | --- |
| startup | socket absent or refuses | fall back to the local `SqliteStore` and the in-home config, raise `StateNotProtected` (Critical) |
| startup | opted out (dev session) | local store, no diagnostic — the operator asked |
| startup, **in the broker** | kernel below the `PIDFD_GET_INFO` floor | **serve nobody but root**, log which precondition failed; do not serve unclassifiable peers |
| mid-session | connection drops | reconnect, with backoff; surface failures as `StoreError`, **do not** fall back |
| mid-session | broker refuses the peer | reconnect refuses too; raise the diagnostic, keep running degraded |

The mid-session row is the one that matters for security. **A fallback that can
be triggered after startup is a downgrade attack**: an activity that could make
the broker unreachable would win back everything this design takes away. So the
fallback is a startup-only decision, made once, recorded in a diagnostic, and
never revisited while the daemon runs. It is worth a test.

The kernel-floor row is new since the first draft of this design and comes from
`dc0c7d5`: `PIDFD_GET_INFO` answers `ENOTTY` below its floor, and there is now a
`PeerError::NoCgroup` for the intermediate case — a kernel new enough for the
ioctl but not for `PIDFD_INFO_CGROUPID`, which "a distribution upgrade can
genuinely land on". `shepherd_ipc::kernel_supports_peer_cgroup()` is the exported
predicate.

The two ends fail in **opposite directions**, deliberately, and this is the same
asymmetry `f953a44` chose for client-vs-server identification:

* **The broker refuses**, because a peer it cannot classify might be an activity,
  and serving it would hand over exactly what this exists to protect. A device
  whose kernel regressed below the floor should stop protecting nothing and start
  saying so.
* **shepherdd falls back and carries on**, because a kiosk that will not start is
  worse than one that says it is unprotected — and because nothing an activity
  does can put the kernel below the floor.

Note the interaction: on such a kernel both ends reach the same conclusion by
different routes and the device ends up on the local store with the diagnostic
raised, which is the intended outcome. `PeerPolicy::for_cgroup` should therefore
fail at construction the way `restricted()` already does, rather than returning a
policy that silently matches nothing.

A new `DiagnosticCode::StateNotProtected` joins `IpcSocketNotHardened`,
`CompositorNotHardened` and `IpcSocketReplaced`, with the same phrasing
discipline: say which precondition failed, so a device that ships degraded says
why. That is now four codes whose whole job is "a protection this device is
configured to have is not in force" — enough that the web UI and companion
should probably group them, which is worth a glance while adding the fourth.

## Opting out — its own flag, following the now-settled pattern

An earlier draft of this design argued for folding the broker's opt-out into
`--no-restrict-ipc-peers` and renaming that flag, on the grounds that the tree
had one "device or developer" concept wearing the name of the first thing it
controlled. **`662ee5b` settled this the other way, and its reasoning is
better**, so the recommendation here is reversed:

> Both are development opt-outs, but they are different risks wanted at different
> times — one decides who may *drive* the daemon, the other decides which code
> the daemon *runs*.

Coupling had a concrete cost the consolidation argument missed: while one flag
governed both, every dev and e2e run took binaries from `$PATH`, so the
trusted-directory resolution a device uses was never exercised outside unit
tests. Splitting them (`--trust-environment`, now passed only by the e2e suite)
made an ordinary dev session resolve helpers exactly as a device does.

The install-time guarantee also turns out not to argue for fewer flags:
`shepherd install sway-config` strips and verifies **all three** independently,
dying with a message naming the flag and its consequence. That check scales.

So the broker gets **its own flag**, and the test it has to pass is the one that
sentence implies: it decides a third, separable risk — whether the daemon's
*state* is protected, as distinct from who may drive it or what code it runs. A
developer wants that off for the same reason and at different times.

Following the three that exist: carried by the in-repo `sway.conf`, stripped and
verified by `shepherd install sway-config` (add the fourth arm to the existing
`sed` and the existing `die` check), and passed by the e2e harness, which already
starts its own shepherdd with an explicit `-d <temp dir>` and has no system
service to talk to.

**And it must not be readable from the environment** — see below. Disarmed, the
daemon is byte-for-byte what it is today.

## Packaging and install

`shepherd install` grows a `state` target, and `install all` calls it:

* `useradd --system shepherd-state` (mirroring the `shepherd-firewall` group).
* `/var/lib/shepherdd/state/<user>/` at `0700 shepherd-state:shepherd-state`.
* `/usr/local/bin/shepherd-stated` (or `/usr/libexec/`, like the firewall helper —
  it is not a command an operator runs).
* `dist/systemd/shepherd-stated.service`, the first *unit* the project ships;
  `dist/systemd/` currently holds only the bluetoothd drop-in.
* **Migration**, and it must be root's job: `shepherd-state` cannot read
  `/home/kiosk` (mode `0750`). `shepherd install state --user kiosk` moves an
  existing `shepherdd.db`, `admin.toml`, sentinel and `config.toml` into the
  protected directory and chowns them. Idempotent, and it must refuse to
  overwrite a protected copy that already exists.
* `shepherd uninstall` moves them back, or leaves them and says where they are.
  Silently orphaning a device's usage history would be worse than either.
* `shepherd-admin` grows `state backup` / `state restore` for the operator, since
  `~/.local/share` is no longer where a support answer can point.

## What an attacker gains from the broker existing

New surface, honestly enumerated:

* **The socket is reachable by every activity** (mode `0666`). They are refused at
  accept, before a byte is parsed — the same ordering #158 chose, for the same
  reason. Refusal closes the connection without answering.
* **Deserialization happens only after acceptance**, so an activity never reaches
  `serde`. This is the main argument for deciding at accept rather than per call.
* **Denial of service is possible**: an activity can open connections in a loop.
  This is `2026-08-29 004`'s finding 3 arriving at a second socket, and it has
  already been solved once: `RejectionReporter` in `shepherd-ipc/src/server.rs`
  reports the first refusal in full — a single probe is never silent — and then
  at most one a minute carrying the suppressed count, gating the `warn!` and the
  diagnostic broadcast on the same decision. **Reuse it rather than writing a
  second one**; the interesting part of that fix was the reasoning about why the
  cgroup detail has to stay in the message, and that reasoning transfers intact.
  The broker should additionally accept-and-close cheaply and cap concurrent
  connections. It must **not** respond by falling back — see above.
* **The service runs unprivileged**, so a hypothetical parser bug yields
  `shepherd-state`, which owns exactly the files this is protecting and nothing
  else. That is a real loss but a bounded one, and it is why this is not root.
* **The broker spawns nothing.** It opens a SQLite file, reads TOML, and asks
  logind a read-only question over D-Bus — no `Command::new` anywhere, so the
  whole `$PATH`-substitution class that `2026-08-29 004`'s finding 1 is about
  does not reach it. That is worth stating because it is a property to *keep*:
  `clippy.toml` now denies bare `Command::new` workspace-wide, so a future
  contributor who reaches for one gets stopped rather than having to know why
  they should not.
* **`IpcPeerRejected` has an analogue here**: an activity probing the state socket
  is worth an administrator's attention for the same reason it is on the
  management socket.

## What this does not close

* **The HTTP management surface (#156)**, which ships `enabled = true`,
  `bind = "0.0.0.0"` and no token. Protecting the file the token lives in does
  not matter while the API accepts requests without one. #157 and #156 have to
  land for either to be worth much; this design makes the *token* unreadable, not
  the *API* closed.
* **Classic-confinement snaps and anything else at the child's uid** can still
  read and write everything else in that home directory. This closes shepherd's
  own state, not the home directory, and #105 remains #105.
* **A second login session for the kiosk user**, if hardening is not applied.
* **The daemon's tracing log**, as above.

## Where the code goes

| crate | change |
| --- | --- |
| `shepherd-ipc` | `PeerPolicy::for_cgroup(u64)`; logind resolution helper. Small. |
| `shepherd-store` | unchanged trait; `SqliteStore` unchanged; new `RemoteStore` (client) |
| `shepherd-state-proto` *(new)* | request/response enums, framing, `Hello` |
| `shepherd-stated` *(new)* | the binary: socket, peer policy, `SqliteStore`, config watch, admin record |
| `shepherd-util` | `Serialize`/`Deserialize` for `LimitSubject`, `TokenState`; **stop `paths.rs` reading `SHEPHERD_DATA_DIR`/`SHEPHERD_SOCKET` ungated** |
| `shepherd-api` | `DiagnosticCode::StateNotProtected` |
| `shepherdd` | connect at startup, choose store, move the config watch, the flag, the diagnostic; verify the broker's uid |
| `scripts/lib/install.sh` | the `state` target, the system user, the unit, migration |
| `dist/systemd/` | `shepherd-stated.service` |

`shepherd-core`, `shepherd-management`, `shepherd-http`: **no changes.** That is
the point of the trait already being there.

## Build order

Each step is landable and leaves the tree working.

1. **The protected directory and the migration**, with shepherdd still opening
   the files directly from their new home via root-installed symlinks or an
   explicit path. No broker yet; proves the packaging, migration and uninstall
   paths in isolation. *(Optional — fold into 3 if it feels like ceremony.)*
2. **`PeerPolicy::for_cgroup` + logind resolution**, with unit tests, and a
   `shepherd-admin` subcommand that prints the resolved trusted cgroup for a user.
   Independently reviewable, and the piece the whole design rests on. Its tests
   belong behind `skip_without_peer_cgroup`, so they skip below the kernel floor
   and **fail** where `SHEPHERD_REQUIRE_PEER_CGROUP` is set — which CI now sets on
   the `test` and `e2e` jobs (`70c9967`). Reuse that; do not invent a second
   skip mechanism, and do not let this land as tests that quietly never run.
2b. **Ungate the paths**: stop `shepherd-util`'s `paths.rs` taking
   `SHEPHERD_DATA_DIR` and `SHEPHERD_SOCKET` from an environment the child can
   write, routing them through the same development gate `env_override` uses for
   helper binaries. Small, independent of everything else here, and it is a
   #144-shaped fix rather than a #157 one — it may well belong on that branch
   instead. Either way the broker is not a boundary until it lands.
3. **`shepherd-stated` serving the database only**, `RemoteStore`, the flag, the
   diagnostic, the startup-only fallback. The bulk of the work and the bulk of
   the value: usage, tokens, cooldowns, overrides, audit.
4. **`config.toml`**: read + change notification, replacing the `notify` watcher.
5. **`admin.toml`, the sentinel, the unbond queue.**
6. **`INSTALL.md`**: hardening moves from "optional" to a documented precondition,
   and the new service gets its section next to the two socket hardenings.

## What building it changed

Recorded here rather than silently, because three of these were design errors
the design could not have caught and a device did.

* **The socket mode.** `0666` was specified and the hand-bind path left it at
  umask `0755`; connecting to a Unix socket needs *write*, so a peer inside the
  trusted session was refused by DAC before the peer check ever ran. `0755`
  looks safer and is simply broken.
* **The session filter needed a fourth term.** `Class=user` + `Type=wayland` +
  a seat was not enough: after `systemctl restart gdm` the outgoing session is
  still listed, still seated, and still owns a live cgroup, with logind
  reporting `closing`. Two sessions matched, the daemon refused to guess — the
  right behaviour — and the custodian would not start. Excluding `closing`
  fixes it; `online` stays accepted, because a kiosk with its VT switched away
  is still the session.
* **Migration had to move before the unit's `StateDirectory=` existed.** That
  directory is created when the *service* first starts, and socket activation
  means that is the moment shepherdd first connects — by which time the daemon
  has created an empty database and the device's history is stranded in the
  home directory it came from. The installer now creates the directory itself,
  migrates, and only then enables the socket.
* **`LimitSubject` already had serde**, by hand-written impl rather than derive,
  so only `TokenState` needed adding. The design said both.
* **The opt-out is `--no-state-custodian`**, its own flag, per `662ee5b`'s
  reasoning rather than this note's original argument for folding it in.

One thing the design did not anticipate at all: a device that falls back leaves
a **stale, unprotected database** in the home directory, still readable and
writable by every activity. Nothing reads it, but nothing says so either, so
shepherdd now warns when it is using the custodian and that file still exists —
and the warning says what is actually true, which is that `install state` will
*not* migrate over a live protected copy.

## To verify on hardware before step 3 is called done

**Done.** The rule was exercised against a GDM-started kiosk (autologin, clean
boot) rather than the `su`-started session `2026-08-29 005` measured on, and
every line below was observed:

* the broker resolving the session scope and naming it in its log,
* shepherdd connected, no `StateNotProtected`,
* an activity refused at the state socket, with the refusal naming its scope,
* `/home/<user>/.local/share/shepherdd/` gone, and nothing under it recreated.

The last one is the regression that would matter most and is the easiest to miss:
a path default that quietly recreates the old file would leave a device protected
and a device upgraded-in-place unprotected, with no visible difference.
