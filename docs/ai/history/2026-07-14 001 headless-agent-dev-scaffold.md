# Headless, agent-drivable development scaffold

## Prompt

> Help me design a scaffold for end-to-end development of this project. The
> biggest roadblock so far seems to be that it needs to be running as the login
> session, making it difficult for the agent to get screenshots.

## Problem

`shepherd dev run` (`scripts/lib/sway.sh:sway_start_nested`) boots the launcher
in a **nested** Sway with `WLR_BACKENDS=wayland`. The wayland backend attaches
to a *parent* Wayland compositor, so the whole dev stack requires a graphical
login session to already be running. Consequences for automated (agent) or
remote (SSH) development:

- No graphical login session → no dev instance at all.
- Even with one, the render lives inside a human's desktop, so an agent has no
  reliable way to screenshot the launcher and verify a visual change.
- `run-dev` is also **blocking** (foreground `wait` on sway), so an agent that
  starts it cannot then run follow-up commands.

## Key insight

The `shepherd-e2e` harness already runs Sway **headless**
(`WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman`,
`crates/shepherd-e2e/src/lib.rs`): no parent compositor, no login session, no
GPU. That backend still exposes a real virtual output. Verified on the dev box:
headless Sway boots a `HEADLESS-1` output (default 1280x720), the mode is
settable over IPC, and a second output can be hot-added — all driven purely
through the `sway-ipc` socket.

Everything an agent needs is a client of that virtual output:

- **Screenshot** — `grim` reads the output via the `wlr-screencopy` protocol,
  which works against the pixman software buffer with no GPU.
- **Inspect** — `swaymsg -t get_tree` gives structural truth (which `app_id` is
  focused / fullscreen), cheaper and less flaky than pixel diffing.
- **Drive** — `wtype` injects keys/text via the virtual-keyboard protocol (no
  daemon, no root, unlike `ydotool`); pointer taps go through
  `swaymsg seat - cursor set/press/release`, so no pointer daemon is needed.

So the scaffold is: **promote the headless backend from a test-only trick into
an interactive, detached dev session** that reuses the *real* `sway.conf`,
`config.example.toml`, `dev-runtime/` paths, and debug binaries — i.e. a
headless twin of the production login session.

## Design

New library `scripts/lib/headless.sh`, dispatched from `shepherd dev <sub>`:

| Command | Action |
| --- | --- |
| `dev headless [--config PATH] [--user NAME] [--size WxH] [--time "..."] [--gpu] [--no-build]` | Build, then start a **detached** headless Sway running the full stack. Writes connection details to `dev-runtime/headless/session.env`. |
| `dev shot [out.png]` | `grim` the virtual output to a PNG (default timestamped under `dev-runtime/headless/`). Prints the path. |
| `dev tree [raw]` | `swaymsg -t get_tree`, summarized via `jq` to `app_id`/focus/fullscreen. |
| `dev key <keysym>...` | `wtype -k` keystrokes (e.g. `Down Return`). |
| `dev type <text>` | `wtype` literal text. |
| `dev click <x> <y> [btn]` | Move + click the virtual pointer via sway IPC. |
| `dev stop` | `swaymsg exit`, reap the stack, sweep stale sockets, remove the session file. |

Reuses `sway_setup_env`, `sway_ensure_dev_media_library`, `sway_kill_existing`,
and `sway_purge_stale_sockets` from `sway.sh` so dev/headless share one runtime
layout and cleanup path.

New deps set `agent` (`scripts/deps/agent.pkgs`: `grim`, `wtype`, `jq`, `sway`),
folded into the `dev` union so a dev checkout gets it out of the box.

`sway.conf` hard-codes `shepherdd -c ./config.example.toml` and `shepherdd`
takes its config only via `-c` (no env var). `--config PATH` therefore
generates a derived sway config under `dev-runtime/headless/sway.headless.conf`
with only that `shepherdd -c <token>` rewritten (every kiosk rule preserved),
and points sway at it. Without `--config`, the static `sway.conf` is used
unchanged (zero risk to the common path). The path is resolved to absolute
before `cd`-ing to the repo root, since `shepherdd` is exec'd with CWD=repo root.

### The intended agent loop

```
shepherd deps install agent          # once: grim + wtype + jq
shepherd dev headless --no-build     # detached; --time to pin the clock
shepherd dev tree                    # assert app_id "shepherd-launcher" is up
shepherd dev shot home.png           # Read home.png to see the UI
#   ...edit code, cargo build, dev stop && dev headless, or hot-restart...
shepherd dev stop
```

### Determinism knobs (for stable screenshots)

- `--time` → `SHEPHERD_MOCK_TIME`, so availability windows / bedtime screen /
  HUD are reproducible.
- Fixed `--size`; `sway.conf` already hides the cursor and all borders.
- `dev shot` issues `output * dpms on` first: `swayidle` blanks the output after
  120s idle (no session), which would otherwise screenshot solid black.
- `dev headless` polls the tree for the launcher surface before declaring ready
  (no fixed sleeps); seed `dev-runtime/data/shepherdd.db` for usage-dependent UI.

### Notable choices

- **Detached, not blocking.** `run-dev` blocks; the agent flow needs the session
  to persist across commands, so `headless` `setsid`s Sway and records the PID +
  sockets. `SWAYSOCK` is derived deterministically from the PID; `WAYLAND_DISPLAY`
  is discovered by diffing `wayland-N` sockets before/after boot (sway ignores a
  preset `WAYLAND_DISPLAY`).
- **`GSK_RENDERER=cairo` + `LIBGL_ALWAYS_SOFTWARE=1`** in the session env: GTK4's
  default GL renderer needs EGL, absent on the pixman/no-GPU path. This is the
  one part the e2e harness never exercised (it spawns no GTK UI), so it is the
  primary thing to validate on first run; `--gpu` selects the GL renderer against
  a real DRM node as a fallback.
- **Short-pathed `XDG_RUNTIME_DIR` required.** The wayland socket path must fit
  the 108-char `sun_path` limit; the real `/run/user/<uid>` is fine, a deep temp
  dir is not (hit during bring-up).

### Running as another user (`--user NAME`)

By default the session runs as the invoking dev user (unrestricted). `--user`
runs the whole stack as another user via `sudo` for a more realistic session —
that user's groups, `HOME`, and, by default, their regular
`~/.config/shepherd/config.toml` (what `default_config_path()` resolves; an
explicit `--config` still wins). Mechanics:

- **Dedicated runtime dir.** The target user often has no `/run/user/<uid>`
  (no login session), so the session gets a fresh, short-pathed,
  `0700`, target-owned `/run/shepherd-headless/<uid>`. With no `SHEPHERD_SOCKET`
  set, shepherdd/launcher/HUD agree on the default
  `$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock` under it.
- **Discovery as the owner.** `$!` is `sudo`, not sway, and the runtime dir is
  `0700`, so the sway PID + wayland/ipc sockets are discovered by inspecting the
  dir *as the target user* (`sudo -u … sh -c`); the PID comes from the
  `sway-ipc.<uid>.<pid>.sock` name.
- **Reattach transparently.** `shot`/`tree`/`key`/`type`/`click`/`stop` read
  `SHEPHERD_HEADLESS_USER` from `session.env` and route their tool through
  `headless_run` (a `sudo -u` wrapper). `shot` runs `grim -o OUT -` to stdout,
  which the **invoker's** shell redirects to the output file — so the PNG is
  owned by the invoker (the agent can read it) even though grim ran as the
  target user.
- **Preconditions, checked early.** The repo must be readable/executable by the
  target user (a checkout under a `0700 $HOME` is not — grant traversal, e.g.
  `setfacl -m u:NAME:x $HOME`), and the config must exist. Both are verified up
  front (as the target user) with actionable errors.
- **Liveness via `/proc`,** not `kill -0` (which returns EPERM across users).

Not reproduced by this mode's default config path but worth remembering: the
BPF firewall / internet-gating is uid+cgroup scoped and needs
`shepherd-firewall-helper` installed; it logs a warning and runs unfiltered
otherwise.

## Status / follow-ups

- Shell scaffold implemented and syntax/help/deps-listing validated. Live
  end-to-end (`grim` screenshot + GTK render under headless pixman) is pending a
  box with the `agent` deps installed.
- Possible next steps: an `e2e` test that screenshots the launcher and asserts on
  the tree; wiring the loop into the repo's `/run` + `/verify` project skills so
  future agents discover it automatically; a `dev restart` that rebuilds and
  respawns the stack without dropping the compositor.
