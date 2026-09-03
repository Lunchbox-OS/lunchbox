# Steam's cgroup isolation, measured (#144, #158)

Review notes from a manual read of #158 (`feat/ipc-peer-cgroup`), plus the
measurements that settled it and the change they led to. The design and the
hardware verification are in `2026-08-29 001` and `002`; this covers only the
Steam corner, which those two got half right.

## The prompt

> #158 is checked out and I'm manually reviewing it. What are all of the cgroups
> in play now as of this implementation
>
> — then: check whether the steam scope survives the reparent; what else
> bypasses `spawn()`; what would the side effects be of spawning steam via a
> cgroup (knowing that snap will move it again later); wrap `preload_steam` too.

## The inventory

Six cgroups are in play once #158 lands.

| # | Cgroup | Who is in it |
|---|--------|--------------|
| 1 | shepherdd's own — `/user.slice/user-<uid>.slice/session-<n>.scope` on a device, the launching shell's cgroup in dev | sway, shepherdd, the launcher, the HUD, swayidle, sway's keybinding one-shots. **This is the allow-list.** |
| 2 | `shepherd-<session-id>.scope` in the **user** manager | every activity that is not a firewalled Process, snap or flatpak |
| 3 | `shepherd-<session-id>.scope` in the **system** manager | firewalled Process entries, via the pkexec helper |
| 4 | `snap.<name>.<name>-<uuid>.scope`, `app-flatpak-<id>-*.scope` under `user@<uid>.service/app.slice` | snap and flatpak apps, scoped by their own runtimes |
| 5 | `shepherd-isolation-probe-<pid>.scope` | the `activity_isolation_status()` probe, for milliseconds |
| 6 | anywhere | root, which bypasses the cgroup check entirely so `sudo` still works |

Decisions compare cgroup **ids** (`PIDFD_GET_INFO`'s `cgroupid`, an inode
number). Paths are read only for the delegated-subtree test at startup and for
log and diagnostic text.

## Steam does not land where the code implies

Measured on a workstation with the steam snap installed, running the exact argv
the adapter builds:

```
$ systemd-run --user --scope --collect --quiet --unit=shepherd-<uuid>.scope \
      -- snap run --shell steam
0::/…/app.slice/snap.steam.steam-dd2c86be-….scope     ← not the shepherd scope

$ systemctl --user is-active shepherd-<uuid>.scope
inactive                                              ← empties, --collect reaps it
```

`snap run` asks the user manager for its own scope and moves itself there. The
unwrapped control lands in the same place, so the re-scope is snapd's doing.

Two further facts, both measured:

- `systemd-run --user --scope` **does** exec in place — recorded pid `216584` ==
  inner pid `216584`. The README's claim holds.
- `snap run` does **not** — recorded `216587`, inner `216660`, with or without
  the wrapper. This is why Steam sessions are tracked by `find_steam_game_pids`
  rather than by pid, and it is not something the wrapper made worse.

And the game is not in that process tree at all: `snap run steam
steam://rungameid/<id>` is a short-lived request to the **preloaded** client
(`preload_steam`), and the game is a child of that client.

So `002`'s summary — "Steam is preloaded as a snap and its games inherit that" —
was right about where Steam ends up and wrong about the code: the isolation
branch skips only `sandboxed_app_name.is_some()`, and `EntryKind::Steam` returns
`snap_name = None`, so Steam was being wrapped anyway. The firewall condition
one screen earlier excludes Steam explicitly; the isolation condition did not.
That asymmetry was the whole finding.

## Side effects of keeping the wrapper, measured

Nearly all benign:

- Scope goes `inactive` within ~1s; `systemctl --user list-units 'shepherd-*'`
  stays empty across runs.
- No dangling teardown: the user-scope name is computed inline and never stored
  in `SessionInfo.firewall_scope` (only the helper path sets that), so nothing
  ever tries to stop a unit that is gone.
- No kill risk. A scope stops when its cgroup is **empty**, so there is nothing
  left to kill. A straggler would keep the scope alive, not get killed by it.
- Env passes through exactly (`WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, custom vars);
  exit status passes through (`exit 42` → `42`).

Costs: one D-Bus round trip to the user manager per launch (negligible against
the 30s launch watchdog), one Started/Stopped pair in the journal, and one new
dependency — a launch now needs the user manager reachable *at launch time*,
while `activity_isolation_status()` is cached from startup.

## The decision

Keep both wrappers, and add the missing one.

Wrapping the `rungameid` request but not the client was the worst of the three
options: it paid the cost and created the impression that Steam games land in
`shepherd-<session>.scope`, without covering the launch a game actually inherits
its cgroup from. So `preload_steam` now wraps too, as
`shepherd-steam-preload-<pid>.scope`.

Neither wrapper changes where Steam ends up — snapd still owns the final scope.
What they buy is that *"nothing shepherd starts for an activity is ever in
shepherd's cgroup"* holds because of what `adapter.rs` does, rather than because
snapd happens to move the process fast enough. The window either wrapper closes
is short and not obviously reachable; the point is that its width was being set
by a third party.

One incidental fix rode along: `preload_steam` passed no `kill_name`, so
`ManagedProcess.command_name` fell back to `argv[0]` — `"snap"`. The `pkill -f`
fallback on that name would have reached every snap on the device. It is now
`"steam"`, matching what the Steam session path already uses.

## What else is inside the allow-list

`spawn()` is not the only thing that starts processes, and everything below runs
as a direct child of shepherdd — inside cgroup #1, and therefore inside the
allow-list. None of it is new in #158; what #158 changed is that being in
shepherdd's cgroup became an authorization decision.

- **Input-compat sidecars** — touch bridge, tablet bridge, gamepad bridge,
  disable-touch grab (`sidecar.rs`, spawned from `adapter.rs`). Per-activity
  lifetime, unwrapped.
- **`wl-mirror`** (`shepherdd/src/display.rs`) — third-party, long-lived while
  mirroring.
- **`shepherd-pairing`** (`shepherdd/src/pairing_display.rs`) — shepherd's own.
- **`yt-dlp`** (`shepherd-media-cache`, driven by `shepherdd/src/media.rs`) — the
  one worth a decision. It runs on a background prefetch timer with no activity
  launched, and it parses remote content from an extractor with a recurring
  history of parser CVEs. The URLs come from admin-configured media sources, so
  an activity cannot choose the target; the untrusted part is the response.
- Short-lived query subprocesses with fixed argv: `wpctl`/`pactl`/`amixer`,
  `pw-dump`, `brightnessctl`, `pgrep`, `pkcheck`, `flatpak --version`.

The `pkexec` calls to the firewall helper are correctly outside — they run as
root in the system manager.

### Resolved: both

`yt-dlp` is now scoped out of shepherdd's cgroup, and the docs list the rest.

The wrapper could not be built in `shepherd-media-cache`: that crate is shared
with the player and the Android build, neither of which has a systemd user
manager, and the probe for whether scoping works lives in
`shepherd-host-linux`. Making it depend on the Linux host adapter to run a
subprocess would have been the wrong inversion. So `shepherdd` — which already
depends on both — **injects** one at startup via `set_scope_prefix_fn`, and
`shepherd-host-linux::helper_scope_argv_prefix` supplies it. Anything that never
calls the setter runs `yt-dlp` bare, unchanged: every test, the player, Android.

Scoped: the download and the playlist fetch. Not scoped: `yt-dlp --version`,
which parses no remote input and runs on every playlist fetch and diagnostics
pass, where a systemd round trip per call would buy nothing. A compromised
yt-dlp *binary* is a different problem, and not one a cgroup helps with.

Measured: `yt-dlp --version` inside the composed argv lands in
`…/app.slice/shepherd-ytdlp-download-<pid>-<n>.scope` and returns `2026.06.09`;
five sequential runs leave no units behind (`systemctl --user list-units
'shepherd-*'` empty). The per-call counter in `helper_scope_argv_prefix` is what
keeps concurrent invocations from colliding on a unit name — a collision fails
the launch outright rather than degrading.

The sidecars, `wl-mirror` and the pairing overlay stay in shepherdd's cgroup on
purpose: shepherd's own furniture, no remote input, and two of them need the
session's own input devices. They are now named in `shepherd-ipc`'s README and
`INSTALL.md` instead of being silently omitted from the allow-list description.
