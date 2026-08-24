# Review follow-ups from #129 (retroarch entry kind)

Context: manual review of the `feat/retroarch-emulator-support` branch on
2026-08-23. Two questions came out of it. One produced a change; the other is
recorded here so the answer does not have to be re-derived.

## 1. Nothing checked whether an entry's core or content exists (changed)

The gap, at three layers:

- `shepherd-config/src/validation.rs` checks the *shape* of a retroarch kind
  (exactly one of `core` / `core_path`, `core` is a name and not a path, paths
  rooted, `content` and `command` non-empty) and never touches the filesystem.
  It cannot: `config.example.toml` has to pass validation in CI, where the ROM
  and core paths do not exist.
- `retroarch.rs::prepare` resolves `core` best-effort and falls back to the bare
  filename with a `warn!`; `core_path` was used verbatim, not even stat'd.
- The failure therefore surfaced only as RetroArch's own fatal error in the
  session log, which needs `service.capture_child_output` to be on to read.

The blast radius was already contained — billing starts at window-ready, which
never happens, and a zero-second session is under the default 120s
`cooldown_min_session`, so no cooldown is burned. Nobody was *told*, which is
the part worth fixing.

Fixed as a probed diagnostic rather than as validation or a launch-time error:

- `retroarch::missing_core(&EntryKind)` → `Option<MissingCore>`, built on
  `find_core` (the search half of `resolve_core`, split out so the probe gets
  neither the bare-filename fallback nor a warning per sweep). It runs the same
  resolution the launch does, so the two cannot disagree.
- `DiagnosticCode::RetroarchCoreMissing`, subject `Entry`, severity `Warning`.
  Warning and not `Critical` because `Critical` means "the configuration claims
  a protection the device is not providing" (firewall); here one activity is
  unavailable.
- Two causes, two remedies: `Unresolved` names the
  `shepherd-admin apps install retroarch <core>` command and mentions
  `core_path` for an unusual install (RetroArch may still resolve the bare
  filename against its own `libretro_directory`, so this one is *probable*, not
  certain); `NoSuchPath` is unambiguous and says so.

Deliberately **not** done: gating availability the way `FirewallNotApplied`
does. Name resolution is best-effort, so an entry shepherd cannot resolve may
still launch fine, and hiding a working activity is worse than showing a warning
next to it.

Missing *content* had the same gap and is covered the same way:
`retroarch::missing_content` and `DiagnosticCode::RetroarchContentMissing`, also
`Warning`, also per-entry. Kept as a second code rather than folded into the
first because the two fail independently and are fixed differently — an entry
missing both raises both, so an administrator is not sent back for a second
round. The check is `exists`, not `is_file`: a few cores load a directory. Being
re-probed every sweep also means a ROM on removable media raises and clears with
the drive, which is the behaviour you want from a probed condition.

## 2. RetroArch's command socket still cannot bind to localhost (filed upstream)

`2026-08-15 002 retroarch-emulator-savestates-scope.md` chose the socket-free
reset on the strength of this; re-verified against upstream `master` on
2026-08-23, since the original check was against the 1.22.2 build:

- `command.c:266` still passes `NULL` as `socket_init`'s `server`.
- `net_socket.c:64-65` derives `AI_PASSIVE` *from* that NULL — the flag is not
  independently settable.
- `configuration.c:3943` still registers only `network_cmd_port`; no bind
  address exists anywhere in the settings table.
- Nothing upstream asked for one at the time (searched issues and PRs by title
  and full text; the recent merged "Add network_cmd" PR, #19025, May 2026, only
  adds `SAVE_STATE_SLOT N` and `GET_CONFIG_PARAM`). Albert has since filed it:
  **[libretro/RetroArch#19459][ra-19459]**, "[Feature Request] Configurable
  network binding", 2026-08-23. It proposes a `network_cmd_listen` key
  defaulting to `0.0.0.0`, so existing deployments do not change, and cites this
  kiosk as the motivating case. A collaborator (hizzlekizzle) answered the next
  day: "seems like a good addition to me, assuming the implementation doesn't
  get too messy" — i.e. the door is open for a PR, on the condition that it stay
  small.
- The `AI_PASSIVE` handling in `net_compat.c:280` is **not** the lever: it is
  inside `#if defined(HAVE_SOCKET_LEGACY) || defined(WIIU)`, the shim for
  platforms without a real `getaddrinfo`. On Linux that block compiles out.
  It is still useful evidence — it writes down the same contract glibc
  implements, so a fix that supplies a node regresses no platform.

Patch shape, using the key name the issue proposes:
`input/input_driver.c:6139-6143` (reads the settings, calls
`command_network_new`), `command.c:245` (take a `const char *bind_address`,
forward it in place of NULL, NULL/empty keeping today's wildcard),
`configuration.c` (`SETTING_ARRAY("network_cmd_listen", …)` beside
`network_cmd_port` at `:3943`, plus the struct field). "Not messy" argues for
config-file-only: a menu entry drags in `menu_setting.c`, `msg_hash` and
displaylist churn that the headless use case does not need. Note `AF_INET` is
hardcoded at that call, so an IPv6 literal would fail; `AF_UNSPEC` is worse,
because `socket_init` only ever uses the first `addrinfo` — so scope it to an
IPv4 literal and say so.

Even if it landed, #129 should keep its socket-free reset: shepherd targets what
Ubuntu ships, so the fallback has to exist regardless, and the highest-value
uses of the interface would be supervision (`GET_STATUS` distinguishes "content
playing" from "process alive with a window") and `SAVE_FILES`, not the reset
button that already works. A loopback bind is also not authentication — anything
running as the same uid could still send `QUIT` or `LOAD_CONTENT`.

[ra-19459]: https://github.com/libretro/RetroArch/issues/19459

## 3. RetroArch is a native Wayland client, not XWayland (docs corrected)

`docs/emulators.md` and `config.example.toml` both told operators to set
`xwayland_native_resolution = true` on RetroArch entries, on the stated grounds
that "RetroArch is an XWayland client". Measured on 2026-08-23; it is not, and
both have been corrected.

How it was measured, so the next person can redo it rather than trust this:

```sh
sudo ./scripts/shepherd-admin apps install retroarch --ppa mgba
curl -sLo ~/Games/roms/arm.gba \
  https://raw.githubusercontent.com/jsmolka/gba-tests/master/arm/arm.gba
# fixture: one `type = "retroarch"` entry, core = "mgba", that ROM,
# service.capture_child_output = true, args = ["--verbose"]
./scripts/shepherd dev headless --config <fixture> --gpu
swaymsg output HEADLESS-1 scale 1.5          # the case the flag exists for
printf '{"request_id":1,"api_version":1,"method":"launch","params":{"id":"gba-test"}}\n' \
  | nc -U dev-runtime/shepherd.sock
swaymsg -t get_tree | jq -r '.. | objects | select(.pid) | "\(.name)\t\(.shell)"'
```

Results:

- `RetroArch mGBA 0.11-dev shell=xdg_shell app_id=com.libretro.RetroArch`, with
  no `window_properties.class`. An XWayland window is the reverse: a `class`,
  no `app_id`.
- Its own log: `[GL] Found GL context: "wayland"`, and it binds
  `wp_fractional_scale_manager_v1`, `wp_viewporter`,
  `zwp_pointer_constraints_v1`, `zwp_relative_pointer_manager_v1`.
- On the `scale 1.5` output, sway reports the window at 853x426 logical while
  RetroArch logs `[GL] Using resolution 1280x720` — the panel's own pixel grid.
  The screenshot has hard pixel edges on the ROM's pixel font, not the bilinear
  smear of a logical-size buffer upscaled by the compositor.

So the flag would have bought nothing on a RetroArch entry, while dropping every
output to scale 1.0 for the duration of the activity and forcing the HUD's
counter-scale. The corrected docs say so and show the `get_tree` check, because
the context RetroArch picks depends on the user's `retroarch.cfg`: a saved
`video_driver` can still send it through X11, and shepherd never writes that
file (`config_save_on_exit = "false"` in the generated fragment).

Caveats on the measurement: it used the PPA build
(`1.22.2+ds1+r202602131849~4c3793f36c`) on Ubuntu 26.04 with sway 1.11 headless
and the gles2 renderer. The Ubuntu archive build (`1.22.2+dfsg-2ubuntu1`) was
not run -- it wants Qt5, absent here -- but it links the same
`libwayland-egl/client/cursor` and carries the same context-driver string table,
so it is expected to behave identically.

Follow-up in the same pass: since the fragment is already rendered per launch,
it now asks for `video_context_driver = "wayland"` rather than leaving the
choice to auto-detection. Reading
`video_context_driver_init_first` (`gfx/video_driver.c:3945`) and
`vk_context_driver_init_first` (`gfx/drivers/vulkan.c:3277`) first, because the
safety of doing that is not obvious: both try the named driver and then **fall
through to iterating their whole list**, so this is a preference and not a
demand -- a host without Wayland still runs. And the name does not collide with
the Vulkan path, whose drivers are called `vk_wayland`, so a Vulkan core keeps
its own ordering (which prefers Wayland anyway). Verified in the headless
session afterwards: the fragment carries the key, RetroArch logs
`[Config] Appending config` then `[GL] Found GL context: "wayland"`, and renders
1280x720 on the scale-1.5 output. Added to `GUARDED_SETTINGS`, so an override
that fights it is warned about like the rest.
