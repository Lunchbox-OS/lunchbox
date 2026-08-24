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

## 2. RetroArch's command socket still cannot bind to localhost (unchanged)

`2026-08-15 002 retroarch-emulator-savestates-scope.md` chose the socket-free
reset on the strength of this; re-verified against upstream `master` on
2026-08-23, since the original check was against the 1.22.2 build:

- `command.c:266` still passes `NULL` as `socket_init`'s `server`.
- `net_socket.c:64-65` derives `AI_PASSIVE` *from* that NULL — the flag is not
  independently settable.
- `configuration.c:3943` still registers only `network_cmd_port`; no bind
  address exists anywhere in the settings table.
- No upstream issue or PR asks for one (searched issues and PRs by title and
  full text). The recent merged "Add network_cmd" PR (#19025, May 2026) only
  adds `SAVE_STATE_SLOT N` and `GET_CONFIG_PARAM`.
- The `AI_PASSIVE` handling in `net_compat.c:280` is **not** the lever: it is
  inside `#if defined(HAVE_SOCKET_LEGACY) || defined(WIIU)`, the shim for
  platforms without a real `getaddrinfo`. On Linux that block compiles out.
  It is still useful evidence — it writes down the same contract glibc
  implements, so a fix that supplies a node regresses no platform.

Patch shape, if it is ever proposed upstream: `input/input_driver.c:6139-6143`
(reads the settings, calls `command_network_new`), `command.c:245` (take a
`const char *bind_address`, forward it in place of NULL, NULL/empty keeping
today's wildcard), `configuration.c` (`SETTING_ARRAY("network_cmd_bind_address",
…)` plus the struct field). Note `AF_INET` is hardcoded at that call, so an IPv6
literal would fail; `AF_UNSPEC` is worse, because `socket_init` only ever uses
the first `addrinfo`.

Even if it landed, #129 should keep its socket-free reset: shepherd targets what
Ubuntu ships, so the fallback has to exist regardless, and the highest-value
uses of the interface would be supervision (`GET_STATUS` distinguishes "content
playing" from "process alive with a window") and `SAVE_FILES`, not the reset
button that already works. A loopback bind is also not authentication — anything
running as the same uid could still send `QUIT` or `LOAD_CONTENT`.
