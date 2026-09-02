# Looking for holes in the #144 peer check

A deliberate hunt for ways around the cgroup allow-list, after the #158 review in
`2026-08-29 003`. Findings are ordered by how much they undermine #144.

Finding 1 was confirmed on real hardware: shepherd was installed from source the
normal way (`shepherd install all --user shepherd-kiosk`) onto a 26.04 host
running GDM, and the environment a kiosk session actually receives was measured
against GDM's own PAM stack. The other two were tested on the same host but not
against a live kiosk login.

## 1. An activity chooses shepherdd's entire environment — FIXED

`shepherdd` execs `systemd-run`, `pkexec`, `snap`, `flatpak`, `yt-dlp`, `pgrep`,
`systemctl`, `wpctl`, `pactl`, `amixer`, `pw-dump`, `wl-mirror`, `script`,
`setsid` and `pkcheck` **by bare name**, resolved through `PATH`.
`firewall_helper_path()` is the only one resolved absolutely, and even that is
reached via a bare-name `pkexec`. `shepherdd` never pins or sanitises `PATH`.

That would be a latent problem if the session's `PATH` were merely untidy. It is
worse than that: on a stock 26.04 + GDM host the kiosk user **dictates** it.

### The mechanism

`/etc/pam.d/gdm-password` and `/etc/pam.d/gdm-autologin` both carry:

```
session required        pam_env.so readenv=1 user_readenv=1 envfile=/etc/default/locale
```

`user_readenv=1` makes `pam_env` read **`~/.pam_environment`** — a file in the
user's own home. The option was deprecated upstream long ago, but
`libpam-modules 1.7.0-5ubuntu3.1` still honours it.

Measured directly against GDM's own PAM service and the real kiosk user, with a
`pam_getenvlist` harness after `pam_open_session`, having first planted a
`~/.pam_environment` owned by `shepherd-kiosk`:

```
$ sudo ./pamenv gdm-password shepherd-kiosk
open_session: Success
--- PAM environment handed to the session ---
PATH=/home/shepherd-kiosk/evil:/usr/bin:/bin      <- attacker-chosen
LANG=en_US.UTF-8
SHEPHERD_PAMENV_MARKER=reached
XDG_SESSION_ID=100
HOME=/home/shepherd-kiosk
XDG_RUNTIME_DIR=/run/user/1001
```

Every activity runs as this uid. So any activity can write one file in its own
home and choose the environment of the next kiosk session — `shepherdd`
included.

### Why it defeats #144 specifically

1. An activity writes `~/.pam_environment` setting `PATH` to a directory it
   owns, and drops a `systemd-run` there.
2. At the next login `shepherdd` execs `systemd-run` by name and gets that
   binary — as a **direct child of shepherdd, in shepherdd's own cgroup**, which
   the management socket accepts as `Admin`.
3. The same substitution makes `user_scope_argv_prefix` a no-op, so every
   activity now shares shepherdd's cgroup too. The check does not fail loudly;
   it stops separating anything.

A kiosk device reboots and logs in on its own, so "at the next login" is not
much of a barrier.

The same file sets *any* variable, including shepherd's own binary-selection
overrides: `SHEPHERD_FIREWALL_HELPER`, `SHEPHERD_TOUCH_BRIDGE_BIN`,
`SHEPHERD_TABLET_BRIDGE_BIN`, `SHEPHERD_GAMEPAD_BRIDGE_BIN`, `SHEPHERD_SOCKET`,
`SHEPHERD_DATA_DIR`.

### What stops it going further

`SHEPHERD_FIREWALL_HELPER` does **not** yield root.
`org.shepherd.firewall.policy` pins
`org.freedesktop.policykit.exec.path=/usr/libexec/shepherd-firewall-helper`, so
`pkexec` on any other path falls back to a generic `auth_admin` action and
fails. That containment is deliberate and it holds. A substituted `pkexec` still
runs as the kiosk user — it cannot escalate, but it can fake the firewall having
been applied.

### What does *not* cause it

Worth recording so the fix targets the right thing:

- `gdm-session-worker`'s compiled-in default `PATH` is
  `/usr/local/bin:/usr/bin:/bin:/usr/games` — no home directories.
- `/etc/environment` is clean, and the `environment.d` generator output is clean.
- GDM does **not** wrap Wayland sessions in `/etc/gdm3/Xsession`, so the
  `~/.profile` sourcing that script does is not on the kiosk path. (A live
  GDM-started *GNOME* session on the same host does show `~/.profile`-derived
  entries — `~/.cargo/bin`, `~/.local/bin` — so `.profile` reaches at least some
  GDM sessions. Whether it reaches a plain `Exec=sway` entry was not
  established, and does not matter while `user_readenv` is enabled.)

Absent `~/.pam_environment` the kiosk `PATH` is fine. The hole is that one
user-writable input, and it is enabled by default.

### Fix

Sanitising the inherited `PATH` is **not sufficient**, because the environment
is attacker-chosen wholesale rather than merely untidy. The daemon has to stop
reading the environment for this at all:

- Resolve helpers to absolute paths, or exec them with a `PATH` constant
  compiled into `shepherdd` rather than one inherited.
- Gate `SHEPHERD_*_BIN` and `SHEPHERD_FIREWALL_HELPER` behind a dev build or an
  explicit flag. As shipped they are a direct binary-substitution primitive.
- Consider having `shepherd harden apply` delete `~/.pam_environment` and
  recreate it root-owned, which closes the vector at its source for anyone who
  runs it — and is the only part of this that also protects the *rest* of the
  session, not just shepherd's own execs.

Roughly 26 call sites move from `Command::new("x")` to a resolved path.
Resolution should be lazy-then-cached, not eager: `pactl` and `wl-mirror` are
optional fallbacks and eager resolution would raise diagnostics for tools that
are legitimately absent.

### Fixed

`crates/shepherd-host-linux/src/helpers.rs`. `resolve` searches a compiled-in
list of root-owned directories and never reads the environment; a name found
nowhere resolves to `/usr/bin/<name>` rather than falling back to a `$PATH`
lookup. `resolve_daemon_sibling` covers shepherd's own binaries — the input
sidecars, and the pairing overlay, which had never had even the `current_exe()`
treatment the sidecars did. `SHEPHERD_*_BIN` and `SHEPHERD_FIREWALL_HELPER` go
through one gate, `env_override`, off unless `--no-restrict-ipc-peers` says this
is a development session. `yt-dlp` is resolved through an injected resolver, for
the same reason the scope wrapper is injected.

Regression tests are in `crates/shepherd-host-linux/tests/helper_resolution.rs`
— an integration test rather than a unit test, because poisoning `PATH` in the
crate's shared test process breaks the several unit tests that spawn `sleep` and
`sh` by name. `tests/helper_env_override.rs` is a second file for the same
reason at one remove: the trust flag is process-global and `resolve` consults
it, so two tests that disagree about it would decide each other's outcome by
running order.

**It broke stubbing, which CI caught.** The e2e browser test installs a fake
`flatpak` on `$PATH` to exercise the policy-injection path without installing
Chrome — resolving only from trusted directories found `/usr/bin/flatpak`, which
does not exist in the CI container, and the launch failed. So `resolve` searches
`$PATH` **first when the environment is trusted**, which is the pre-#144
behaviour and is off on a device by the same flag that arms the peer check.
Stubbing a helper is a legitimate thing for a test to do; what must not happen
is a *device* taking binaries from a `$PATH` the kiosk user writes.

Verified in the headless dev stack, which exercises the real spawn paths:

```
Activities will be launched into a cgroup of their own      <- resolved systemd-run ran
Environment overrides for helper binaries are enabled...    <- dev gate fires in dev only
Steam preloaded in background pid=288898                    <- resolved + wrapped snap path
yt-dlp ... HTTP Error 400 (placeholder playlist id)         <- resolved yt-dlp really ran
```

No helper failed to resolve.

### CI could not exercise the peer check, until the runner is upgraded

The Rust jobs run in a container, so they get the **host's kernel** however new
the image is — and the runner's was older than `PIDFD_GET_INFO`, which answers
`ENOTTY`. Four `peer::tests` cases (and the impostor case in
`server_identity.rs`) failed there while passing on any 26.04 host.

They skip via `skip_without_peer_cgroup()`, which counts `NoCgroup` as
unsupported too: that is what a kernel new enough for `PIDFD_GET_INFO` but not
for `PIDFD_INFO_CGROUPID` answers, an intermediate an upgrade can land on, and
treating it as support would run the tests and fail them.

**A skip is invisible.** `cargo test` captures a passing test's output, so the
`[SKIP]` line never reaches the log and a green run says nothing about whether
the check was exercised. So the guard escalates: set
`SHEPHERD_REQUIRE_PEER_CGROUP=1` and a skip becomes a panic naming the reason.
The runner is self-hosted, so the floor is something the host can be upgraded
past — and `ci.yml` now sets that variable on the `test` and `e2e` jobs, which
makes the coverage mandatory and stops it disappearing again unnoticed. If those
jobs start failing with *"this kernel cannot report a peer's cgroup"*, the
runner is below the floor rather than the code being wrong.

### ...and then it ran as root, which is not the same as running

With the kernel floor met, `an_armed_policy_refuses_a_peer_from_another_cgroup`
failed: `a peer outside shepherd's cgroup must be refused: Admin`. The CI image
has no `USER` directive, so the tests run as **root** — and `classify` accepts
root from any cgroup by design, checking that *before* it looks at the cgroup.
The test passed `Some(getuid())`, which as root is `Some(0)`, so it never
reached the comparison it exists to make.

The code was right; the test assumed a non-root user. Worse, two of its
neighbours were passing for the same wrong reason —
`an_armed_policy_accepts_a_peer_in_our_own_cgroup` and
`a_disarmed_policy_classifies_as_before` both got their expected answer from the
root rule without comparing anything. So a suite that looked green under root was
exercising less than the same suite under uid 1000.

All three now claim a fixed non-root `PEER_UID`, which is what the accept loop
would have read from `SO_PEERCRED`; nothing about them depends on the process's
real uid any more. Verified by running the test binary both ways — reproduced
the failure under `sudo` first, then confirmed the fix under `sudo` and as the
normal user, and swept the whole workspace as root (1015 passed) for other uid
assumptions. There were none.

Two things stay skipped in a container even on a new kernel, and neither is
about the kernel: `a_client_refuses_an_impostor_in_another_cgroup` needs a
systemd **user manager** and a session bus to put the impostor in a scope of its
own, and `activity_isolation_status()` needs the same to isolate an activity at
all.

### Nothing stopped the next bare `Command::new`

The fix above changed 26 call sites and wrote the rule into two READMEs, and
that was all: no lint, no test, no CI check. `clippy.toml` already existed and
already used `disallowed-methods` — for exactly one thing, `chrono::Local::now`
— so the mechanism was there and unused for this.

Both `Command::new` paths are now denied workspace-wide, pointing at
`helpers::command()`. Legitimate spawns take an
`#[allow(clippy::disallowed_methods)]` with a comment naming the exception,
which makes each one greppable and argued rather than invisible.

Arming it found a live gap immediately: `brightness.rs` exec'd `brightnessctl`
through `$PATH`. The original sweep matched `Command::new("<literal>")` and this
one names the binary in a `const`, so it had been missed — the precise failure
mode a grep-based guard would also have missed, found on the first run of a
type-aware one.

Verified by reintroducing the hole on purpose (`helpers::command("pkcheck")` ->
`std::process::Command::new("pkcheck")`) and confirming CI-equivalent clippy
rejects it with the reason attached, then restoring it.

## 2. An activity can take over the management socket's name — FIXED

Demonstrated end to end with a throwaway socket:

```
real daemon bound, mode 0o660
UNLINK: succeeded            <- same uid, different cgroup
IMPOSTER: bound at the same path
CLIENT got: {"impostor":true}
real daemon still listening: True
```

The socket is `$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock`, mode `0660`, in a
directory `shepherdd` creates inside a `0700` runtime dir owned by the kiosk
uid. Any process at that uid can `unlink()` it and `bind()` its own listener
there. Existing connections survive; every *new* one — a relaunched launcher or
HUD, a sway keybinding one-shot, `swayidle`'s `--screen-off` — reaches the
impostor. Nothing in `client.rs` checks who it connected to.

This is the mirror of the problem #144 solved. The peer check protects
`shepherdd` from activities; nothing protects *clients* from a fake `shepherdd`.
Not privilege escalation — the impostor cannot make the real daemon act — but it
can lie to the launcher and the HUD, and silently swallow `--screen-off`, which
is a policy effect a child would notice and a parent would not.

Any fix must keep `server.rs:105`'s unconditional `remove_file`, which is there
so a crashed daemon's stale socket does not make a restart impossible.

### The obvious fix does not work

This note originally recommended moving the socket to a root-owned
`/run/shepherd/`. That is wrong, and the reason generalises: **no file mode can
help while the daemon and the activities share a uid.** Measured:

```
root-owned dir, 0755:  bind: BLOCKED -> [Errno 13] Permission denied
sticky bit, 1777:      activity unlink: SUCCEEDED
```

A directory `shepherdd` cannot write is a directory it cannot bind in. The
sticky bit restricts deletion to the file's *owner*, and an activity is the
owner — same uid. Preventing the name being taken requires the socket to be
created by something other than the daemon, i.e. systemd socket activation with
root binding it and passing the fd, which `shepherdd` cannot use while sway
`exec`s it.

### Fixed, by asking the question backwards

The name can still be taken. What an impostor cannot do is be believed.

`IpcClient::connect` now identifies the listener with the same
`SO_PEERPIDFD` + `PIDFD_GET_INFO` machinery the daemon uses on its peers
(`classify_server`, `ServerCheck`): shepherd's own clients live in the daemon's
cgroup, so "is the server in my cgroup?" is exactly the question, and an
activity's listener is in a scope of its own by construction. Root is exempt, as
it is on the server side. `connect_unverified` exists for clients that
legitimately live outside the session.

The two failure modes deliberately fail in opposite directions: a *server* that
cannot be identified is refused, because an impostor can cause that by exiting
once the connection is accepted; a client that cannot read its *own* cgroup
warns and continues, because nothing an activity does causes that and refusing
would leave a device with a launcher that will not start.

`IpcServer::socket_was_replaced` compares `(st_dev, st_ino)` against what was
bound; `shepherdd` polls it once a minute and raises the new `Critical`
diagnostic `ipc_socket_replaced`. That is the residual: the name can be taken,
so the session can be made unreachable. Nothing is given away, but it should not
look like a launcher that stopped working for no reason.

Tests: `crates/shepherd-ipc/tests/server_identity.rs`, including a real impostor
put in a cgroup of its own with `systemd-run --user --scope` (skipped where no
user manager is available). Verified in the headless dev stack: eight client
connections, no refusals, full launcher grid.

## 3. A rejected peer can amplify one `connect()` into a broadcast — FIXED

Per refused connection (`server.rs:169-186`): two `SO_PEERPIDFD` +
`PIDFD_GET_INFO` round trips (once in `classify`, once in `reject` for the log),
a `/proc/<pid>/cgroup` read, a `warn!` line, and a
`ServerMessage::ClientRejected` that becomes `diagnostics.raise(...)`.

The registry keys on `(code, subject)` so it does not grow — but
`ipc_peer_rejected_diagnostic` embeds the peer's pid and cgroup in the message,
so `*existing == updated` is false on *every* attempt, `raise` returns `true`,
and `changed.send(())` fires. That wakes every diagnostics subscriber: the web
UI, the BLE companion, the launcher.

There is no accept-rate limiting. The existing `rate_limiter` is keyed on
`client_id` and applied after accept; a rejected peer never gets one, and the
accept loop `continue`s immediately.

Low severity — noise and CPU, not a bypass — but it is noise an activity
controls, aimed at the channel an administrator watches for exactly this
diagnostic.

### Fixed, but not the way this note first suggested

The first suggestion here was to drop the pid and cgroup from the diagnostic
message so repeats dedupe. That turns out to be wrong: naming the cgroup is a
deliberate, tested decision (`a_refused_peer_names_where_it_came_from`), and it
is what turns "something probed the socket" into "this activity did". An
activity's scope carries its session id, so the detail is the whole value of the
report.

So the other option was taken instead: rate-limit the reporting.
`RejectionReporter` in `server.rs` reports the first refusal in full — a single
probe is never silent — and then at most one a minute, carrying the count of
what it suppressed. The `warn!` line and the `ClientRejected` message are gated
together, so both the journal and the diagnostics broadcast are bounded by the
same decision.

Not changed: `classify` still does its two pidfd round trips and a `/proc` read
per refusal, even when the result will not be reported. That is a few
microseconds against an accept-and-close that costs more, and separating it
would mean restructuring `PeerPolicy::classify` to gather the human-readable
detail lazily. Worth doing only if a refusal flood ever shows up in a profile.

Test: `the_first_refusal_is_reported_and_a_flood_is_counted`.

## What was checked and found sound

Recorded so the next reader does not re-derive it:

- **The session scope is not delegated.** The whole boundary rests on an activity
  being unable to write itself into `shepherdd`'s cgroup. Verified:
  `session-2.scope` is `root:root`, its `cgroup.procs` is `-rw-r--r-- root root`,
  and `systemctl show … -p Delegate` reports `Delegate=no`. Contrast
  `user@1000.service`, which is user-owned with an ACL.
- **The root bypass is not forgeable.** `get_peer_uid` uses `SO_PEERCRED`
  (`nix … sockopt::PeerCredentials`), set by the kernel at connect.
- **`systemd-run` fails closed.** Forced a `StartTransientUnit` failure by
  colliding on a unit name: `Failed to start transient scope unit`, `exit=1`, and
  the command did **not** run. A scoping failure means "the activity does not
  launch", never "the activity launches in shepherdd's cgroup".
- **No pid-reuse race.** The decision goes through a pidfd, never a `/proc`
  lookup by pid; the `/proc` read in `reject()` is log-only and post-decision.
- **The polkit path pin holds** — see finding 1.
- **`shepherd install sway-config` strips the dev flags.** The installed
  `/etc/sway/shepherd.conf` carries neither `--no-harden-sway-ipc` nor
  `--no-restrict-ipc-peers`.

## Found while installing (not security)

- **INSTALL.md step 0 is not a valid command.** `sudo ./scripts/shepherd deps
  build run` returns `[ERROR] Unknown deps command: build`. It should be
  `deps install build` followed by `deps install run`.
- **The desktop entry may be installed where GDM does not look.** `install all`
  with the default prefix writes
  `/usr/local/share/wayland-sessions/shepherd.desktop`, but
  `gdm-session-worker`'s built-in search paths are `/usr/share/wayland-sessions/`
  and `/usr/share/gdm/greeter/wayland-sessions/`. Whether GDM picks up the
  `/usr/local` copy depends on its `XDG_DATA_DIRS`, which could not be sampled
  (no greeter running). Worth confirming before a real deployment — it may need
  `--prefix /usr`.

## Priority

All three are fixed.

The residual on 2 is denial: an activity can still make the session unreachable
by taking the socket's name, and only systemd socket activation would prevent
that. It is reported rather than prevented, which is the same trade the
compositor hardening makes.

## The source of finding 1, closed separately

`shepherdd` no longer trusts its environment, but `~/.pam_environment` still set
the environment of everything *else* in the session. `shepherd harden apply` now
strips `user_readenv=1` from every `/etc/pam.d` service that enables it — seven
GDM services on a stock 26.04 — and `harden revert` puts them back.

Deleting the file instead would not work, and the reason is the same one that
defeats a root-owned socket directory: the user owns their home directory, so
they can remove a root-owned file there and put their own back. The
configuration that *reads* it is what has to go, and that lives in root-owned
`/etc/pam.d`.

Verified end to end on the installed kiosk user, with the attack file left in
place:

```
$ sudo ./pamenv gdm-password shepherd-kiosk       # before
PATH=/home/shepherd-kiosk/evil:/usr/bin:/bin
SHEPHERD_PAMENV_MARKER=reached

$ sudo shepherd harden apply --user shepherd-kiosk
$ sudo ./pamenv gdm-password shepherd-kiosk       # after
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin
```

`harden revert` restored all eight files, confirmed with `dpkg -V gdm3`.

One trap worth knowing: `grep -r` does not follow symlinks and `/etc/pam.d` is
full of them (`gdm-smartcard` -> `/etc/alternatives/...`). The edit loop is right
to skip them — `sed -i` through a symlink replaces the link with a regular file
— but the *verification* globs instead, so a symlink whose target is still
enabled is caught rather than passed over by the same blind spot.

### It did not compose across users, and it aborted on the second run

Two bugs in the first version of the above, both found by auditing what
hardening does to *other* users of the same machine.

**`set -o pipefail` vs. the success case.** The verification was
`still_enabled="$(grep -lE ... | tr '\n' ' ')"`. `grep -l` exits 1 when it finds
nothing — which is the outcome being checked for — and `common.sh` sets
`set -euo pipefail`, so the assignment aborted the whole run. `harden apply`
therefore worked exactly once and failed on every later invocation, after
stripping PAM but before writing the `hardened` marker: a half-hardened user
that `harden revert` then refused to touch. Fixed with `|| true`, and the
symptom is why the comment there now says so.

**Per-user backups of system-wide files.** `/etc/pam.d` has no per-user form, so
disabling `user_readenv` disables it for everyone — but it was backed up under
the hardened user's state directory. Harden A, harden B, revert A, and A's
backup (taken before anything changed) put the file back, silently un-hardening
B. `/etc/security/access.conf` had the same shape and had had it since before
this branch: appended per user, restored wholesale.

Now: system-wide changes live in `$HARDENING_STATE_DIR/.global`, applied by the
first hardened user and restored only when the last one is reverted; shared
files that take a per-user rule get a `# BEGIN/END shepherd hardening for user:`
block that revert removes surgically. Measured across two users:

```
stock:            pam_enabled=8  access_rules=0
after harden A:   pam_enabled=0  access_rules=1
after harden B:   pam_enabled=0  access_rules=2
after revert A:   pam_enabled=0  access_rules=1   <- B still hardened, B's rule kept
after revert B:   pam_enabled=8  access_rules=0   <- restored only at the last one
```

`dpkg -V gdm3` reports no drift afterwards.
