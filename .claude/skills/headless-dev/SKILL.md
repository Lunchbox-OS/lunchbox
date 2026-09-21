---
name: headless-dev
description: >-
  Run and visually verify Lunchbox end-to-end during development without
  a graphical login session. Use whenever you need to launch/start/run the app,
  take a screenshot, or confirm a UI change (launcher grid, HUD, media, browser,
  availability/bedtime, time limits) actually works in the real stack — not just
  in unit tests. Boots the real sway + lunchboxd + launcher + HUD headless (no
  GPU, no parent compositor), so it works over SSH / in an agent sandbox. This is
  the project skill that /run and /verify should use for this repo instead of
  falling back to `./run-dev` (which needs a login session).
---

# Headless end-to-end development for Lunchbox

Lunchbox is a Sway-based kiosk. The normal `./run-dev` boots a
**nested** compositor (`WLR_BACKENDS=wayland`) that needs a graphical login
session — unusable headlessly. Instead, use `lunchbox dev headless`, which boots
the **same** stack (same `sway.conf`, `config.example.toml`, debug binaries)
against the headless wlroots backend the e2e harness uses. It needs no login
session, no parent compositor, and no GPU, and it exposes a virtual output you
can screenshot and drive.

## One-time setup

```sh
./scripts/lunchbox deps install agent    # grim + wtype + jq (also folded into `deps install dev`)
```

## The loop

```sh
./scripts/lunchbox dev headless          # build + boot, detached; prints when ready
./scripts/lunchbox dev tree              # assert app_id "com.lunchboxos.launcher" is up/focused
./scripts/lunchbox dev shot home.png     # screenshot -> Read home.png to SEE the UI
# ...edit code...
./scripts/lunchbox dev headless --no-build   # respawn is cheap; or rebuild without --no-build
./scripts/lunchbox dev stop              # tear down when done
```

`dev headless` runs **detached** and writes connection details to
`dev-runtime/headless/session.env`, so every later `dev` subcommand reattaches
automatically. Always `dev stop` when finished (or before starting a fresh one).

**To see why the UI did what it did, not just what it did**, set `RUST_LOG`
when booting — it is passed through to the session, and there is no other way
in, since sway and lunchboxd start these binaries with no command line you
control:

```sh
RUST_LOG=lunchbox_launcher=debug ./scripts/lunchbox dev headless   # then:
grep 'laying the field out' dev-runtime/headless/sway.log
```

The target is the **binary** name, because a binary crate's root module is
named after the binary: `lunchbox_launcher`, not the package's
`lunchbox_launcher_ui`. The package name matches nothing and the filter looks
broken rather than wrong.

**`SWAYSOCK` is not the socket sway made.** lunchboxd hard-links the compositor
socket to `$XDG_RUNTIME_DIR/lunchbox-dev-sway.<n>.sock` and `session.env` records
*that* — because in production lunchboxd unlinks the original so no activity can
reach the compositor (issue #144). Finding the socket yourself
(`sway --get-socketpath`, globbing `sway-ipc.*`) is therefore not reliable; go
through `headless_run` in `scripts/lib/headless.sh`, which sources `session.env`,
or source it yourself. Add `--harden-ipc` to `dev headless` to boot the way an
installed device does, with the original name removed — everything above still
works, because it all goes through the alias either way.

**The management socket's peer check is off in dev, and `--harden-ipc` does not
turn it on.** lunchboxd otherwise accepts a client on its own socket only from
its own cgroup (issue #144). On a device that is the display manager's
root-owned session scope; here the whole stack shares the cgroup of the shell
that launched it, so the check would only refuse clients started from another
terminal while protecting nothing. Every dev entry point passes
`--no-restrict-ipc-peers`, and `headless.sh` fails loudly if `sway.conf` stops
doing so. Nothing you drive through `dev shot` / `dev tree` / `dev key` is
affected — those go through sway, not lunchboxd.

lunchboxd hardens by default; `sway.conf` opts out with `--no-harden-sway-ipc`
because it is the development config, and `--harden-ipc` takes that opt-out back
off. So a plain `dev headless` leaves the compositor reachable by `swaymsg`, and
nothing you write by hand needs to remember a flag to keep it that way.

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

- `--config PATH` — boot an arbitrary lunchboxd config instead of
  `./config.example.toml`. Good for minimal fixtures that isolate one entry/flow.
- `--time "YYYY-MM-DD HH:MM:SS"` — sets `LUNCHBOX_MOCK_TIME` so availability
  windows, the bedtime screen, HUD clock, and time-limit behavior are
  reproducible. **Use this** whenever the thing under test depends on the clock.
- `--user NAME` — run the whole stack as another user (via sudo) for a realistic
  session: their groups, `HOME`, and default `~/.config/lunchbox/config.toml`.
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
./scripts/lunchbox dev headless --time "2025-12-25 21:00:00"
./scripts/lunchbox dev shot bedtime.png   # Read it: only the after-hours entries show
./scripts/lunchbox dev stop
```

## Gotchas (read before trusting a screenshot)

- **`dev click` doesn't reliably activate GTK widgets.** The synthetic pointer
  (`swaymsg seat seat0 cursor set/press/release`) does not fire GTK4
  `connect_clicked` handlers here — a launcher grid tile won't launch and a HUD
  button won't respond, at either logical or physical coordinates. Drive the app
  a different way:
  - **Launch/stop an activity** (and anything else lunchboxd exposes): send
    newline-delimited JSON-RPC to the daemon socket at
    `./dev-runtime/lunchbox.sock`, e.g.
    `printf '{"request_id":1,"api_version":1,"method":"launch","params":{"id":"<entry-id>"}}\n' | nc -U dev-runtime/lunchbox.sock`.
    This runs the real launch path (incl. the HiDPI scale hack for
    `xwayland_native_resolution` entries). Method names/params are in
    `crates/lunchbox-ipc/src/client.rs`.
  - **A page in the web UI** (`lunchbox-webui`): `dev click` does not activate
    a link or a nav item in a browser either, and the SPA has no URL routing to
    deep-link with — so to render one page, temporarily change the initial
    `useState<Page>` in `src/App.tsx`, `npm run build`, and re-boot (the daemon
    embeds `dist/` at compile time, so this needs a full `dev headless`, not
    `--no-build`). Run the browser inside the session by sourcing
    `dev-runtime/headless/session.env` and launching `firefox --kiosk
    http://127.0.0.1:8080/` with `MOZ_ENABLE_WAYLAND=1`. Firefox on Ubuntu is a
    **snap**: a `--profile` outside `$HOME/snap/firefox/common` fails with
    "Your Firefox profile cannot be loaded", and the half-started instance then
    holds a lock that makes every later launch claim Firefox is "already
    running, but is not responding" — use the default profile. `Page_Down` does
    nothing without content focus; `wtype -M ctrl -k minus -m ctrl` (zoom out)
    is the reliable way to get more of a long page into one screenshot.
  - **The vertical HUD** (issue #171): export `LUNCHBOX_HUD_ANCHOR=left` before
    `dev headless`. That **pins** the bar, so it also stops the HUD following
    lunchboxd — which is what you want to look at the layout, and not what you
    want to test the config path. `headless.sh` forwards the variable
    explicitly so it survives the `env -i` on the `--user` path.

    To exercise the *config* path instead, leave it unset and boot
    `dev headless --config <path>` with either `[service.hud] orientation` or
    an entry's `hud_orientation`, then launch that entry over the socket. The
    daemon's view is readable at any time with the `get_hud_orientation` RPC,
    and both sides log the transition (`HUD orientation changed` from
    `lunchboxd::hud_layout`, `Rebuilding the HUD` from `lunchbox_hud::app`).
    Note that a `media`-kind activity exits within a second or two in the
    headless session, so for anything you want to screenshot mid-session use a
    long-lived `process` entry (`command = "/usr/bin/sleep"`, `args = ["600"]`).
  - **The HUD's confirm popovers** have a permanent debug-build hook:
    export `LUNCHBOX_HUD_DEBUG_CONFIRM_TRIGGER=<path>` before `dev headless`
    (env propagates from the invocation into the sway-spawned HUD), then
    `: > <path>` pops the "End session" prompt, `: > <path>.reset` pops the
    reset prompt (only for activities that offer it — `type = "retroarch"`),
    `: > <path>.volume` / `: > <path>.brightness` open the two pop-out slider
    controls (issue #178), and `: > <path>.down` dismisses whichever is up.
    Every file is consumed, so open/close cycles are just two `touch`es.
  - **The reading layout** (page-turn buttons, and the bar's worst case for
    room) has its own permanent debug-build hook: export
    `LUNCHBOX_HUD_DEBUG_FORCE_PAGE_BUTTONS=1` before `dev headless`. Okular is
    not installed here, so there is no real `type = "ebook"` session to start,
    and both #171 and #178 previously had to add a throwaway override to look
    at this layout. The hook only forces the buttons *visible*; pressing one
    still checks the real session state, so it cannot send page keys into an
    unrelated activity. Pair it with `--size 1280x600` to see the overflow
    behaviour issue #178 is about.
  - **Any other HUD-only UI action**: add a temporary one-shot debug hook gated
    behind an env var that calls `widget.emit_clicked()`, boot with the env var
    set, screenshot, then remove the hook. `dev key` (keyboard) *does* reach the
    focused surface, but the always-on HUD bar uses `KeyboardMode::None`, so keys
    won't reach it unless a popover raises it to `OnDemand`.
- **`dev key` needs a longer-lived keyboard for egui/winit apps.** `lunchbox-media`
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
- **Qt/KDE clients need the same repeat trick, and ignore synthetic clicks.**
  Okular (issue #160's Phase 0) takes `wtype -s 800 -M ctrl -k o -m ctrl` when
  the sequence is repeated two or three times, and drops a single press. Menubar
  mnemonics (`Alt+F`) never landed at all, and `dev click` on a menu title did
  nothing — so check a menu's *contents* through its keyboard shortcuts, with an
  unrestricted control run to prove the key would have worked.
- **Never `pkill -f <pattern>` from a driver command.** `bash -c` puts the whole
  script in its own command line, so the pattern matches the driver itself and
  kills the script mid-run (exit 144) — the same failure mode as the `sleep`
  gotcha below, from the other direction. Use `pgrep -x <name>` and kill the
  pids.
- **A GUI app that ignores `SIGTERM` can be closed politely** with
  `swaymsg '[app_id="…"] kill'` (an `xdg_toplevel.close` *request*, not a
  signal), which is how you verify save-on-close behaviour that Lunchbox's
  current graceful stop does not trigger. See the #160 scope note.
- **Settle after "ready".** `dev headless` returns once the launcher *surface*
  maps, but async icon/tile loading can lag a beat (a tile may still say
  "Loading…"). For the fully-painted UI, poll `dev tree` for the specific entry,
  or take a second `dev shot` a moment later.
- **Assert the state you meant to capture, in the pixels.** A `launch` that
  races lunchboxd's startup — or hits a daemon that is still running the previous
  activity — is rejected, and the "No session" bar screenshots just as happily
  (with the volume slider in its *disabled* styling, which measures differently
  from the live one). Before keeping a shot, poll for something only the target
  state paints, e.g. the warning banner's background colour.
- **The headless output is not at scale 1, so `--size` is not logical pixels.**
  It comes up at **scale 1.5**, so `--size 1280x600` gives a *853x400 logical*
  screen — a far harsher test than it looks, and the wrong number to quote when
  an issue is written in logical pixels (issue #178 is: "displays less than 720
  logical pixels tall"). Check with `swaymsg -t get_outputs` — the output's
  `rect` is logical, its `current_mode` is physical — and pin both together
  before measuring anything size-dependent:

  ```sh
  swaymsg output HEADLESS-1 mode 1280x720 scale 1   # 1280x720 logical
  ```

- **Output-scale changes settle asynchronously.** Useful when testing the
  `xwayland_native_resolution` HiDPI hack, which needs a non-1x scale
  (`swaymsg output HEADLESS-1 scale 1.5`, via `headless_run` in
  `scripts/lib/headless.sh` — there is no `dev swaymsg` passthrough). `stop_current`
  returns *before* lunchboxd restores the pre-launch scale, so a scale you set
  immediately afterwards gets clobbered a second later. Wait, then re-read
  `swaymsg -t get_outputs`, and check it again at screenshot time.
- **Black screenshot?** `swayidle` blanks the output (DPMS off) after ~120s idle
  with no session; `dev shot` already forces `dpms on` first, but if you script
  raw `grim`, do the same.
- **GTK renderer.** The session sets `GSK_RENDERER=cairo` (GTK4's GL renderer
  needs EGL, absent on the pixman/no-GPU path). If the UI doesn't paint, check
  `dev-runtime/headless/sway.log` and try `--gpu`.
- **`--gpu` is not enough for hardware video decoding.** logind grants
  `/dev/dri/renderD128` by ACL to whoever holds the *active seat* session, and an
  SSH or agent shell has no seat — so `mpv`/`lunchbox-media` silently decode in
  software and anything GPU-specific (VA-API decode paths, driver bugs) cannot be
  reproduced. There is no error; `hwdec-current` just reads `no`. Grant yourself
  the node first:

  ```sh
  sudo setfacl -m u:$USER:rw /dev/dri/renderD128   # revoke with -x u:$USER
  ffmpeg -init_hw_device vaapi=va:/dev/dri/renderD128 -f lavfi -i testsrc2 -f null -  # check
  ```

  Then boot with `--gpu` and *confirm* the path is live before trusting a result
  — `lunchbox-media --log-level info` logs which decoder mpv settled on, and bare
  `mpv` answers `{"command":["get_property","hwdec-current"]}` over
  `--input-ipc-server`. This is what made an earlier session conclude the machine
  was "a VM with no GPU access" and ship an unverified fix
  (`docs/ai/history/2026-09-04 001 green-frames-after-a-seek.md`).
- **Config edits.** `config.example.toml` must pass `./scripts/lunchbox config
  validate` (CI checks this). Test config-driven changes with `--config` against
  a fixture first.
- **Never give a fixture entry `command = "sleep"`.** Stopping a session calls
  `kill_by_command(<command name>)`, which kills *every* process of that name the
  user owns — including the `sleep`s in your own driver script, which then dies
  mid-scenario (exit 144). Point the fixture at a small wrapper script
  (`exec tail -f /dev/null`) so the name is unique to the fixture.
- **`dev headless` right after `dev stop` can hang lunchboxd at startup — retry
  it.** Twice in four boots (2026-09-07) the daemon stopped dead after

  ```
  INFO lunchbox_host_linux::process: Activities will be launched into a cgroup of their own
  ```

  — the last line of `process::init()` — and never reached the volume-controller
  line, the compositor, or anything else, so the harness reported "lunchboxd did
  not connect to the compositor within 30s" against a perfectly healthy sway.
  The GTK clients time out on the portal 25 s later, which makes the log look
  like a compositor problem it is not. It is not caused by the config: the same
  config booted first try on the retry. Kill every survivor the
  `ps -eo pid,cmd | grep …` listing below names, `rm
  dev-runtime/headless/session.env`, and boot again — do not spend time
  debugging the daemon over a first failed boot.
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
  Also note that killing lunchboxd runs `kill_by_command` on the way down, which
  kills every `sleep` you own — a driver command containing one dies with exit
  144 alongside it, and that is one of the ways you end up orphaned here in the
  first place.
- **Do not look for survivors with `pgrep -f`.** `-f` matches whole command
  lines, so `pgrep -f sway.headless.conf` matches *the shell running the pgrep*,
  which contains that string as an argument. It therefore always "finds" a
  session, and reports a different pid every time — which reads exactly like a
  stack respawning itself in a loop. `pkill -f` has the same blind spot with
  teeth: it matches your own `bash -c` and kills the shell you are typing in,
  which surfaces only as a silent exit 144. Use a listing you can eyeball
  instead, and kill the pids it names:

  ```sh
  ps -eo pid,cmd | grep -E "sway.*headless|lunchboxd|lunchbox-media" | grep -v grep
  ```

  Also check for more than sway: `dev stop` can report "Headless session
  stopped" and leave the `lunchboxd` it spawned (and its two `sh -c` wrappers)
  running against your fixture config. Those keep the socket alive, so the next
  boot's launch requests land in the *old* daemon. Kill every pid that listing
  shows before booting again.
- **`--no-build` against a cleaned `target/debug`** boots a session whose
  `lunchboxd` binary is missing; sway's `|| swaymsg exit` then tears the whole
  session down a second later. Build once before using `--no-build`.
- **"lunchboxd did not connect to the compositor within 30s" can be a lie.**
  A GTK `Cannot get portal org.freedesktop.host.portal.Registry version: Timeout
  was reached` eats ~26s of the 30s budget, and the alias socket then appears at
  around T+50s with a perfectly healthy stack behind it. The damage is that the
  start path bailed **without writing `session.env`**, so `dev stop` says "No
  live headless session to stop" and every `dev` subcommand has nothing to
  reattach to while sway, lunchboxd, the launcher and the HUD all keep running.
  Confirm with `ls /run/user/1000/ | grep sway` (the alias is there) and
  `ls -l dev-runtime/lunchbox.sock` (live), then kill the pids from
  `ps -eo pid,cmd | grep -E "sway.*headless|lunchboxd"`, `rm
  dev-runtime/headless/session.env`, and boot again — the second boot is
  usually well inside the budget.
- **"lunchboxd did not connect to the compositor" is a different failure from
  "Sway did not create its IPC socket"**, and the harness now tells them apart:
  the first waits on the alias (which only exists once lunchboxd has connected),
  the second on either socket name. If you get the first, sway is fine and the
  daemon is the problem — read `dev-runtime/headless/sway.log` rather than
  suspecting the compositor.

## Seeing the web UI (not just the native surfaces)

The management SPA is embedded into `lunchboxd` and served on
**`https://127.0.0.1:8080`** — `https`, since issue #156: `config.example.toml`
binds `0.0.0.0`, and a non-loopback bind now comes up on a generated
self-signed certificate rather than in the clear. The session has **Firefox**,
so the web UI can be rendered and driven here; it does not need a JS
component-test harness.

`dev click` **cannot** drive it: the headless seat has no pointer device, so
Firefox never binds `wl_pointer` and synthetic `swaymsg ... cursor press` events
land nowhere (same root cause as the `dev key` note above, and adding motion and
delays does not help). Drive it over **Marionette** instead, which is built into
Firefox and needs no extra packages:

```sh
# Profile must live under $HOME — Firefox is a snap and cannot read /tmp/claude-*.
PROF=$HOME/ff-test; mkdir -p $PROF
printf 'user_pref("marionette.port", 2828);\nuser_pref("browser.aboutwelcome.enabled", false);\n' > $PROF/user.js
set -a; . dev-runtime/headless/session.env; set +a
MOZ_ENABLE_WAYLAND=1 setsid firefox --profile $PROF --marionette --new-window about:blank &
```

Then speak the wire protocol (length-prefixed JSON on TCP 2828): connect, read
the handshake, `WebDriver:NewSession`, then `WebDriver:ExecuteScript` /
`WebDriver:FindElement` / `WebDriver:ElementSendKeys`. Only **one** session at a
time, so do a whole scenario in one script. `ExecuteScript` is enough to click
MUI controls and read `innerText` back for assertions; `dev shot` still gets you
the pixels.

**Pass `acceptInsecureCerts` at `WebDriver:NewSession`**, or every navigation
fails with `insecure certificate` against the dev stack's self-signed cert.
Marionette wants it in both places:

```python
m.cmd("WebDriver:NewSession", {
    "acceptInsecureCerts": True,
    "capabilities": {"alwaysMatch": {"acceptInsecureCerts": True}},
})
```

Launch on `about:blank` and navigate *after* the session exists, for the same
reason: a URL on the command line is loaded before any capability applies.

**Signing in (issue #156).** The API no longer has an open mode, and the
`localStorage` `apiToken` route is gone for browsers — a person logs in and
gets an `HttpOnly` cookie. On a fresh dev stack:

```sh
CODE=$(grep enrolment_code dev-runtime/data/web-auth.toml | cut -d'"' -f2)
curl -sk -c cookies.txt -X POST https://127.0.0.1:8080/api/v1/auth/setup \
  -H 'Content-Type: application/json' \
  -d "{\"code\": \"$CODE\", \"password\": \"a real password\"}"
```

or drive the setup form in the browser, which is the same thing and also shows
you the screen. To get back to a *fresh* device — the setup screen, a new code
on the TV — `rm dev-runtime/data/web-auth.toml` and restart the session.

For a scripted client that wants no browser at all, set
`auth_token` under `[service.management_api]` and send
`Authorization: Bearer`: it authenticates a request, and deliberately cannot
open a session.

Gotchas here:

- **A curl cookie jar is keyed by host, so a session can look revoked when it
  is not.** Move the daemon from a loopback bind to a LAN bind and every
  `-b cookies.txt` request comes back 401, because curl silently stops sending
  a cookie saved under `127.0.0.1`. Send it explicitly —
  `-H "Cookie: lunchbox_session=$(awk '/lunchbox_session/{print $7}' cookies.txt | tail -1)"`
  — before concluding anything about session lifetime or revocation.

- **`pkill -f firefox` kills your own shell.** The Bash tool's wrapper carries
  the command text, so `-f` matches it. Use `pkill -x firefox`, or match the
  profile path.

- **`npm run build` alone does not reach the running daemon.** `rust_embed`
  embeds `lunchbox-webui/dist/` at compile time but does not make cargo consider
  `lunchboxd` dirty when only `dist/` changed, so `dev headless` cheerfully
  reboots the *old* SPA. Touch the embedding source first:
  `touch crates/lunchbox-http/src/web_assets.rs && cargo build -p lunchboxd`.
- **A fullscreen window freezes the HUD's pixels.** The compositor stops sending
  frame callbacks to an occluded layer surface, so GTK stops repainting and
  `grim` captures whatever the HUD last drew — a *stale* clock and a stale
  volume, minutes old, which reads exactly like a missed-event bug. Verify HUD
  state with the window closed (or non-fullscreen), and sanity-check the HUD
  clock against `date` before believing a HUD screenshot taken over another app.
- **A session that is up but unreachable: check the socket.** If the launcher and
  HUD loop on `Failed to connect to lunchboxd: No such file or directory` while
  `[OK] Headless session up` and `pgrep lunchboxd` both say everything is fine,
  look at `ls dev-runtime/lunchbox.sock`. Two overlapping daemons used to end
  this way — the outgoing one deleted the path the incoming one had bound — which
  `IpcServer::shutdown()` now guards against by only removing the socket file it
  bound itself. If you see it anyway, kill every `lunchboxd` and
  `sway.headless.conf` process, remove `session.env`, and boot again.
- **Real pointer input works through Marionette, unlike `swaymsg`.**
  `WebDriver:PerformActions` with a `pointer` source delivers genuine
  pointerdown/move/up to the page, so drag-to-commit controls (an MUI slider)
  can be driven properly. Keyboard via `WebDriver:ElementSendKeys` also works,
  but a control that disables itself while a mutation is in flight swallows most
  of a key burst — prefer a pointer drag when one gesture should produce one
  commit.

## Under the hood

Connection state is in `dev-runtime/headless/session.env`; the compositor log is
`dev-runtime/headless/sway.log`. Design notes and rationale:
`docs/ai/history/2026-07-14 001 headless-agent-dev-scaffold.md`. Human-facing
usage: the "Headless development" section of `CONTRIBUTING.md`.
