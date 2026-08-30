# Looking for holes in the #144 peer check

A deliberate hunt for ways around the cgroup allow-list, after the #158 review in
`2026-08-29 003`. Findings are ordered by how much they undermine #144.

Finding 1 was confirmed on real hardware: shepherd was installed from source the
normal way (`shepherd install all --user shepherd-kiosk`) onto a 26.04 host
running GDM, and the environment a kiosk session actually receives was measured
against GDM's own PAM stack. The other two were tested on the same host but not
against a live kiosk login.

## 1. An activity chooses shepherdd's entire environment

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

## 2. An activity can take over the management socket's name

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
so a crashed daemon's stale socket does not make a restart impossible. Putting
the socket in a root-owned directory (`/run/shepherd/`, created by the
installer) closes it and keeps that property.

## 3. A rejected peer can amplify one `connect()` into a broadcast

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
diagnostic. Dropping the pid/cgroup from the diagnostic message (keeping them in
the log, where they belong) would make repeated rejections dedupe to one
broadcast.

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

1 is the one to fix, and it should block the branch: it is reachable by any
activity, it needs one file write, and it converts the peer check from a
boundary into a formality. 2 needs an installer change and a decision about
where the socket lives. 3 is a papercut, worth folding into whatever touches the
diagnostic next.
