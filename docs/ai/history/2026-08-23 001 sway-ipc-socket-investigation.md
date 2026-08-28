# Talking to sway over its IPC socket — investigation (issue #147)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/147>
> Prompt: "investigate #147"
> Related: #135, #136, #138 (supervision escapes), #141 (orphaned windows),
> #143/#145 (the diagnostics channel), #87 (docking, which owns `display_watch`).

## Verdict

The issue's diagnosis holds and its proposed shape is the right one. Three of
its supporting details are wrong, and three things it does not mention are the
parts that will actually cost time. Details below; the short version:

* **Do it, and hand-roll the client.** `swayipc-async` is on the wrong async
  runtime for this workspace (`async-io`, not tokio) — see [Dependency](#dependency-swayipc-async-is-on-the-wrong-runtime).
* **Split it in two.** The client + error surfacing is a self-contained safety
  win; the `window` subscription changes behaviour and carries the real design
  risk. See [Staging](#staging).
* **The window subscription is noisier than the poll it replaces** and needs a
  change-type filter plus a debounce, or it will make more compositor traffic
  than the 2 s sweep it removes.

## What was verified

Measured against a bare headless sway 1.11 on this dev box (idle machine, no
clients, 5304-byte `get_tree` payload — a real device's tree is larger and its
CPU busier, so this understates the gap):

| path | wall mean | wall p50 | wall p95 | CPU/call |
| --- | --- | --- | --- | --- |
| `swaymsg -t get_tree --raw` subprocess | 0.891 ms | 0.849 ms | 1.201 ms | 0.853 ms |
| persistent IPC socket, `GET_TREE` | 0.103 ms | 0.099 ms | 0.137 ms | 0.025 ms |

At the issue's 43,200 sweeps/day that is **36.8 s of CPU a day versus 1.1 s**
(excluding sway's own per-connection cost, which is not counted here). The
issue's own framing — "untidy, not urgent" — is the correct reading of that
number. Process count is not the reason to do this.

Also confirmed by reading the tree:

* `WINDOW_READY_POLL` = 500 ms over `WINDOW_READY_WATCH` = 300 s is 600 spawns
  per launch (`adapter.rs:59`, `:64`). The non-Steam path does exit early once
  the pid and pgid are dead; the Steam path really can run the full five
  minutes.
* `RECONCILE_EVERY_TICKS` = 20 on a 100 ms monitor tick (`adapter.rs:69`,
  `:1111`) is a sweep every 2 s for the daemon's whole uptime, running whether
  or not anything is escaped — `reconcile_escaped` queries the compositor
  *before* its `if snapshot.is_empty() { return; }` (`adapter.rs:810` vs `:819`),
  because the same list feeds `report_unowned_windows`.
* Opcodes and framing match the issue's description, per `sway-ipc(7)` on
  sway 1.11: 6-byte magic `i3-ipc` + `u32` length + `u32` type in **native**
  byte order (14 bytes), then JSON. `RUN_COMMAND` 0, `SUBSCRIBE` 2,
  `GET_OUTPUTS` 3, `GET_TREE` 4; events `output` 0x80000001, `window`
  0x80000003.
* Nothing in the tree tests the subprocess layer. `sway.rs`'s tests cover
  `walk`, `parse_outputs`, `parse_displays`, `select_primary`,
  `pick_mirror_mode` — every one of them a pure function fed a literal. The
  issue is right that a fake socket would be the first coverage this layer has
  ever had.

## Corrections to the issue

**`swaymsg --get-socketpath` does not exist.** The flag is on `sway`, not
`swaymsg` (`swaymsg: unrecognized option '--get-socketpath'`). More importantly
`sway --get-socketpath` is *not an independent fallback* — it echoes
`$SWAYSOCK`/`$I3SOCK` back. With the environment cleared while sway was running
it printed `sway socket not detected.` and exited non-zero. So socket discovery
is exactly "read `$SWAYSOCK`, then `$I3SOCK`", and there is nothing to fall back
*to*. The only other option is globbing
`$XDG_RUNTIME_DIR/sway-ipc.<uid>.*.sock`, which leans on a naming detail sway
does not document. Recommend: read the two variables, and make a miss a
diagnostic rather than papering over it.

**A subscription does not require a dedicated connection.** Verified: after
`SUBSCRIBE ["window","output"]` succeeded, a `GET_TREE` on the same connection
answered normally, and the reply carried its own payload type (`0x00000004`)
distinct from the event bit. Two connections is still the right call — it avoids
demultiplexing a reply stream against an event stream — but it is a
simplification we are choosing, not a constraint the protocol imposes. Worth
knowing because it means a single-connection design is a legitimate fallback if
two connections turn out to be awkward.

**`adapter.rs:921` is not the same defect as `adapter.rs:810`.** Both discard
the error, but the consequences differ. At `:921` (`spawn_window_watch`) a
failed query only delays or loses a `WindowReady`, and billing falls back to the
whole session — the documented safe direction. At `:810` (`reconcile_escaped`) a
failed query means the escape sweep closes nothing and
`report_unowned_windows` reports nothing, while the daemon concludes the screen
is clear. **`:810` is the whole safety argument**; `:921` is tidiness.

## What the issue does not cost

### The window event payload carries no workspace

Verified against a live subscription: a `window` event's `container` object is a
node with `id`, `pid`, `app_id`, `window_properties.class`, `type`, `focused`,
`nodes`, `floating_nodes` — and **no workspace**. Both consumers filter on
`in_scratchpad` (`adapter.rs:677` in `report_unowned_windows`, `adapter.rs:924`
in `spawn_window_watch`), and `in_scratchpad` is derived in `walk` from the
enclosing workspace node's name being `__i3_scratch` (`sway.rs`). It cannot be
recovered from the event.

This is not a problem — it is the design. Use the event as a *trigger* for one
`GET_TREE`, exactly as `display_watch.rs` already does for outputs ("We don't
inspect the payload — reconcile re-queries the full topology"). At 0.1 ms a
query that is affordable, and it leaves `walk` / `is_window` /
`report_unowned_windows` / `is_infrastructure` untouched and still tested.

### The `window` subscription is noisy

One `foot` terminal's entire life, from a live subscription, produced:

```
new     pid=… app_id=testterm name=None
title   pid=… app_id=testterm name="foot"
focus   …
floating …          <- `move scratchpad`
move    …
close   …
move    …           <- sway emits a second `move` after close
```

`title` fires on every title change; a browser or a game retitles constantly,
and `focus` fires on every focus shift. A naive event → `GET_TREE` would make
*more* compositor traffic than the 2 s poll it replaces. The subscription needs
a change-type filter (`new` / `close` / `move` / `floating` — the four that can
alter what is on screen and where) plus a short debounce (100–250 ms) so a burst
collapses to one pass. Reconciliation is already idempotent, so collapsing is
safe.

Note also that `move` and `floating` are what a scratchpad transition looks
like, so scratchpad moves *are* observable — good, since that is how the Steam
client is parked.

### `new` carries the pid before it carries a name

In the trace above the `new` event already had `pid` and `app_id` set while
`name` was still `null`. That is what makes exact first-window billing possible
and it means the matcher must key on **pid**, not name — which is what
`spawn_window_watch` already does (`wpid == pid || pid_in_group(wpid, pgid) ||
steam_pids.contains(…)`). Straight port.

### The escape-sweep tests currently depend on the silent failure

`adapter.rs:2155`, `:2188` and `:2251` call `LinuxHost::reconcile_escaped`
directly, in a test process with no compositor. They pass today *because*
`list_windows().await.unwrap_or_default()` swallows the error and hands back an
empty list. Make that error mean something and those three tests start raising a
spurious "cannot see the compositor" condition on every run.

So "stop discarding errors" is not a one-line change: `reconcile_escaped` needs
a window-source seam — a trait alongside `OutputBackend`, or an injected
closure — before the error can be surfaced. This is the single largest piece of
work the issue does not name, and it is also the piece that makes the new
behaviour testable at all.

### The diagnostics channel is ready, but not wired to the host adapter

#145 landed the machinery the issue points at, and `shepherd_api::DiagnosticSink`
(`crates/shepherd-api/src/diagnostics.rs:203`) exists for precisely this case —
its own doc comment names "the host adapter" as a raise site that "sit[s] in
crates that know nothing about shepherdd's registry". `DiagnosticPublisher`
(`crates/shepherdd/src/diagnostics.rs:432`) implements it.

But `LinuxHost` takes no sink today — there is no reference to `Diagnostic`
anywhere in `adapter.rs`. So this needs:

* a sink threaded into `LinuxHost` (constructor or a setter, since the adapter
  is built before the registry in `main.rs`),
* a new `DiagnosticCode::CompositorUnreachable` variant
  (`crates/shepherd-api/src/diagnostics.rs:41`), which regenerates the Kotlin
  and TS wire mirrors and fails `tests/rpc_codegen_drift.rs` until the
  regenerated files are checked in,
* raise-at-the-site / clear-on-next-success handling. This is an **observed**
  condition, not a probed one — nothing can retroactively ask "was the
  compositor reachable an hour ago" — so it uses `raise`/`clear` directly rather
  than the `ProbeFacts` path. Severity `Critical`: the config claims supervision
  the device is not providing.

### Sway does not restart under us

`sway.conf:166` `exec`s shepherdd from the compositor, and `docs/INSTALL.md:280`
records that shepherdd "runs as part of the kiosk session rather than as a
system service". If sway exits, the session goes with it. So the reconnect loop
is insurance against a transient socket error, not against a compositor
restart — worth having (it is ~15 lines, copied from `display_watch`'s respawn
loop) but it should not drive the design.

By the same token, `SWAYSOCK` missing from the environment is a dev or packaging
misconfiguration rather than a runtime race — which is exactly the case where a
diagnostic beats a silent empty list, because the symptom is otherwise "escape
supervision quietly does nothing, forever".

## Dependency: `swayipc-async` is on the wrong runtime

`swayipc-async` 3.0.0 depends on `async-io 2`, `async-pidfd`, `futures-lite`,
`serde`, `serde_json`, `swayipc-types 2`.

This workspace is tokio-only: `async-io`, `async-std` and `smol` appear nowhere
in `Cargo.lock`. (The two `polling` entries are `calloop`'s, via the wayland
stack — unrelated.) Adopting the crate would add a second async reactor, with
its own thread, to a daemon running on a handheld. It would also bring
`swayipc-types`' `Node`, which would displace the `parse_outputs` /
`parse_displays` / `walk` seam the issue explicitly wants kept as-is.

Cadence, for the record: 3.0.0 (2025-10-26), 2.1.1 (2025-09-17), 2.1.0
(2025-06-11), 2.0.4 (2024-11-11). Maintained, roughly a major a year.

**Recommendation: hand-roll.** The whole framing is a 14-byte header; the
request path is a `write_all` and two `read_exact`s on a `tokio::net::UnixStream`.
Measured above at 0.1 ms. It is less code than the adapter it replaces and it
keeps the tested parse seam intact.

## Recommended shape

New `crates/shepherd-host-linux/src/sway_ipc.rs`:

* framing (`Message` write, reply read) over `tokio::net::UnixStream`,
* socket discovery from `$SWAYSOCK` / `$I3SOCK`, error (not empty) on a miss,
* a request `Connection` behind a `tokio::sync::Mutex`, lazily connected and
  reconnecting on error,
* `subscribe(events) -> impl Stream`, on its own connection, with the respawn
  loop from `display_watch.rs`,
* one place that inspects `{"success": false}` on a `RUN_COMMAND` reply, so
  `sway.rs:95-103`'s per-caller convention disappears,
* unit tests against a fake socket: a `tokio::net::UnixListener` in a tempdir
  speaking the protocol back. First coverage this layer has had.

`sway.rs` keeps every public signature. `run_command`, `get_outputs`,
`get_displays`, `list_windows` route through the client. `parse_outputs`,
`parse_displays`, `walk`, `is_window`, `OutputBackend`, `SwaymsgBackend` are
untouched — though `SwaymsgBackend` wants renaming once nothing shells out.

`display_watch.rs` moves onto `subscribe(["output"])` and drops its subprocess;
the host adapter gets its own `subscribe(["window"])`. Two subscription
connections rather than one demuxed: the two consumers live in different crates
and neither wants the other's events.

## Staging

**PR 1 — the client, and the error that matters.** `sway_ipc.rs` + fake-socket
tests; `sway.rs` onto it; `logout`'s `swaymsg exit` onto `RUN_COMMAND`; the
window-source seam in `reconcile_escaped`; `CompositorUnreachable` raised at
`adapter.rs:810` and cleared on the next success. No behaviour change beyond
"a failed query is no longer indistinguishable from an empty screen." This is
the entire safety argument and is independently mergeable.

**PR 2 — the subscription.** `window` events with the change-type filter and
debounce; `RECONCILE_EVERY_TICKS` becomes a slow safety-net sweep;
`WINDOW_READY_POLL` becomes a `window::new` match. This is where behaviour
changes and where the risk is.

## Open decisions, with a recommendation for each

* **Crate vs hand-rolled** → hand-rolled. Runtime mismatch, above.
* **Safety-net sweep: keep, and at what cadence?** → Keep, at 60 s. It is the
  only thing that catches a bug in the event handling itself, and at 0.1 ms a
  query the cost is nil. Removing it trades a cheap backstop for nothing.
* **Does `logout` move?** → Yes. The issue makes its own argument: leaving one
  shell-out behind is how the current situation got justified.
* **`WINDOW_READY_POLL`** → Replace with `window::new` matched by pid, keeping
  the `WINDOW_READY_WATCH` deadline as the give-up bound. Expect recorded play
  time to shift by up to 500 ms per session; any test asserting on billing
  boundaries needs to tolerate that.
* **Unlisted, but decide it:** event-driven orphan detection will report
  short-lived windows the 2 s sweep currently misses entirely — including
  legitimate flashes from shepherd's own launches. `report_unowned_windows` is
  report-only and dedupes by pid, so this is log noise rather than a
  correctness problem, but the noise floor will rise. Consider requiring a
  window to survive the debounce window before it counts as an orphan.

## Reproducing the measurements

A bare headless sway is enough — the full `./scripts/shepherd dev headless`
stack is not needed:

```sh
printf 'default_border none\nxwayland disable\n' > /tmp/bench-sway.conf
setsid env WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    sway -c /tmp/bench-sway.conf --unsupported-gpu >/tmp/sway.log 2>&1 &
export SWAYSOCK=$(ls /run/user/$(id -u)/sway-ipc.*.sock | head -1)
# … benchmark / subscribe …
swaymsg exit
```

`foot -a testterm sh -c 'sleep 30'` gives a client to generate window events
against, and `swaymsg '[app_id=testterm] move scratchpad'` exercises the
scratchpad transition.

---

# Hardening sway's IPC, in light of #144

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/144>
> Prompt: "also considering #144, can anything be done here to harden Sway's IPC"

Short answer: **yes, and #147 is what makes it possible.** A persistent
connection means the socket path is needed exactly once, at startup — after
which shepherdd can `unlink()` it and no new client can ever connect. Verified
working on sway 1.11. It is the only measure found that is a real boundary
rather than an obstacle, and it is unavailable as long as every compositor call
re-execs `swaymsg`.

## Sway's socket has #144's defect, with worse consequences

#144's mechanism applies verbatim: same uid, no credential, no token. But
sway's IPC grants strictly more than `ManagementService` does. Verified against
a live sway 1.11:

* **`RUN_COMMAND "exec …"` is arbitrary command execution outside supervision.**
  `swaymsg "exec touch /tmp/pwned-by-activity"` returned `{"success": true}` and
  the file appeared. The spawned process is sway's child, not shepherdd's — so
  it has no pgid in `processes`, is not in the session's systemd scope, and
  **is not inside the cgroup the firewall BPF program is attached to**
  (`crates/shepherd-firewall-helper/src/bpf.rs:36`, `apply_cgroup` +
  `BPF_CGROUP_INET_EGRESS`). An activity configured `default_deny` can reach the
  network by asking sway to make the request for it.
* **`exit` ends the kiosk session.** shepherdd is `exec`'d from `sway.conf:166`
  and dies with it.
* `kill` closes any window by criteria, including the HUD and the launcher.
* `output … disable` / `dpms off` blanks the screen; `scratchpad show` reveals
  the parked Steam client; `seat - cursor …` synthesizes pointer input;
  `input …` remaps devices; `GET_TREE` enumerates everything on screen.
* There is **no access control to turn on**. sway's `ipc`/`permit` security
  blocks were removed in 1.0; `man 5 sway` on 1.11 has nothing.

Where it differs from #144 in our favour: **`SWAYSOCK` is not leaked to
activities.** `INHERITED_ENV_VARS` (`crates/shepherd-host-linux/src/process.rs:186`)
is an allowlist and neither `SWAYSOCK` nor `I3SOCK` is on it. That is worth a
regression test — it is currently true by omission, and one plausible-looking
addition to that list would undo it.

## What does not work (checked, so nobody re-checks)

**Permissions and ownership.** #144's conclusion holds here too. The socket
itself is `srwxrwxr-x`; what protects it is `$XDG_RUNTIME_DIR` being 0700. Same
uid, so an activity walks straight in.

**Hiding the path.** Two independent leaks, both verified:

* `/proc/net/unix` is world-readable and lists the full bound path of every
  filesystem socket on the machine. An unguessable filename buys nothing.
* `/proc/<sway-pid>/environ` is readable at the same uid and contains
  `SWAYSOCK=…` verbatim.

**Stealing the listening fd instead.** Not a path: a socket cannot be reopened
through `/proc/<pid>/fd/<n>` — every attempt returned `ENXIO` (errno 6). And
`ptrace_scope` is 1 on this host, under which an activity cannot ptrace sway
(it is a descendant of sway, not an ancestor of it).

## What does work: unlink the socket once shepherdd has connected

Verified end to end on sway 1.11:

1. shepherdd connects to `$SWAYSOCK`.
2. shepherdd `unlink()`s the path.
3. The existing connection keeps working completely — `GET_VERSION` and
   `RUN_COMMAND` both still succeed after the unlink.
4. A new client connecting by path fails:
   `[ERROR] [common/ipc-client.c:66] Unable to connect to …`.
5. sway stays alive and unaffected; it holds the listening fd, it just has no
   name in the filesystem any more.
6. `/proc/net/unix` still lists the now-dangling path, which is harmless —
   there is nothing there to connect to.

That converts sway's IPC from an ambient capability that every same-uid process
holds for the life of the session into a one-shot one that shepherdd consumes
at startup. **It is only available because of #147.** Today every call re-execs
`swaymsg`, which requires the path to exist forever; a persistent connection
needs it once.

Production is already arranged for this: shepherdd is the only sway IPC client
in the shipped session. `crates/shepherd-launcher-ui`, `crates/shepherd-hud` and
`crates/shepherd-pairing-display` contain no reference to `swaymsg` or
`SWAYSOCK`; `swayidle` and `swaybg` use Wayland protocols, not sway IPC.

### What it costs

* **It forecloses reconnect**, which #147 asks for. Once the path is gone a
  dropped connection cannot be re-established. Two ways out: open every
  connection needed at startup (request + `window` events + `output` events,
  plus a spare or two) and treat a loss as fatal; or keep the path and give up
  this hardening. Given shepherdd dies with sway anyway, "the connection is
  gone, therefore the session is over" is defensible — and it is exactly the
  `CompositorUnreachable` diagnostic from PR 1, raised once and terminally.
  This tension needs deciding before PR 1 fixes the connection lifecycle.
* **`sway.conf:66`** (`bindsym Mod4+Shift+Escape exec pkill -TERM shepherdd &&
  swaymsg exit`) breaks. It already kills shepherdd first, and the comment
  beside it says to remove or rebind it in production; the clean version routes
  the whole thing through shepherdd's `logout`, which #147 is moving onto the
  socket regardless.
* **The dev and test workflow breaks outright.** `scripts/lib/headless.sh`
  drives `swaymsg` for structural assertions (`get_tree`), pointer input
  (`seat - cursor`), mode setting and shutdown, and both the `headless-dev` and
  `companion-pairing` skills depend on it. This must be **off by default in
  dev** and on only under an explicit setting — the same shape as the e2e escape
  hatch #144 already calls for. Getting that switch wrong makes the repo's
  primary verification path unusable.
* Ordering: unlink only after every connection is up, and only after the
  launcher and HUD have started, in case either grows a sway IPC dependency
  later. A comment at the unlink site should say so.

## Two cheap complements

**Keep `SWAYSOCK` off the inherited-env allowlist, in a test.** A unit test on
`build_inherited_env` asserting `SWAYSOCK` and `I3SOCK` are absent. Cheap, and
it converts an accident into an invariant.

**Relocate sway's socket out of `$XDG_RUNTIME_DIR`.** Verified: sway honours a
pre-set `SWAYSOCK` and then does *not* create the default
`$XDG_RUNTIME_DIR/sway-ipc.<uid>.<pid>.sock` at all. This buys nothing on its
own (see the `/proc/net/unix` leak) but it is the enabling change for #105's
per-activity-filesystem option: an activity needs `$XDG_RUNTIME_DIR` for
Wayland, PipeWire and D-Bus, so that directory can never be hidden — a separate
directory simply is not bind-mounted. Two caveats: `sun_path` is 108 bytes and
truncation is silent (observed while testing — a long path bound at a truncated
name with no error), and `headless.sh`'s socket discovery globs the default
`sway-ipc.*` name and would need updating.

Worth noting for #105's own scoping: **uid separation closes sway IPC for
free**, because the 0700 `$XDG_RUNTIME_DIR` blocks a different-uid activity
before the socket's own 0775 mode matters. That is one more entry on the ledger
for uid separation over a namespace-only approach.

## What none of this closes

Every activity is still a Wayland client, and sway has no per-client protocol
filtering. `strings` over sway 1.11 and its wlroots confirms it advertises,
among others:

* `zwlr_layer_shell_v1` — cover the HUD from an overlay layer,
* `zwp_virtual_keyboard_v1`, `zwlr_virtual_pointer_v1` — synthesize input,
* `zwlr_output_power_manager_v1`, `zwlr_output_manager_v1` — blank or
  reconfigure outputs,
* `zwlr_screencopy_manager_v1`, `ext_image_copy_capture_manager_v1` — capture
  the screen,
* `zwlr_foreign_toplevel_manager_v1`, `ext_foreign_toplevel_list_v1` — enumerate
  and act on other windows,
* `zwlr_data_control_manager_v1` — read the clipboard.

So closing the IPC socket removes the *command* channel, not the compositor as
an attack surface. It is still worth doing — `exec` is the only one of these
that escapes the firewall and the supervisor — but the claim should be stated
that narrowly.

## And the detection half

Independently of any of the above, PR 2 of #147 is the detection story for this
threat. An `exec`'d process that maps a window is caught by
`report_unowned_windows` the moment `window::new` arrives, instead of up to 2 s
later or — for a window that maps and unmaps inside one sweep — never. An
`exec`'d process that maps no window remains invisible to the compositor; that
one belongs with the #135–#137 orphan work, as "processes in the session cgroup
that shepherd did not spawn".

One gap the subscription would also make fixable, currently impossible with a
sampling sweep: **nothing notices when a shepherd window disappears.**
`is_infrastructure` (`adapter.rs:639`) exempts the launcher, HUD, pairing
display and `wl-mirror` from orphan reporting, and there is no counterpart that
reacts to one of them going away. `sway.conf`'s `exec`/`exec_always` do not
respawn on process death, so an activity that issues `[app_id=org.shepherd.hud]
kill` removes the HUD for the rest of the session. A `window::close` on an
infrastructure `app_id` is exactly the signal for a respawn or a `Critical`
diagnostic. Beyond #147's scope, but it is the same subscription.

---

# Keeping the headless workflow alive under an unlinked socket

> Prompt: "go more into the ways to keep headless working"

## The blast radius is one file

Checked every consumer:

* **`scripts/lib/headless.sh` is the only thing that breaks.** Every sway IPC
  call in the repo's tooling goes through its `headless_run` helper (:56).
* **`crates/shepherd-e2e` is unaffected.** It starts its own sway
  (`src/lib.rs:288`) but never speaks sway IPC — it sets `WAYLAND_DISPLAY`
  (`:364`, `:635`) and nothing else, and its window-level assertions are
  actually argv assertions (`tests/browser.rs:155`). So #144's "the e2e suite
  needs an explicit escape hatch" constraint does **not** extend to this work.
* **`grim` and `wtype` keep working.** They are Wayland protocol clients
  (screencopy and virtual-keyboard); `headless_run` passes them `SWAYSOCK` but
  they never read it. So screenshots and keyboard input survive an unlinked
  socket untouched. Only the pointer path (`swaymsg seat - cursor`) is affected.

### What `headless.sh` actually needs, and when

| call | site | purpose | when it runs |
| --- | --- | --- | --- |
| `swaymsg -t get_version` | `:95` | readiness probe | **session startup, racing shepherdd** |
| `swaymsg "output $OUT mode $size"` | `:354` | pin the virtual output | **session startup, racing shepherdd** |
| `swaymsg -t get_tree` | `:107`, `:415`, `:420` | wait-for-launcher; `dev tree` | throughout |
| `swaymsg "output * dpms on"` | `:400` | un-blank before `dev shot` | throughout |
| `swaymsg "seat - cursor set/press/release"` | `:451`–`:454` | `dev click` | throughout |
| `swaymsg exit` | `:467` | `dev stop` | teardown |
| `swaymsg output … scale`, `-t get_outputs`, `-t get_seats` | `headless-dev` SKILL.md:119–144 | HiDPI and seat debugging | ad hoc |

**The two startup calls are the awkward ones.** `headless_start` boots sway with
the repo's own `sway.conf` — or a derived copy in which only the
`shepherdd -c <path>` token is rewritten (`:230-235`) — so the headless session
`exec`s shepherdd exactly like production. `headless_wait_ipc` then polls
`get_version` in a 0.1 s loop while shepherdd is starting up in parallel. If
shepherdd unlinks whenever it happens to finish connecting, that probe becomes a
coin flip. **Any design where the harness races the unlink is unacceptable** —
it produces exactly the intermittent failure that is most expensive to debug.

## Mechanisms, all verified on sway 1.11

### A hard link survives the unlink

```
ln $SWAYSOCK /run/user/1000/alias.sock   # link created
rm  $SWAYSOCK                            # ambient name gone
SWAYSOCK=/run/user/1000/alias.sock swaymsg -t get_version   # → 1.11
```

Connecting resolves the path to the socket's inode, and the inode is alive while
any name points at it. Requires the alias to be on the same filesystem as the
socket, i.e. inside `$XDG_RUNTIME_DIR`.

### A bind mount does the same, across filesystems

`mount --bind $SWAYSOCK /run/shepherd-alias/sway.sock` then removing the source
leaves the bind-mounted path fully connectable — verified across the `/run` and
`/run/user/1000` tmpfs boundary. Needs root and leaves an entry in
`/proc/mounts`; the hard link needs neither. Use it only if the alias must live
outside the runtime dir.

### `/proc/net/unix` does not leak the alias

It lists the **bind-time** path only. After linking to `hatch/s.sock` and
unlinking the original, `/proc/net/unix` still showed the original (now dead)
path and never the alias. So an alias name is not disclosed there — the leak
that defeats path-hiding for the *ambient* socket does not apply to a link made
afterwards.

### A root-owned 0700 alias directory is a real boundary

```
drwx------ 2 root root  /run/user/1000/rootipc     (root creates the link)
unprivileged connect → Unable to connect to …/rootipc/sway.sock
unprivileged ls       → Permission denied
unprivileged rmdir    → Directory not empty      (and it cannot be emptied)
root connect          → 1.11
```

`swaymsg` run as root against a user-owned socket works fine (root bypasses the
0775 mode). So this hatch is usable in production — but root must create the
link *before* shepherdd unlinks, which is the race again.

### A 0730 group-writable, non-listable directory closes that race — with a catch

```
drwx-wx--- 2 root shepherd-dev  /run/user/1000/hatch    (mode 0730)
shepherdd (unprivileged, in the group) creates the link  → OK
the same user lists the directory                        → Permission denied
connect by the known alias name after the unlink         → 1.11
```

Mechanically perfect: shepherdd can create the alias itself (no race), and a
same-uid activity can neither list the directory nor learn the name from
`/proc/net/unix`. **The catch is name delivery.** The name has to be
unguessable, so it cannot be a fixed string; but it also cannot travel in argv
or the environment, because `/proc/<shepherdd-pid>/cmdline` and `environ` are
readable at the same uid — the same leak that defeats hiding the socket in the
first place. Nor can it be logged, since `log_dir` is under the user's state
directory. There is no channel left that reaches an operator without also
reaching an activity. Recording it here so nobody rebuilds this and ships the
leak.

## Options, ranked

**1. `--sway-ipc-alias <path>` on shepherdd — recommended.**

shepherdd connects, hard-links the socket to `<path>`, then unlinks the ambient
name. One code path: it always unlinks; the only conditional is whether an alias
was requested first. No race, because shepherdd does both operations itself in
order. It matches the existing CLI style exactly — `main.rs:71` already uses
`#[arg(short, long, env = "SHEPHERD_SOCKET")]`, so this becomes
`env = "SHEPHERD_SWAY_IPC_ALIAS"`.

`headless.sh` picks it up at the insertion point it already owns: the derived
sway config at `:230-235` rewrites the `shepherdd -c <token>` line with `sed`
and validates the result with `grep -qF`. Extending that rewrite to append
`--sway-ipc-alias "$rt/sway-dev.sock"` is a two-line change to an already-guarded
mechanism. `headless_run` then exports the alias as `SWAYSOCK` and every one of
the seven call sites above works unmodified.

Startup ordering stays correct without any new synchronisation:
`headless_wait_ipc` polls the **alias**, which does not exist until shepherdd
has connected and created it — so the probe now waits for "sway up *and*
shepherdd connected" instead of just "sway up". That is a strictly better
readiness signal than the current one, and it removes the implicit
`sleep 1`-shaped assumptions around `exec sleep 1 && $hud`.

What it does not give you: production stays hardened, so debugging a live
device means restarting the session with the flag set. For a kiosk that is
normal and worth stating in `docs/INSTALL.md`.

**2. Plain on/off config key, hardening disabled in dev.**

Simplest to write, and the worst of the set: the hardened path then never runs
in dev or CI, so it rots and the first time anyone exercises it is on a real
device. If this is chosen anyway, it needs a dedicated integration test that
turns it on and asserts a second connection is refused — which is most of the
work of option 1 without the benefit.

**3. Move the read-only assertions onto shepherd's own API.**

`ManagementService::list_windows` (`crates/shepherd-management/src/service.rs:165`)
returns `WindowInfo`, which carries `app_id`, `window_class`, `focused`,
`visible`, `workspace`, `in_scratchpad`, `pid` and `owner`
(`crates/shepherd-api/src/types.rs:1048`). That is **everything**
`headless_tree`'s jq filter extracts (`:415`) except `fullscreen_mode`, and it
covers `headless_wait_launcher`'s app_id grep outright.

Worth doing on its own merits — it shrinks the surface needing any escape hatch
to the pointer, the output mode and teardown. But it should not be the *whole*
answer: `dev tree` exists partly as an independent view of the compositor, and
routing it through the daemon under test means a shepherdd that misreports the
tree also misreports it to the test. Keep a raw path.

**4. Reject: a sway passthrough over shepherdd's management socket.**

The tempting design — `shepherd dev sway <command>` — hands back precisely the
capability the unlink removed, because per #144 that socket is reachable by
every activity with no credential. A read-only variant is less bad but still
gives an activity `get_tree`. If it were built it would need the same dev gate,
so it costs more than option 1 and protects less. Note that `headless-dev`
SKILL.md:141 already records "there is no `dev swaymsg` passthrough" as a
deliberate property; this is the reason to keep it that way.

## Follow-on edits, whichever option wins

* `headless_socket_as` (`:120-137`) globs `sway-ipc.*.sock` to discover the
  session's socket. If the *relocate* idea from the previous section also lands,
  that glob and the `SWAYSOCK=$swaysock` line written into the session env file
  (`:363`) both need updating.
* `headless_stop` (`:467`) uses `swaymsg exit`. With an alias it keeps working;
  without one it needs to fall back to the `SIGTERM` path already below it
  (`:476` `pkill -x shepherdd`) plus a kill of the recorded sway pid.
* `sway.conf:66`'s `Mod4+Shift+Escape … swaymsg exit` binding is unaffected in
  dev if the alias is exported into sway's environment, and should be rewritten
  to go through shepherdd's `logout` for production regardless.
* Both `headless-dev` and `companion-pairing` SKILL.md files describe driving
  `swaymsg` directly and would need a line about the alias.

---

# What is still degraded once the alias is in place

> Prompt: "once this is in place, what about the headless is broken (or at least
> more difficult to drive)"

The alias restores every one of the seven `headless.sh` call sites verbatim.
What it does not restore is **independence**: today the compositor can be
inspected whether or not shepherdd works, and afterwards it cannot. Everything
below follows from that one change.

## The real regression: shepherdd becomes a dependency of inspection

Today `swaymsg` answers as soon as sway is up. Afterwards the drivable path is a
name shepherdd creates, so **a broken shepherdd takes the debugging tools with
it** — precisely when they are wanted. Three concrete consequences:

**`headless_wait_ipc` starts lying.** Its failure message
(`headless.sh:299-306`) is "Headless Sway did not answer IPC within 10s", and
after this change the most likely cause is that *sway is fine and shepherdd
never started*. `headless-dev` SKILL.md:191 already documents that exact
scenario (a missing `shepherdd` binary, after which sway's `|| swaymsg exit`
tears the session down). Diagnosing it currently takes one `swaymsg` call;
afterwards the two failures are indistinguishable from the harness.

*Fix, and it is an improvement over today:* probe in two stages. The ambient
socket exists from sway start until shepherdd unlinks it, so "ambient **or**
alias answers" means sway is up, and "alias exists" means shepherdd connected.
That distinguishes the two failures for the first time, and it replaces the
`exec sleep 1 && $hud` guesswork with a real signal.

**`headless_start` must stop treating no-IPC as fatal.** It currently `die`s if
IPC does not answer in 10 s (`:344-350`), tearing down the session. If shepherdd
is the broken component, that destroys the evidence. It should warn, record the
session anyway, and leave `dev shot` working — see the fallback ladder below.

**A half-started shepherdd can strand the session.** The bad state is
"shepherdd connected, unlinked the ambient name, and the alias is missing".
Design it out rather than handling it: **create the alias first, and unlink only
after it exists.** If an alias was requested and could not be made, do not
unlink at all — a session that is still drivable is strictly better than one
that is hardened and inert. Correspondingly the alias must be created as the
very first thing after connecting, before any other startup work, so that
"shepherdd reached the compositor" is its only precondition.

## The fallback ladder, if the alias is unavailable anyway

| capability | tool | survives a missing alias? |
| --- | --- | --- |
| `dev shot` | `grim` (wlr-screencopy) | **yes** — never touches `SWAYSOCK` |
| `dev key` / `dev type` | `wtype` (virtual-keyboard) | **yes** — same |
| `dev tree` | `swaymsg -t get_tree` | no |
| `dev click` | `swaymsg seat - cursor` | **no, and there is no substitute** |
| `dev stop` | `swaymsg exit` | degrades to `pkill -x shepherdd` + kill the recorded sway pid (`:476`) |

So an agent keeps eyes and a keyboard, and loses structural assertions and the
pointer. **The pointer is the one capability with no alternative:**
`scripts/deps/agent.pkgs:16-18` records the reasoning — `wtype` has no pointer
support and `ydotool` needs root, which is *why* clicks go through sway IPC.
This is partly mitigated by the fact that `dev click` is already the weakest
tool in the kit (SKILL.md:94 — synthetic cursor events do not fire GTK4
`connect_clicked` handlers, so the documented advice is already to drive the app
another way), but it means a missing alias removes it outright rather than
making it flaky.

## A destructive footgun to design against

**shepherdd must never unlink `$SWAYSOCK` by default.** A developer or agent
running `cargo run -p shepherdd` from a terminal inside their own sway desktop
would have `SWAYSOCK` pointing at their real session, and shepherdd would delete
it — breaking every `swaymsg`, status bar and script in that session until
logout, with no obvious cause. Nothing in `CONTRIBUTING.md` documents running
shepherdd standalone, but it is an obvious thing to try, and the blast radius is
the developer's whole desktop.

The conclusion is that the hardening has to be **opt-in** — set by the
production installer, absent everywhere else — rather than on-by-default with a
dev opt-out. That is the reverse of how the flag was framed in the previous
section, and it is the safer default: the worst case for opt-in is an unhardened
device, and the worst case for opt-out is a wrecked developer session.

Checked and *not* a problem: `sway_dev_run` starts a nested sway
(`scripts/lib/sway.sh:162-198`) without clearing the outer `SWAYSOCK`, but sway
falls back to generating its own `sway-ipc.<uid>.<pid>.sock` when the preset
path already exists — verified by starting a second sway with the first one's
`SWAYSOCK` still set. So the nested session's children get the nested path, and
`dev run` is safe.

## Smaller things that get more awkward

**Restarting shepherdd in place stops working — unless it uses the alias.** Once
the ambient name is gone a second shepherdd has nothing to connect to. With the
alias present it can connect through it, but the implementation has to expect
that: `link()` returns `EEXIST` on a repeat (verified), so alias creation must be
idempotent, and shepherdd must **never unlink the alias** on startup or
shutdown — a naive "unlink then link" in the second process would brick the
session for the first. In production this means shepherdd genuinely cannot be
restarted within a session, which matches the current architecture (it dies with
sway anyway) but should be stated in `docs/INSTALL.md`.

For the documented dev loop this is not a regression: `headless-dev`
SKILL.md:37 already prescribes tearing the session down and respawning
("respawn is cheap"), not restarting shepherdd in place.

**Reattaching to an old session becomes impossible.** `headless_load_session`
reads a saved env file and reattaches to a live session. A session started
before this change, or by a shepherdd without the flag, has no recoverable
socket path — nothing can drive it again for the rest of its life. Today any
live sway is drivable. Losing that "let me just poke at it" affordance is minor
in aggregate and annoying in the moment.

**Socket discovery has to change.** `headless_socket_as` (`:120-137`) globs
`sway-ipc.*.sock` to find "the sole socket" in a runtime dir, and `:363` writes
that path into the session env file. Both must record the *alias*. Getting
`:363` right is the single most important line in the whole change — every ad
hoc `swaymsg` an agent types goes through `headless_run`, which sources
`SWAYSOCK` from that file, so if it records the alias then everything in
SKILL.md (`output … scale`, `-t get_seats`, `-t get_outputs`) keeps working
untouched.

Two concurrent sessions in one runtime dir also need distinct alias names; the
current glob works because it assumes exactly one. A fixed `sway-dev.sock` would
collide, so include the sway pid the way the default name does.

**`--user` mode is unchanged but stays clumsy.** The alias lands inside the
session user's 0700 runtime dir, so the invoker still cannot reach it directly
and every call still routes through `sudo -u`. No worse than today, but no
better either — and discovery gets *simpler*, since a deterministic alias path
replaces the sudo'd glob.

**Habits that stop working.** Anything that finds the socket for itself — a
copied one-liner, `sway --get-socketpath`, `ls $XDG_RUNTIME_DIR/sway-ipc.*` —
now yields a dead path. Inside `headless_run` this is invisible; outside it, it
is a confusing five minutes. Worth a line in `headless-dev` SKILL.md saying the
session's socket is the alias and `headless_run` is the only supported way to
reach it.

## What is unaffected

`crates/shepherd-e2e` (starts sway, never speaks sway IPC), `grim`, `wtype`,
`swaybg`, `swayidle` (Wayland protocols, not sway IPC), and the
`companion-pairing` skill, which drives adb and the shepherd stack and issues no
`swaymsg` of its own.

## Rebase onto `main`, 2026-08-27

Rebased the four commits onto `bf6651f` (`main` after the graphical config
editor, the RetroArch activity kind, the BPF alignment fix, and the CI move to
Ubuntu 26.04). Three conflicts, all in the same shape — this branch and `main`
each added something new at the same seam — plus one the merge resolved cleanly
and wrongly.

**`adapter.rs`, twice: keep both.** `main`'s #129 work added `tracked_pgids`
(the by-name kill's protected set) immediately after `kill_activity`, which is
exactly where this branch put `windows_for_sweep` and `start_window_watch`.
Nothing overlapping, just adjacent insertions.

**`shepherd-webui/src/api/types.ts`: take `main`, then regenerate.** The wire
types are generated now (#139 codegen), so `DiagnosticCode` moved to
`wire-types.generated.ts` and `types.ts` only re-exports it. The
`compositor_unreachable` variant this branch added by hand had to come back
through `cargo run -p shepherd-wire-codegen --bin rpc-codegen` instead — the
Rust side (`diagnostics.rs`) merged clean, so the regeneration is the whole fix.
`tests/rpc_codegen_drift.rs` catches this, but only under `cargo test
--workspace`; the codegen crate is outside `default-members` and a bare `cargo
test` skips it silently.

**`main.rs`: a silent duplicate.** This branch *moves* the
`diagnostics_changed` channel earlier in `run()`, so the host adapter can be
handed a `DiagnosticPublisher` before its monitor starts. `main` had edited the
code around the old declaration, so the three-way merge applied the addition and
kept the original — two `let (diagnostics_changed_tx, mut
diagnostics_changed_rx)` bindings, the second shadowing the first. It compiles.
The only symptom is an `unused_variables` warning on the *first* receiver, which
is the one the main loop is supposed to select on: every diagnostic raised by
the host adapter would have been published to a channel nobody read. Worth
remembering that a moved declaration is the conflict a rebase is least likely to
show you.

Revalidated with `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace --all-targets`, and the
web UI's `check:boundary` / `check:coverage` / `test` / `typecheck`, then
end-to-end through `dev headless --harden-ipc`: the log shows the window-event
subscription and the output-event watch opening, the socket aliased and then
unlinked, a launch billed off `Activity window appeared` (the `list_windows`
hot path this branch rewrote), and the launcher focused again after
`stop_current` — with no `CompositorUnreachable` raised at any point.
