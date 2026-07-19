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
  - **A HUD-only UI action** (e.g. opening the "End session" confirm popover):
    add a temporary one-shot debug hook gated behind an env var that calls
    `widget.emit_clicked()`, boot with the env var set (it propagates from the
    `dev headless` invocation into the sway-spawned HUD), screenshot, then remove
    the hook. `dev key` (keyboard) *does* reach the focused surface, but the
    always-on HUD bar uses `KeyboardMode::None`, so keys won't reach it unless a
    popover raises it to `OnDemand`.
- **Settle after "ready".** `dev headless` returns once the launcher *surface*
  maps, but async icon/tile loading can lag a beat (a tile may still say
  "Loading…"). For the fully-painted UI, poll `dev tree` for the specific entry,
  or take a second `dev shot` a moment later.
- **Black screenshot?** `swayidle` blanks the output (DPMS off) after ~120s idle
  with no session; `dev shot` already forces `dpms on` first, but if you script
  raw `grim`, do the same.
- **GTK renderer.** The session sets `GSK_RENDERER=cairo` (GTK4's GL renderer
  needs EGL, absent on the pixman/no-GPU path). If the UI doesn't paint, check
  `dev-runtime/headless/sway.log` and try `--gpu`.
- **Config edits.** `config.example.toml` must pass `./scripts/shepherd config
  validate` (CI checks this). Test config-driven changes with `--config` against
  a fixture first.

## Under the hood

Connection state is in `dev-runtime/headless/session.env`; the compositor log is
`dev-runtime/headless/sway.log`. Design notes and rationale:
`docs/ai/history/2026-07-14 001 headless-agent-dev-scaffold.md`. Human-facing
usage: the "Headless development" section of `CONTRIBUTING.md`.
