# 2026-07-12 — Default managed Android config: GApps + DPC (Lock Task) + libndk

## Prompt

Following the DPC + GAPPS device-owner prototype ([2026-07-12 001]), the user
asked to make **GApps image + DPC device owner (with *functional* Lock Task) +
libndk ARM translation** the default Android (Waydroid) configuration shepherd
provisions and ships. Two scoping decisions were taken up front:

- **D1** — do the runtime windowing rework so the DPC's Lock Task actually locks
  the kiosk (not a dormant device owner).
- **D2** — the Waydroid *engine* install stays guided; automation starts once
  `waydroid` is installed: GApps init + libndk + DPC/set-device-owner.

One later revision: libndk is gated on **amd64** only.

Plan file: `~/.claude/plans/pure-soaring-ritchie.md`. Shipped in three phases.

## Phase 1 — provisioning (commit 36ee982)

New `scripts/lib/waydroid.sh`, wired into `shepherd-admin apps install android`:
- `provision_waydroid_gapps` — `waydroid init -s GAPPS` (idempotent; `--clean`
  opt-in to wipe `~/.local/share/waydroid/data`, which `init -f` does *not*).
- `install_libndk` — **amd64-only**, overlay-based (no `system.img` surgery):
  with `mount_overlays = True`, drops the pinned upstream ndk-translation payload
  (supremegamers commit `68734c52…`, md5-verified) into
  `/var/lib/waydroid/overlay/system` and writes the native-bridge props into
  `waydroid.cfg`'s `[properties]` (via python3 configparser). Idempotent.
- `install_dpc` — resolve the apk via `get_data_dir`, `pm install` (poll past the
  ~15 s GApps Play-Protect registration delay), then `dpm set-device-owner`,
  gated on a running session + `accounts=0` (device owner must precede sign-in).

## Phase 2 — runtime Lock Task

**2a (commit 976f0e7):** `[service.waydroid] lock_down: bool` → `lock_mode` enum
(`off|statusbar|locktask`), back-compatible (`lock_mode` wins; else legacy
`lock_down` maps true→statusbar / false→off; validated in `validate_config`;
default stays `statusbar`). New privileged helper actions `pin --package` (DPC
`LaunchActivity`) and `unlock` (DPC `ControlReceiver`), sharing force-stop's
package trust boundary, `shell --` guarding the forwarded `--es` args.

**2b (commit 799680d):** the second `spawn_android` path, validated end-to-end
against live Waydroid (`waydroid_locktask_launch_and_stop`). Lock Task suppresses
the per-app `waydroid.<pkg>` toplevel, so a locktask session is the single
full-UI **`Waydroid`** surface (exact app_id, capital W):
- spawn: `waydroid show-full-ui` → wait for the `Waydroid` surface → `pin`.
- stop: `unlock` + `force_stop` + sway-`kill` the `Waydroid` window (see gotchas)
  → the window-watch sees it gone → `Exited` (reuses the existing exit path).
- `preboot` forces `multi_windows` off for locktask; `sway.conf` gets
  `for_window [app_id="Waydroid"] fullscreen enable` after the negative-lookahead.

## Phase 3 — ship the apk (commit 2aa7ef5)

Kiosks have no Android SDK, so the DPC apk ships prebuilt. `build.sh` honors
`DPC_KEYSTORE`/passwords/alias env; `package.sh` stages a prebuilt
`shepherd-dpc.apk` into `/usr/share/shepherd/`; the `deb` job runs in the android
image (Forgejo has no cross-job artifacts) and, when `SHEPHERD_DPC_KEYSTORE_B64`
is set, builds+signs the apk before packaging + publishes it standalone. **The
DPC key is un-rotatable** (a device-owner app only updates with the same key).

## Bench findings (the hard-won ones)

Everything in Phase 2b was settled by iterating on the live box:
- Full-UI surface app_id is exactly `"Waydroid"`; DPC pin → `mLockTaskModeState=
  LOCKED`, HOME blocked; unlock → `NONE`; `dumpsys ResumedActivity` is the
  foreground signal.
- The DPC's `LaunchActivity` is briefly **unresolvable right after a session
  start** ("Activity class does not exist") → **double-pin with a 2 s settle**.
- Killing the `show-full-ui` child does **not** destroy the surface (it detaches
  the renderer), and a graceful sway `close` is **ignored** by the renderer —
  only sway **`kill`** (`WindowAction::Close` already maps to `kill`) tears it
  down.
- Test-harness gotcha: the DPC only resolves if the session was started in a
  **full shell context** (as `test-waydroid.sh` pre-starts it); a session started
  by the minimal-env test binary boots an Android that can't resolve the
  third-party DPC. Production shepherdd has a full env, so this is a harness
  artifact — `test-waydroid.sh` pre-starts the session for the locktask phase.

## Still open

- Locktask self-exit detection (app finishing *inside* the full UI) isn't
  observed — stop is the exit trigger; Lock Task largely prevents self-exit.
- The operator must generate the persistent DPC keystore + add the Forgejo
  secret; Google sign-in + device certification remain interactive.
