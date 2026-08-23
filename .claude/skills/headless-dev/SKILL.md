---
name: headless-dev
description: >-
  Run and visually verify shepherd-launcher end-to-end during development without
  a graphical login session. Use whenever you need to launch/start/run the app,
  take a screenshot, or confirm a UI change (launcher grid, HUD, media, browser,
  availability/bedtime, time limits) actually works in the real stack — not just
  in unit tests. Boots the real sway + shepherdd + launcher + HUD headless (no
  GPU, no parent compositor), so it works over SSH / in an agent sandbox. This is
  the project skill that /run and /verify should use for this repo instead of
  falling back to `./run-dev` (which needs a login session).
---

# Headless end-to-end development for shepherd-launcher

`shepherd-launcher` is a Sway-based kiosk. The normal `./run-dev` boots a
**nested** compositor (`WLR_BACKENDS=wayland`) that needs a graphical login
session — unusable headlessly. Instead, use `shepherd dev headless`, which boots
the **same** stack (same `sway.conf`, `config.example.toml`, debug binaries)
against the headless wlroots backend the e2e harness uses. It needs no login
session, no parent compositor, and no GPU, and it exposes a virtual output you
can screenshot and drive.

## One-time setup

```sh
./scripts/shepherd deps install agent    # grim + wtype + jq (also folded into `deps install dev`)
```

## The loop

```sh
./scripts/shepherd dev headless          # build + boot, detached; prints when ready
./scripts/shepherd dev tree              # assert app_id "org.shepherd.launcher" is up/focused
./scripts/shepherd dev shot home.png     # screenshot -> Read home.png to SEE the UI
# ...edit code...
./scripts/shepherd dev headless --no-build   # respawn is cheap; or rebuild without --no-build
./scripts/shepherd dev stop              # tear down when done
```

`dev headless` runs **detached** and writes connection details to
`dev-runtime/headless/session.env`, so every later `dev` subcommand reattaches
automatically. Always `dev stop` when finished (or before starting a fresh one).

## Commands

| Command | Purpose |
| --- | --- |
| `dev headless [opts]` | Build + boot a detached headless session |
| `dev shot [out.png]` | Screenshot the virtual output (default: timestamped under `dev-runtime/headless/`). Prints the path — then **Read** the PNG. |
| `dev tree [raw]` | Window-tree summary via `jq` (app_id / focused / fullscreen / visible). Cheaper and less flaky than pixel-diffing for structural checks. |
| `dev key <keysym>...` | Inject keystrokes, e.g. `Down Down Return`, `Escape` |
| `dev type <text>` | Type literal text into the focused surface |
| `dev click <x> <y> [btn]` | Move + click the virtual pointer |
| `dev stop` | Tear the session down and clean up |

### `dev headless` options

- `--config PATH` — boot an arbitrary shepherdd config instead of
  `./config.example.toml`. Good for minimal fixtures that isolate one entry/flow.
- `--time "YYYY-MM-DD HH:MM:SS"` — sets `SHEPHERD_MOCK_TIME` so availability
  windows, the bedtime screen, HUD clock, and time-limit behavior are
  reproducible. **Use this** whenever the thing under test depends on the clock.
- `--user NAME` — run the whole stack as another user (via sudo) for a realistic
  session: their groups, `HOME`, and default `~/.config/shepherd/config.toml`.
  The repo must be readable/executable by NAME (a checkout under a `0700 $HOME`
  is not — grant traversal, e.g. `setfacl -m u:NAME:x $HOME`). Reattach commands
  detect the owner and route through sudo automatically; `dev shot` still writes
  a PNG the invoker can read.
- `--size WxH` (default 1280x720), `--gpu` (GL renderer vs. pixman),
  `--no-build` (skip the cargo build).

## Verifying a change

Prefer observing behavior over trusting the build:

1. **Structural** — `dev tree` to assert the right surface is mapped/focused
   (e.g. after launching an activity, its window — not the launcher — is focused;
   after stopping, the launcher is back).
2. **Visual** — `dev shot out.png`, then Read the PNG. Confirm the actual pixels
   (layout, text, the specific widget you changed).
3. **Drive it** — use `dev key` / `dev click` to reach the state you changed
   (navigate the grid, open a modal), screenshot, assert.

Example (bedtime restriction):

```sh
./scripts/shepherd dev headless --time "2025-12-25 21:00:00"
./scripts/shepherd dev shot bedtime.png   # Read it: only the after-hours entries show
./scripts/shepherd dev stop
```

## Gotchas (read before trusting a screenshot)

- **`dev click` doesn't reliably activate GTK widgets.** The synthetic pointer
  (`swaymsg seat seat0 cursor set/press/release`) does not fire GTK4
  `connect_clicked` handlers here — a launcher grid tile won't launch and a HUD
  button won't respond, at either logical or physical coordinates. Drive the app
  a different way:
  - **Launch/stop an activity** (and anything else shepherdd exposes): send
    newline-delimited JSON-RPC to the daemon socket at
    `./dev-runtime/shepherd.sock`, e.g.
    `printf '{"request_id":1,"api_version":1,"method":"launch","params":{"id":"<entry-id>"}}\n' | nc -U dev-runtime/shepherd.sock`.
    This runs the real launch path (incl. the HiDPI scale hack for
    `xwayland_native_resolution` entries). Method names/params are in
    `crates/shepherd-ipc/src/client.rs`.
  - **The HUD's "End session" confirm popover** has a permanent debug-build hook:
    export `SHEPHERD_HUD_DEBUG_CONFIRM_TRIGGER=<path>` before `dev headless`
    (env propagates from the invocation into the sway-spawned HUD), then
    `: > <path>` pops the prompt and `: > <path>.down` dismisses it. Both files
    are consumed, so open/close cycles are just two `touch`es.
  - **Any other HUD-only UI action**: add a temporary one-shot debug hook gated
    behind an env var that calls `widget.emit_clicked()`, boot with the env var
    set, screenshot, then remove the hook. `dev key` (keyboard) *does* reach the
    focused surface, but the always-on HUD bar uses `KeyboardMode::None`, so keys
    won't reach it unless a popover raises it to `OnDemand`.
- **`dev key` needs a longer-lived keyboard for egui/winit apps.** `shepherd-media`
  is an eframe (winit) client, and a single `dev key <keysym>` lands nowhere: the
  headless seat has *no* input devices (`swaymsg -t get_seats` shows an empty
  `devices` list), so winit never binds `wl_keyboard`. `wtype` creates a virtual
  keyboard, sends its key and exits immediately — the client loses the race
  between the capability appearing and the key arriving. Keep the device alive
  across several presses instead, and the later ones land:
  `wtype -s 700 -k Return -k Return` (env: the session's `WAYLAND_DISPLAY` /
  `XDG_RUNTIME_DIR` / `SWAYSOCK`, see `dev-runtime/headless/session.env`). GTK
  clients rebind on the capability change, which is why `dev key` works there.
  Synthetic pointer clicks never reached the egui surface under any timing.
- **Settle after "ready".** `dev headless` returns once the launcher *surface*
  maps, but async icon/tile loading can lag a beat (a tile may still say
  "Loading…"). For the fully-painted UI, poll `dev tree` for the specific entry,
  or take a second `dev shot` a moment later.
- **Assert the state you meant to capture, in the pixels.** A `launch` that
  races shepherdd's startup — or hits a daemon that is still running the previous
  activity — is rejected, and the "No session" bar screenshots just as happily
  (with the volume slider in its *disabled* styling, which measures differently
  from the live one). Before keeping a shot, poll for something only the target
  state paints, e.g. the warning banner's background colour.
- **Output-scale changes settle asynchronously.** Useful when testing the
  `xwayland_native_resolution` HiDPI hack, which needs a non-1x scale
  (`swaymsg output HEADLESS-1 scale 1.5`, via `headless_run` in
  `scripts/lib/headless.sh` — there is no `dev swaymsg` passthrough). `stop_current`
  returns *before* shepherdd restores the pre-launch scale, so a scale you set
  immediately afterwards gets clobbered a second later. Wait, then re-read
  `swaymsg -t get_outputs`, and check it again at screenshot time.
- **Black screenshot?** `swayidle` blanks the output (DPMS off) after ~120s idle
  with no session; `dev shot` already forces `dpms on` first, but if you script
  raw `grim`, do the same.
- **GTK renderer.** The session sets `GSK_RENDERER=cairo` (GTK4's GL renderer
  needs EGL, absent on the pixman/no-GPU path). If the UI doesn't paint, check
  `dev-runtime/headless/sway.log` and try `--gpu`.
- **Config edits.** `config.example.toml` must pass `./scripts/shepherd config
  validate` (CI checks this). Test config-driven changes with `--config` against
  a fixture first.
- **Never give a fixture entry `command = "sleep"`.** Stopping a session calls
  `kill_by_command(<command name>)`, which kills *every* process of that name the
  user owns — including the `sleep`s in your own driver script, which then dies
  mid-scenario (exit 144). Point the fixture at a small wrapper script
  (`exec tail -f /dev/null`) so the name is unique to the fixture.
- **A stale `dev-runtime/headless/session.env` breaks the next boot.** The start
  path sources it, so a leftover `SWAYSOCK` from a dead session is inherited by
  the new sway, which uses *that* socket path while the script waits on the one
  it derived from the new pid: "Headless Sway did not answer IPC within 10s"
  while a perfectly healthy stack is running. `dev stop` first, or delete the
  file (and `unset SWAYSOCK`) before `dev headless`.
  **`dev stop` will not save you here**: it reads the stale pid, says "No live
  headless session to stop", and exits — so the orphaned stack keeps running and
  the *next* boot fails the same way, this time as a bare
  "[ERROR] Failed to start headless session" with a fully working daemon behind
  it. If `dev stop` denies there is a session while one is actually running, kill
  the survivors directly, `rm dev-runtime/headless/session.env`, and boot again.
  Also note that killing shepherdd runs `kill_by_command` on the way down, which
  kills every `sleep` you own — a driver command containing one dies with exit
  144 alongside it, and that is one of the ways you end up orphaned here in the
  first place.
- **Do not look for survivors with `pgrep -f`.** `-f` matches whole command
  lines, so `pgrep -f sway.headless.conf` matches *the shell running the pgrep*,
  which contains that string as an argument. It therefore always "finds" a
  session, and reports a different pid every time — which reads exactly like a
  stack respawning itself in a loop. Use a listing you can eyeball instead:

  ```sh
  ps -eo pid,cmd | grep -E "sway.*headless|shepherdd|shepherd-media" | grep -v grep
  ```

  Also check for more than sway: `dev stop` can report "Headless session
  stopped" and leave the `shepherdd` it spawned (and its two `sh -c` wrappers)
  running against your fixture config. Those keep the socket alive, so the next
  boot's launch requests land in the *old* daemon. Kill every pid that listing
  shows before booting again.
- **`--no-build` against a cleaned `target/debug`** boots a session whose
  `shepherdd` binary is missing; sway's `|| swaymsg exit` then tears the whole
  session down a second later. Build once before using `--no-build`.

## Under the hood

Connection state is in `dev-runtime/headless/session.env`; the compositor log is
`dev-runtime/headless/sway.log`. Design notes and rationale:
`docs/ai/history/2026-07-14 001 headless-agent-dev-scaffold.md`. Human-facing
usage: the "Headless development" section of `CONTRIBUTING.md`.
