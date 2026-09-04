# Hardening shepherdd's own IPC socket — scoping (issue #144)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/144>
> Follow-up: `2026-08-29 002 shepherdd-socket-peer-credentials-verified.md`
> settles the three open questions below on real hardware, and corrects the
> deny-list phrasing this note's "Where the check goes" section slips into.
> Related: #147 (the sway IPC migration, which closed the *same* hole on a
> *different* socket), #151/#152 (the per-entry firewall cgroups this design
> wants to reuse), #143 (the diagnostics channel a rejected peer would report
> through).

Written while reviewing the #147/#144 branch, after the sway half shipped. This
is the design discussion for the half that is still open; nothing here is
implemented. Its purpose is to stop the next person re-deriving it, and in
particular to stop them reaching for the two mechanisms that look obvious and
do not work.

## Verdict

The unlink that hardened sway's socket **does not transfer**, and the bearer
token that suggests itself instead is defeated by the same shared uid that
defeats everything else. What works is peer credentials — but the credential is
only half of it. The kernel hands you an unforgeable *pid*; every hard question
is about what shepherdd compares it against, and today shepherdd cannot answer
that question for every activity. Closing that gap is the actual work.

Short version:

* **Not the unlink.** shepherdd's socket has clients that connect by path
  forever, by design. See [Why the unlink does not transfer](#why-the-unlink-does-not-transfer).
* **Not a token.** Any secret readable by a trusted client is readable by an
  activity at the same uid. See [Why a bearer token does not work](#why-a-bearer-token-does-not-work).
* **Allow-list, not deny-list**, or it fails open on exactly the cases it cannot
  classify. See [The inversion](#the-inversion-allow-list-not-deny-list).
* **The identity is the cgroup, not the process group.** `setsid` is
  unprivileged, so `tracked_pgids` is advisory, not a boundary. See
  [The identity that holds](#the-identity-that-holds-cgroup-not-pgid).
* **Prerequisite: every activity needs a scope**, not just firewalled ones.
  That is the one change that makes the whole design possible, and it is
  currently conditional. See [Coverage](#coverage-the-prerequisite).

## What #144 is actually about

Worth restating, because the branch that closed sway's socket is labelled #144
throughout and closes something else.

`crates/shepherd-ipc/src/server.rs:140-157` derives a role from the peer's uid
at accept:

```rust
let uid = get_peer_uid(&stream);
let role = match uid {
    Some(0) => ClientRole::Admin,
    Some(u) if u == nix::unistd::getuid().as_raw() => ClientRole::Admin,
    _ => ClientRole::Shell,
};
let info = ClientInfo::new(role);
```

Every activity runs as shepherdd's own uid, so every activity gets `Admin`.
The role is then stored in `ClientInfo`, logged at connect — and **never
consulted at dispatch**. So an activity can call `logout`, `stop_current`,
`set_brightness`, `set_screen_power`, anything. There is no partial protection
here to preserve; the socket is fully open to anything running at that uid.

## Why the unlink does not transfer

Sway's socket could be unlinked because shepherdd could be made to hold its
connections: the #147 work replaced per-call `swaymsg` subprocesses with three
long-lived connections, and after that the socket's *name* was needed exactly
once. Removing it cost nothing.

shepherdd's socket is permanently in the position `swaymsg` was in, and by
design. `sway.conf` binds short-lived clients that connect by path at arbitrary
times, for the life of the session:

| trigger | client |
| --- | --- |
| `XF86AudioRaiseVolume` / `Lower` / `Mute` | `$launcher --volume-up` etc. |
| `XF86MonBrightnessUp` / `Down` | `$launcher --brightness-up` etc. |
| `Alt+F4`, `Ctrl+w`, `Home` | `$stop_current` |
| `swayidle` timeout / resume | `$launcher --screen-off` / `--screen-on` |

Each is a fresh process that connects, calls, exits — `shepherd-launcher-ui`'s
own comment describes the shape. There is no moment after which no new client
connects, so there is no moment at which the name becomes removable.

This is not an argument for changing that design. Short-lived CLI clients are
the right shape for compositor keybindings; the daemon holding state and the
keypress being a one-shot is exactly right. It just rules out the one technique
that worked next door.

## Why a bearer token does not work

The natural next thought — and the one this note exists to head off — is to
generate a random token at startup, give it to shepherdd and to the clients that
need it, and check it on each call.

It fails for the same reason hiding the socket path failed in #147, and the
dead ends were already checked there:

* `/proc/<pid>/environ` is readable at the same uid, so a token in sway's
  environment (or any client's) is readable by every activity. This is precisely
  how `SWAYSOCK` was shown to be undiscoverable-in-principle in the sway
  investigation — see `crates/shepherd-host-linux/src/sway_ipc.rs`'s module
  header.
* `/proc/<pid>/cmdline` is world-readable, so passing it in argv is worse.
* A token file is owned by shepherdd's uid, which is the activity's uid. File
  permissions cannot separate them.

Generalised: **under a shared uid, any secret a trusted client can read, an
activity can read.** A bearer token would look like protection while providing
none — the precise failure mode the #147 work exists to remove. Peer credentials
need no secret at all, which is why they survive the shared uid.

(The existing `http_token` is not a counterexample. It authenticates *remote*
HTTP and BLE callers, who are not at this uid and have no `/proc` access. It is
the right mechanism for that surface and the wrong one for this.)

## What the kernel gives you

On an `AF_UNIX` socket:

* **`SO_PEERCRED`** → `struct ucred { pid, uid, gid }`, captured by the kernel
  **at `connect()` time**, not read on demand. The peer cannot set it, alter it
  afterwards, or lie about it. `get_peer_uid` (`server.rs:394`) already reads
  this and discards everything but the uid.
* **`SO_PEERPIDFD`** (Linux 6.5+, comfortably inside the 26.04 floor) → a pidfd
  for the connecting process, also captured at connect.
* **`SCM_CREDENTIALS`** → sender-supplied, per message. Ignore it; `SO_PEERCRED`
  needs no cooperation from the peer.

`uid`/`gid` are useless here by definition. So the signal is `pid`.

### The pid-reuse race, and why the pidfd matters

`SO_PEERCRED`'s pid is trustworthy at the instant of connect. The race is in the
*lookup*: shepherdd takes pid P and reads `/proc/P/...`, but P may have exited
and been recycled in between. The connection outlives the process, so a peer
that exits immediately after connecting leaves shepherdd inspecting whatever
inherited its pid.

`SO_PEERPIDFD` closes it: the pidfd pins that specific process and is not
recycled, so the `/proc` read can be validated as referring to the same process
that connected. The exact retrieval path is worth checking against the shipping
kernel — the newer `PIDFD_GET_INFO` ioctl returns metadata directly, but its
availability is version-dependent and was not verified while writing this.

## The inversion: allow-list, not deny-list

The most consequential decision, and the easiest to get backwards.

A **deny-list** — "reject peers I recognise as activities" — **fails open**.
Anything unrecognised is trusted, and the unrecognised cases are the interesting
ones (see [Coverage](#coverage-the-prerequisite): Steam games satisfy no test
shepherdd currently has).

An **allow-list** — "accept only peers I positively recognise as legitimate
clients" — fails closed. An unclassifiable peer is refused, which is the correct
default for a boundary, and matches the posture #152 already set for the
firewall ("fail closed when a runtime scope's firewall cannot be applied").

Note what this rules out: **the binary cannot be the identity.**
`/usr/local/bin/shepherd-launcher` is readable and executable by the same uid,
so an activity can `exec` it and be byte-identical by `comm`, `/proc/pid/exe`
and `cmdline`. Identity has to come from *provenance* — where in the process
tree the peer came from — not from what it is running.

## The identity that holds: cgroup, not pgid

There is an attractive wrong answer here. shepherdd already computes
`tracked_pgids` (`adapter.rs`) for the by-name kill, and each activity gets
`setsid` at spawn, so an activity and its descendants share a pgid. It looks
ready to reuse.

**It is not a security boundary.** `setsid(2)` is unprivileged: any child of an
activity can call it and obtain a fresh session and process group that appears
in none of shepherd's tables. The adversary controls its own pgid. It remains
perfectly good at its actual job — not `pkill`ing a sibling session — which is a
safety check against shepherd's own actions, not against a hostile one.

The cgroup is different in kind. Moving a process between cgroups requires write
access to the destination's `cgroup.procs`. The firewall helper's transient
scopes live in the **system** manager (`adapter.rs:1860`, root-owned), so an
unprivileged activity cannot move itself out. It is a kernel-maintained label
the activity cannot forge, inherited by every descendant — including
double-forked and reparented ones, which is exactly where walking PPID chains
falls apart.

So: **cgroup is the identity, pgid is a hint.**

## Coverage: the prerequisite

The design above only works if every activity has a scope. Today it does not.

`adapter.rs:1867` creates one `if let Some(ref spec) = options.firewall`, and
only for `Process` kind with the helper installed and polkit granting. Snap and
Flatpak go through `apply_firewall_to_existing_scope`. Steam is excluded
outright — and a Steam game is a child of the long-lived Steam client, so it
shares no pgid with anything shepherdd spawned either. It satisfies neither
test, and is tracked only by `find_steam_game_pids`, a polled lookup by app id.

A non-firewalled activity is worse in a subtler way: it inherits shepherdd's
cgroup, which *is* the sway session's user scope — the same cgroup the launcher
and HUD live in. It is not merely unidentified; it is genuinely
indistinguishable from a trusted client by cgroup.

Hence the prerequisite, and it is the bulk of the work:

> **Decouple scope creation from `options.firewall`.** Every activity gets its
> own transient system-manager scope at spawn, whatever its kind and whatever
> its firewall config.

`SessionInfo.firewall_scope` (`adapter.rs:232`) already retains the scope name
per session — it only needs to be populated unconditionally, and renamed, since
it stops being about the firewall. Launching the Steam *client* into a scope
closes the Steam gap for free, because games inherit its cgroup; that is the
property `find_steam_game_pids` is currently reconstructing by hand.

With that in place the peer test is one question:

> Is the peer's cgroup at or below any scope shepherd created for an activity?
> → reject. Cannot determine the peer's cgroup? → reject.

## Where the check goes

`server.rs:140-157`, the existing accept path, which is already doing half the
job and dropping it. Two changes: derive the role from the peer's cgroup rather
than its uid, and actually consult it.

**Reject at accept, not at dispatch.** One decision per connection rather than
per call; it cannot be forgotten when someone adds a method; and it does not
leak the event stream to a peer that should not be reading state at all.

### Tiers

The local socket needs two roles. HTTP and BLE carry their own authentication
and are not in scope here.

* **Session control** — launcher, HUD, and the CLI one-shots. `get_state`,
  `subscribe`, `stop_current`, volume, brightness, `set_screen_power`.
* **Admin** — root, or a local operator shell. Policy edits, config reload,
  daily overrides.

`set_screen_power` (added on this branch for the `swayidle` fix) belongs in the
first tier: it is compositor plumbing invoked by `swayidle`, not an
administrative action.

A rejected peer is an administrator-facing condition and should raise a #143
diagnostic rather than only logging — an activity probing the management socket
is worth surfacing. Note the ordering constraint that already bit once on this
branch: the `DiagnosticPublisher` must be constructed before the IPC server if
the IPC server is a raise site. `run()` currently satisfies this.

## Open questions

Three assumptions this note reasons from but does not verify. All are cheap to
check on a real device (`leibniz`), and all should be settled before committing
to the design:

1. **Does a firewalled activity's `/proc/<pid>/cgroup` actually differ from the
   launcher's?** Read both on a live session. The whole design rests on this
   being true, and it has not been observed.
2. **Can an activity write another scope's `cgroup.procs`?** This confirms the
   boundary rather than assuming systemd's delegation defaults. Note that a
   non-firewalled activity today sits in the user's *delegated* subtree, where
   it can create sub-cgroups and move itself around freely — which is part of
   why every activity needs a system-manager scope.
3. **What does the process tree look like through
   `pkexec → shepherd-firewall-helper → systemd-run --scope`?** `--scope`
   normally leaves the command as its own child rather than reparenting to
   systemd, but the pkexec hop is worth seeing rather than assuming, because
   several tempting shortcuts depend on the answer.

## Related: the crash-recovery gap hardening opens

Making the unlink the default has a consequence recorded in the #147 note's
"Failure modes when a component dies" register, and repeated here because it
belongs to whoever owns hardening rather than whoever owns the transport:
`sway.conf`'s `… shepherdd … || swaymsg exit` fallback cannot connect once the
socket is unlinked, so a shepherdd that *crashes* mid-session leaves sway up
with no supervisor. A shepherdd that fails during *startup* is fine, because
hardening runs late in `run()` and the name still exists at that point.

## What this does not close

Peer credentials protect the *management socket*. They do not touch the
compositor as an attack surface: every activity is still a Wayland client, and
sway has no per-client protocol filtering — `zwlr_layer_shell_v1` can cover the
HUD, the virtual keyboard and pointer protocols can synthesize input,
`zwlr_screencopy_manager_v1` can capture the screen. The #147 branch's own
"what this does not close" section covers this and the reasoning is unchanged.
