# Emulators / RetroArch save states — scope (issue #125)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/125>

## Prompt

> scope out #125, particularly the retroarch-based save states

Scoping only — no implementation in this pass.

## Issue text

> **Emulators example**
>
> This should go in the example config and maybe a wiki entry, likely no code
> required.
>
> Simply show using RetroArch with direct launch via its CLI.

Comment 1:

> Some code is probably useful for save state management -- real usage shows
> that we should be saving/restoring the state on close and open.
>
> Two things fall out of that:
>
> - A dedicated "reboot" button to properly reset the console, likely on the
>   HUD, as that ability is lost when a default savestate is used
> - "emulator" or "retroarch" entry type so the above can be implemented cleanly

Comment 2 — the motivating entry:

```toml
[[entries]]
id = "pokemon-firered"
label = "Pokemon FireRed"
xwayland_native_resolution = true

[entries.kind]
type = "process"
command = "retroarch"
args = ["-L", "mgba_libretro.so", "Games/retroarch/pokemon-firered.zip"]
```

## Headline finding: the doc-only version of this issue silently eats saves

The entry above does not just *lack* save-state handling — as of today it
**loses the child's in-game save every time the activity is closed**, and
turning on RetroArch's own auto-save settings would not fix it.

RetroArch installs one `SIGTERM`/`SIGINT` handler
([`frontend/drivers/platform_unix.c`][sig]) that counts signals:

```c
unix_sighandler_quit++;
if (unix_sighandler_quit == 1) { /* request clean shutdown */ }
if (unix_sighandler_quit == 2) exit(1);
if (unix_sighandler_quit >= 3) abort();
```

The first signal asks for a clean quit — which is what flushes SRAM (the `.srm`
battery save, i.e. the actual Pokémon save file) and writes the auto save state.
The **second signal calls `exit(1)` immediately**, skipping both.

Shepherd's graceful stop sends more than one:

- `crates/shepherd-host-linux/src/adapter.rs:860` — `kill_by_command(&info.command_name, SIGTERM)`,
  which is `pkill -TERM -f retroarch` (`process.rs:602`).
- `crates/shepherd-host-linux/src/adapter.rs:873` — `p.terminate()`, which
  (`process.rs:842`) *also* calls `kill_by_command(...)`, then
  `kill(-pgid, SIGTERM)`, then signals every descendant PID individually.

So a `process`-kind RetroArch receives at least two `SIGTERM`s back-to-back,
before its main loop gets a chance to run the shutdown path. It hits `exit(1)`.
Nothing is saved. This is deterministic, not a race.

This changes the shape of the issue: **the "no code required" doc-only version
is not deliverable as-is.** The minimum honest deliverable is Phase 1 below.

[sig]: https://github.com/libretro/RetroArch/blob/master/frontend/drivers/platform_unix.c

Two secondary consequences worth noting for their own sake:

- The double-`SIGTERM` applies to *every* `process`-kind entry, not just
  emulators. Any app that saves on `SIGTERM` and de-duplicates or counts
  signals is affected. Worth a separate issue.
- `pkill -f retroarch` matches on the full command line of *every* process on
  the box. A second RetroArch (an admin's own, over SSH/VT) gets killed too.
  Pre-existing hazard of `kill_by_command`; emulators just make it likely.

## What RetroArch actually gives us

Verified against RetroArch `master` (`config.def.h`, `command.h`, `command.c`),
not from memory.

| Need | Mechanism | Default |
|---|---|---|
| Restore on open | `savestate_auto_load = "true"` — loads `<savestate path>.auto` at startup | `false` |
| Save on close | `savestate_auto_save = "true"` — writes `<savestate path>.auto` at end of process lifetime | `false` |
| In-game save durability | `autosave_interval = <secs>` — periodic SRAM flush; guards against `SIGKILL` | `0` on desktop |
| Reset the console | `RESET` over the network command interface | n/a |
| Clean quit without signals | `QUIT` over the network command interface | n/a |
| Flush SRAM on demand | `SAVE_FILES` over the network command interface | n/a |
| Keep the child out of settings | `kiosk_mode_enable = "true"` (+ `kiosk_mode_password`) | `false` |
| Isolate state per activity | `savestate_directory`, `savefile_directory` | shared |

Network command interface: `network_cmd_enable = "true"`, `network_cmd_port`
(default **55355**, `config.def.h:1562`). Commands are newline-delimited UDP
text datagrams; the full verb table is `map[]`/`action_map[]` in `command.h:465+`
and includes `QUIT`, `RESET`, `SAVE_STATE`, `LOAD_STATE`, `SAVE_STATE_SLOT <n>`,
`LOAD_STATE_SLOT <n>`, `SAVE_FILES`, `GET_STATUS`, `GET_CONFIG_PARAM <k>`,
`PAUSE_TOGGLE`, `MENU_TOGGLE`, `CLOSE_CONTENT`.

### Three gotchas that will bite whoever implements this

1. **`config_save_on_exit` defaults to `true`** (`config.def.h:638`). On a clean
   exit RetroArch rewrites `retroarch.cfg` from its *current* settings — which
   include everything shepherd injected via `--appendconfig`. Shepherd's
   settings would silently become permanent in the user's own config. The
   fragment must therefore set `config_save_on_exit = "false"` itself.
2. **`pause_nonactive` defaults to `true`** (`config.def.h:1401`). The HUD is a
   layer-shell surface that takes keyboard focus for its popovers
   (`gtk4_layer_shell::KeyboardMode` in `shepherd-hud/src/app.rs`). Expect the
   game to pause whenever the HUD confirm prompt opens unless we set
   `pause_nonactive = "false"`.
3. **The UDP command socket binds `0.0.0.0`**, not loopback
   (`command.c:245` → `socket_init(..., NULL, ...)` with a NULL host = passive
   bind). See "Security" below.

## Design decisions

| Question | Decision |
|---|---|
| Entry kind name | **`retroarch`**, not `emulator`. Every behaviour below is RetroArch-specific (its config keys, its command verbs, its `.auto` state naming). A generic `emulator` kind would be a lie with one arm. A future `dolphin`/`ppsspp` kind can sit beside it. |
| Where the logic lives | `shepherd-host-linux` (adapter + a new `retroarch.rs`), same layer as `browser.rs` — which is the exact precedent: materialize config for a third-party app before spawn, clean up after exit. |
| How settings reach RetroArch | Shepherd writes a per-entry fragment and passes `--appendconfig`. The user's `retroarch.cfg` is never edited. |
| Save-state mechanism | RetroArch's own `savestate_auto_save` / `savestate_auto_load`. Shepherd does **not** manage state files by hand — it only guarantees a clean exit and owns the directory they land in. |
| Clean-exit mechanism | `QUIT` over UDP when the command interface is on; exactly **one** `SIGTERM` otherwise. Never the current multi-signal path. |
| Reset button | **Quit, drop the auto state, relaunch** — gated on a per-session capability flag plumbed to the HUD. Revised: the UDP `RESET` this table first chose needs a control socket that cannot be bound to localhost (see Security). |
| Graceful timeout | Per-kind override, **15 s** (default is 5 s, `traits.rs:52`). A compressed save state plus SRAM flush on slow storage can exceed 5 s, and the penalty for being wrong is a corrupt save. |
| Content path | Require absolute or `~/`-prefixed. `expand_tilde` (`adapter.rs:40`) only handles a leading `~`, and a bare relative path resolves against shepherdd's cwd. The issue's example (`Games/retroarch/...`) would not find the ROM. |

## Config shape

```toml
[[entries]]
id = "pokemon-firered"
label = "Pokemon FireRed"
xwayland_native_resolution = true

[entries.kind]
type = "retroarch"
core = "mgba"                                       # → mgba_libretro.so
content = "~/Games/retroarch/pokemon-firered.gba"
save_state = "auto"                                 # "auto" | "off"
reset_button = true                                 # show the HUD reboot button
```

Optional escape hatches, all defaulted:

```toml
command = "retroarch"        # or an absolute path / a flatpak wrapper
core_path = "/usr/lib/x86_64-linux-gnu/libretro/mgba_libretro.so"  # bypass `core`
args = []                    # extra passthrough args, appended last
env = {}
kiosk = true                 # kiosk_mode_enable, default true
control_port = 55355         # only used when save_state/reset need it
```

`core = "mgba"` resolves to `<libretro_directory>/mgba_libretro.so`; on Ubuntu
26.04 the `libretro-mgba` package (the exact core from the issue) installs to
`/usr/lib/<triplet>/libretro/`. `core_path` skips resolution entirely.

### Fragment shepherd generates

Written to `$XDG_STATE_HOME/shepherd/retroarch/<entry-id>/append.cfg`, passed as
`--appendconfig`:

```cfg
config_save_on_exit = "false"     # or our settings leak into the user's cfg
savestate_auto_save = "true"
savestate_auto_load = "true"
savestate_directory = "<state-dir>/<entry-id>/states"
savefile_directory  = "<state-dir>/<entry-id>/saves"
autosave_interval   = "10"        # SRAM flush every 10s, SIGKILL insurance
pause_nonactive     = "false"     # HUD popovers take focus
video_fullscreen    = "true"
kiosk_mode_enable   = "true"
network_cmd_enable  = "true"      # only when save_state != off or reset_button
network_cmd_port    = "55355"
```

Per-entry `savestate_directory`/`savefile_directory` means two entries pointed
at the same ROM keep separate progress, and backing up a child's saves is one
directory copy.

### Resulting argv

```
retroarch --appendconfig <fragment> -f -L <core-path> <content>
```

## Lifecycle

**Open** — materialize the fragment, spawn, done. `savestate_auto_load` does the
restoring; no shepherd-side sequencing.

**Close** (`stop`, `StopMode::Graceful`):

1. `SAVE_FILES` over UDP (flush SRAM now, cheap, idempotent).
2. `QUIT` over UDP. RetroArch runs its normal shutdown: auto save state + SRAM.
3. Poll for exit up to the 15 s timeout.
4. On timeout, **one** `SIGTERM` to the RetroArch PID only — not
   `kill_by_command`, not the process-group broadcast.
5. On a further timeout, `SIGKILL`. `autosave_interval` caps the loss at ~10 s
   of play.

Every path that ends a session goes through `HostAdapter::stop` — time-limit
expiry, bedtime, the HUD "X", `stop_current` over RPC — so this is one place.
`StopMode::Force` deliberately keeps its current behaviour.

**Reset** — new `reset_current` RPC → engine → new
`HostAdapter::reset(handle)` → `RESET` over UDP. The session, its deadline, and
its usage accounting are untouched; the console reboots to its title screen in
place.

### Alternative reset design (now the recommended one)

> Revised after confirming the command port cannot be bound to localhost — see
> Security below. The UDP `RESET` above costs either a LAN-open control socket
> or a direction-aware ingress filter; this costs neither.


Reset could be done with **no RetroArch IPC at all**: quit cleanly, delete
`<content>.state.auto`, relaunch under the same session. SRAM survives, so the
game boots to the title screen with the child's progress intact — and unlike
UDP `RESET`, it also recovers from a *corrupt* auto state, which is the one
failure mode that otherwise bricks an activity permanently.

Its cost is real but local: the engine has to swap the host handle inside a live
`ActiveSession` while suppressing the `HostEvent::Exited` that the intentional
stop fires — a race with the exit watcher (`adapter.rs:400-455`) that wants
care. That is a contained, testable problem in code this project owns, and it
now compares favourably against standing up an unauthenticated LAN control
socket on a child's kiosk (or writing the ingress filter to contain one).

It is also strictly more capable: `RESET` cannot recover from a *corrupt* auto
state, which otherwise bricks an activity permanently, while
quit-drop-state-relaunch does. Pair it with an admin-side "clear saved state"
action (management RPC / webui) for the same failure mode from the parent's
side.

The visible cost is a few seconds of black screen instead of an instant
in-place reboot, and the session's own accounting must be preserved across the
respawn (same session id, same deadline, no cooldown, no double-count).

## Security: the command interface listens on the LAN

**It cannot be restricted to localhost.** Confirmed on the real 1.22.2 build,
not inferred: with `network_cmd_enable = "true"`, `ss -lunp` shows
`UNCONN 0.0.0.0:55355 users:(("retroarch",…))`, and `GET_STATUS` sent to the
host's LAN address answered
`PLAYING game_boy_advance,gba-tests-arm,crc32=aee3e2bf` — the running game,
to an unauthenticated stranger.

There is no setting for it. The only two config keys are `network_cmd_enable`
and `network_cmd_port` (upstream's own `retroarch.cfg:1016-1018`; nothing
resembling a bind address exists anywhere in it). `command_network_new`
(`command.c:245`) calls
`socket_init((void**)&res, port, NULL, SOCKET_TYPE_DATAGRAM, AF_INET)`, and
`socket_init` (`libretro-common/net/net_socket.c:35`) turns a NULL `server`
into `AI_PASSIVE` — the wildcard. Note the parameter *exists*: the bind address
is simply hardcoded to NULL, so adding `network_cmd_bind_address` upstream would
be a three-line patch. Worth proposing, but not something to depend on.

The UDS command interface is not an escape either: it is `HAVE_LAKKA`-gated, and
`retroarch --features` on the Ubuntu build lists "Network Command … yes" with no
UDS entry.

So anyone on the home network can send `QUIT`, `CLOSE_CONTENT`,
`LOAD_CONTENT <path>`, or `WRITE_CORE_MEMORY` to a child's kiosk.

Shepherd's existing firewall does **not** cover this: it attaches
`cgroup_skb` to `BPF_CGROUP_INET_EGRESS` only, by explicit choice
(`crates/shepherd-firewall-helper/src/bpf.rs:224-229`, whose comment already
notes systemd's `IPAddressDeny=` covers both directions and that egress alone
was judged sufficient).

Options, in order of preference:

1. **Don't open the socket.** Phases 1–2 need no port at all — one `SIGTERM`
   drives the whole save/restore cycle, verified against real RetroArch. The
   exposure exists only to serve the reset button, which has a socket-free
   implementation (quit, drop the auto state, relaunch — see below). Given the
   cost of the alternatives, this is now the recommended default rather than
   the fallback.
2. **Ingress firewall**, if the UDP interface is wanted anyway. *Correction to
   an earlier draft of this doc*, which called this "attach the same BPF program
   to `BPF_CGROUP_INET_INGRESS`, small and principled": the program matches on
   `dst_addr` only (`shepherd-firewall-bpf/src/main.rs:76-84`). On ingress the
   peer is the *source* address, so the same program would test our own IP
   against the rules and let every attacker through. It needs a direction-aware
   address selection (a flag in a map, or a second program). Still modest, but
   not free, and easy to get silently wrong — an ingress filter that looks
   attached and blocks nothing is worse than none.
3. Accept and document. Defensible on a home LAN; should still be a conscious
   decision, not a side effect.

## Work breakdown

**Phase 1 — clean shutdown (required before any of this is safe).**
Make graceful stop of a non-sandboxed process send exactly one `SIGTERM` to the
session leader, escalating only on timeout. Touches
`crates/shepherd-host-linux/src/adapter.rs:839-880` and
`crates/shepherd-host-linux/src/process.rs:842-873`. Standalone value; probably
its own issue and its own PR. *Without this, Phases 2-3 do not work.*

**Phase 2 — the `retroarch` entry kind.**

- `crates/shepherd-api/src/types.rs` — `EntryKindTag::Retroarch` (`:14`),
  `EntryKind::Retroarch { .. }` (`:99`).
- `crates/shepherd-config/src/schema.rs` — `RawEntryKind::Retroarch`.
- `crates/shepherd-config/src/validation.rs:223-271` — core/content presence,
  reject bare relative paths, warn on a missing core file.
- `crates/shepherd-config/src/policy.rs:835-861` — raw → api conversion.
- `crates/shepherd-config/src/icon.rs:11-36` — icon fallback (`retroarch`).
- `crates/shepherd-host-api/src/capabilities.rs:58-77` — add to `linux_full`.
- `crates/shepherd-host-linux/src/retroarch.rs` (new) — fragment rendering,
  core resolution, argv, UDP client.
- `crates/shepherd-host-linux/src/adapter.rs:478-560` — spawn arm; per-session
  control info alongside `steam_sessions`/`session_info`; stop arm per the
  sequence above.
- `crates/shepherd-config/src/bin/validate-config.rs:57-77` — summary line.
- `shepherd-webui/src/api/types.ts:32-40` — `EntryKindTag` union (hand-written).
- `companion-android/.../WireTypes.generated.kt` — regenerate via
  `shepherd-wire-codegen` if the tag reaches the wire schema.

**Phase 3 — the HUD reboot button.** Mirrors `confirm_on_close` (issue #78)
end to end, which is the cheapest available template:

- `shepherd-api/src/types.rs:524` — `SessionInfo::can_reset`, plus the
  `SessionStarted` payload (`events.rs:39`).
- `shepherd-core/src/engine.rs:1040,1073` and `shepherd-core/src/events.rs:20` —
  carry it from the entry.
- `shepherd-management/src/service.rs:41+` — new `reset_current` RPC on the
  `#[management_rpc]` trait; regenerate `docs/rpc-schema.json`.
- `shepherd-ipc/src/client.rs:110` — client helper beside `stop_current`.
- `shepherd-host-api/src/traits.rs` — `reset()` defaulting to
  `Err(HostError::UnsupportedKind)`; `mock.rs` follows.
- `shepherd-hud/src/state.rs:86-100` — `can_reset()` beside `confirm_on_close()`.
- `shepherd-hud/src/app.rs:520-579` — a button next to the "X", reusing the
  existing confirm-popover machinery (resetting drops up to `autosave_interval`
  seconds of play, so it should confirm). Must be rebuilt on scale change —
  see the #128 fix, this is the same trap.

**Phase 4 — docs and the example, i.e. what the issue originally asked for.**

- `config.example.toml` — the Pokémon FireRed entry, with an absolute content
  path and a comment pointing at the docs.
- `docs/emulators.md` (new, alongside `docs/shepherd-media.md`) — installing
  `retroarch` + `libretro-mgba`, where cores live, where saves live, the
  save-state model, the reboot button, kiosk mode, and an explicit note that
  ROMs are the operator's problem and nothing is bundled.
- Validate: `cargo run -p shepherd-config --bin validate-config -- config.example.toml`.

Phases 1, 2+4, and 3 are independently shippable in that order. Phase 1 alone
already makes the issue's original doc-only example behave correctly for SRAM
saves; Phase 2 adds true resume-where-you-left-off.

## Testing

- **Unit** — fragment rendering (including `config_save_on_exit = "false"`),
  core-name → `.so` resolution, argv construction, tilde expansion of `content`,
  UDP datagram encoding.
- **Stop-path integration** — a fake `retroarch` shell script that listens on
  the UDP port, records which signals it received, and writes a marker file on
  clean quit. Asserts: exactly one `SIGTERM` ever reaches it; `QUIT` alone
  suffices; the marker exists after every graceful stop. This is the test that
  would have caught the headline bug, and it needs no emulator.
- **End-to-end** — the `headless-dev` skill (`dev headless` → `dev shot` →
  `dev stop`) with `retroarch` + `libretro-mgba` installed on the dev box.
  Use freely-redistributable homebrew as test content (e.g. a CC-licensed GB/GBA
  homebrew ROM); do not commit ROMs. Verify: launch → play → close → relaunch
  resumes; reboot button returns to the title screen; SRAM survives both.
- **Screenshot** the HUD with the reboot button at a non-1x scale — this repo
  has a history of HUD widgets breaking exactly there (#114, #128).

## As built (Phases 1 and 2)

Built as scoped, with these divergences worth recording.

**The graceful path signals the process *group*, not the leader PID.** Scoping
said "one SIGTERM to the session leader", which would have orphaned the real app
whenever an entry's `command` is a wrapper script. `setsid()` at spawn makes the
child a group leader, so one group-directed signal reaches it and every
descendant that stayed in the group — still exactly one signal per process. The
decision now lives in a `GracefulSignal` enum in `adapter.rs` rather than an
if-chain, so "a plain process is signalled once, via its process group, and by
nothing else" is a unit-testable statement.

**The double-SIGTERM cannot be reproduced with a shell stand-in.** Verified
empirically: two SIGTERMs 3 ms apart fire a `dash` trap exactly *once*, because
its handler sets a flag and defers, and the second delivery folds into the
first. RetroArch's C handler increments on every delivery, which is why it
breaks and a script cannot be made to. So the behavioural test asserts the
property that actually matters — the activity completes its shutdown and exits
on its own — using a stand-in that copies RetroArch's semantics (first signal
starts a slow save and resets the disposition; a second is fatal). The
"only one mechanism signals a plain process" invariant is pinned separately by
the `GracefulSignal` unit test. Neither test alone would have caught the
original bug; together they cover it.

**`SpawnOptions` gained `entry_id`.** The scope left the state-directory key
open. Keying by entry id needed the id at the host layer, which only the browser
path had (via `BrowserSpec::policy_id`). One optional field on `SpawnOptions`,
set at the single production launch site, was cleaner than deriving a key from
the content path. Callers without one fall back to the content's file stem.

**Per-core state sorting dropped.** `sort_savestates_enable` and friends were in
the scope to stop states written by one core being loaded by another. An entry
carries exactly one core and the directory is already per-entry, so the keys
would never do anything. Fewer generated settings to get wrong.

**`root_dir()` absolutizes.** Caught by the end-to-end run, not by scoping: the
dev harness sets `SHEPHERD_DATA_DIR=./dev-runtime/data`, and a relative
`savefile_directory` / `--appendconfig` is resolved by *RetroArch* against its
own working directory, not the daemon's. Saves would have scattered. Same class
of bug as the relative `content` path the validation rejects.

**Installation.** `shepherd-admin apps install retroarch [--ppa[=channel]] [core...]`
was added beside the existing `steam` / `chrome` backends. Cores are named the way an
entry's `core =` field names them (`mgba`, not `libretro-mgba`), so there is one
spelling to learn, and only the distro's packaged cores are offered —
RetroArch's built-in core downloader fetches unsigned binaries at runtime, which
a supervised kiosk should not be doing behind the operator's back. Names are
validated before `require_root`, so a typo doesn't cost a sudo prompt.

**Phase 4** added the `pokemon-firered` entry to `config.example.toml` (the
issue's own example, with the relative content path corrected) and
[`docs/emulators.md`](../../emulators.md), linked from the README and
`INSTALL.md`. The documented entry shape was then run end-to-end: the on-disk
layout it describes is the layout the daemon actually produced, and the two
RetroArch log excerpts it quotes for troubleshooting were taken from real
failures, not written from memory.

All four phases are built.

## As built (Phase 3 — the reset button)

Built with the socket-free design, so nothing in this feature opens a listening
port. `reset_current` stops the activity gracefully (it saves what it owns),
deletes the `*.state.auto` files, and respawns under the same session id, the
same deadline, and the same spawn options — the firewall and browser policy are
resolved through one shared `resolve_spawn`, so a restarted activity cannot
come back under weaker rules than it launched with.

**The exit-event race was real, and worse than scoped.** Scoping predicted the
engine would need to ignore the exit its own teardown causes, and that flag
(`CoreEngine::restarting`) was the first thing built. It was not enough: the
host reports exits through a channel the reset does not wait on, so the *old*
process's exit routinely arrived **after** `finish_restart` had already cleared
the flag — the reset visibly worked and then the session ended a moment later.
Caught end-to-end, not by a test.

The fix is not a wider flag but an identity check: `notify_process_exited`
compares the exiting handle's pid against the one currently backing the session
and ignores anything else. That closes the race by construction rather than by
timing, and it also fixes a pre-existing hazard nobody had hit yet — a
previous activity's late exit could end a freshly launched session, since
`notify_session_exited` never looked at *which* process had died. The flag is
still needed for the window where the session's handle is still the outgoing
process; the two guards compose.

**HUD.** A reset button appears beside the "X" only for activities that offer
it, with its own confirmation prompt sharing the `build_confirm_prompt` builder
(now parameterized by a `ConfirmAction`). The existing debug hook was extended
— `<trigger>.reset` pops the new prompt — so the popover can be screenshot from
a shell like the close prompt already could.

### Verification

Unit and integration tests cover fragment rendering, core-name normalization,
path keying, argv construction, config parsing, and validation.

The whole chain was then run under `dev headless` against **real RetroArch
1.22.2 with the real mGBA core**, both installed through the new admin method,
playing [jsmolka/gba-tests][gba-tests] `arm.gba` — an MIT-licensed test ROM
whose source is in the same repo. It renders its result to the screen, so it
proves the emulator actually ran rather than merely started.

- **It runs.** `dev tree` showed `com.libretro.RetroArch` focused and the
  launcher hidden; the screenshot showed the ROM's own output fullscreen with
  shepherd's HUD (activity name, timer, clock, volume, close) over it.
- **Core resolution works.** `core = "mgba"` became
  `/usr/lib/x86_64-linux-gnu/libretro/mgba_libretro.so` — the resolved absolute
  path, not the bare-name fallback — and the log confirmed
  `Appending config: ".../gba-tests/append.cfg"`.
- **Close saves.** `stop_current` returned in **307 ms**, and the log recorded
  `[State] Auto save state to "…gba-tests-arm.state.auto" succeeded`,
  `[SRAM] Saving RAM type #0 to "…gba-tests-arm.srm"`, then
  `[Core] Unloading game… Unloading core…` — a full clean shutdown, not a hard
  exit, and nowhere near the 15 s deadline.
- **Open restores.** The next launch logged `[State] Found auto save state…`,
  `Auto-loading save state… succeeded`, and `Loading state…, 528472 bytes`.
- **The user's config is untouched.** `~/.config/retroarch/retroarch.cfg` still
  reads `config_save_on_exit = "true"` and contains none of the injected keys —
  the fragment's own `config_save_on_exit = "false"` suppressed the writeback
  for the run without changing the operator's setting. This was the scoping
  gotcha most likely to go unnoticed, and it holds.

Two things observed on real hardware that scoping did not predict, neither
needing a change:

- RetroArch puts saves and states in a **per-core subdirectory** of the ones we
  hand it (`…/gba-tests/states/mGBA/…`) on its own. The `sort_savestates_*`
  keys the scope dropped would have been redundant, which is now confirmed
  rather than assumed.
- RetroArch still keeps its **playlists, history, and runtime logs** in
  `~/.config/retroarch/`. Shepherd isolates saves and states, not everything.
  Harmless, but worth a line in the Phase 4 docs.

### Verification (Phase 3)

Same stack, real RetroArch, same MIT test ROM. Launched, closed (state
written), relaunched (state resumed), then reset over RPC:

- `reset_current` returned in **205 ms**;
- the RetroArch pid changed (670939 → 671444) — the process really was replaced;
- `current_session` reported the **same session id and the same deadline**
  afterwards: the reset did not restart the clock, spend a cooldown, or open a
  new session;
- the `*.state.auto` files were gone, and the post-reset RetroArch log contains
  **zero** "Found auto save state" lines — it booted from the ROM's start;
- the 128 KB `.srm` in-game save survived, and `dev tree` showed RetroArch
  focused again;
- closing normally afterwards wrote a fresh auto state, so the activity is back
  to its usual save/restore behaviour.

The HUD was screenshot at scale 1.0 and again under the
`xwayland_native_resolution` counter-scale at 1.5: the button appears only for
resettable activities, and its prompt renders correctly sized and unclipped at
both — the failure mode that needed fixing three times before (#114, #118,
#128).

[gba-tests]: https://github.com/jsmolka/gba-tests

## Core naming: a bug only the second core found

The installer and the entry kind were built and shipped having only ever been
exercised with **one** core, `mgba` — whose apt package (`libretro-mgba`) and
shared object (`mgba_libretro.so`) happen to agree. `resolve_core` computed the
filename as `format!("{name}_libretro.so")` on that evidence.

Installing a second core showed the assumption was wrong for **9 of the 14**
packaged cores, and for the PPA core that prompted the check:

| configured / package name | actual shared object |
|---|---|
| `genesisplusgx` | `genesis_plus_gx_libretro.so` |
| `bsnes-mercury-*` | `bsnes_mercury_*_libretro.so` |
| `mupen64plus-next` | `mupen64plus_next_libretro.so` |
| `beetle-pce-fast` | `mednafen_pce_fast_libretro.so` |
| `beetle-psx` | `mednafen_psx_hw_libretro.so` |
| `beetle-vb`, `beetle-wswan` | `mednafen_vb`, `mednafen_wswan` |

Every one of those would have failed to resolve, fallen back to the bare
computed name, and died in RetroArch with `--libretro argument … is not a file`
— including two entries in the core table `docs/emulators.md` published as
working configuration.

`resolve_core` now matches against the files actually on disk rather than a
computed name, comparing with separators stripped (so `-`/`_` and run-together
words all land), plus a four-entry alias table for the Beetle/Mednafen family,
which differ by more than punctuation. `SHEPHERD_LIBRETRO_DIR` overrides the
search path, which is also what lets the test cover all of these naming shapes
against a fixture directory. Verified live afterwards for both an archive core
(`mgba`) and a PPA core (`mupen64plus-next`).

The lesson worth keeping: a lookup validated against a single example is not
validated. The one core that worked was the one where the two naming schemes
coincided.

## Open questions

1. **Flatpak RetroArch** (`org.libretro.RetroArch`) as well as the apt build?
   It needs the fragment path visible inside the sandbox and its config tree
   lives under `~/.var/app/...`. Recommend apt-only for the first cut and a
   follow-up for flatpak, mirroring how `browser.rs` supports exactly one
   flatpak today.
2. ~~**Ingress firewall**, or accept the LAN exposure?~~ **Resolved:** neither —
   the socket cannot be bound to localhost, so Phase 3 does the reset without
   one. Revisit only if something else needs the UDP interface (an in-place
   pause on bedtime, say), and then do the direction-aware ingress filter
   properly.
3. **Reset semantics** — is "reboot the console, keep the save file" the right
   behaviour, or does the child also want "start this game over"? The latter is
   an admin action (clear saved state), not a HUD button.
4. **State backups.** Cheap insurance: rotate two copies of `*.state.auto` after
   each clean exit. Worth doing, or is `autosave_interval` on the SRAM enough?
5. Should `kiosk = true` be the default? It locks the child out of RetroArch's
   own menu — which is the point in a kiosk — but also out of per-core options
   an operator may want to set once. A `kiosk_mode_password` is the escape.
