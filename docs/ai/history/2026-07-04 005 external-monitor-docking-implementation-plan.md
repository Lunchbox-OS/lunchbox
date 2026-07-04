# External monitor / docking — implementation plan (issue #87)

Follows the scope in `2026-07-04 004 external-monitor-docking-support.md`.
Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/87>

## Decisions (from issue author)

1. **Mirroring:** use [`wl-mirror`](https://github.com/Ferdi265/wl-mirror) (new runtime dep).
2. **Mirror resolution:** drive the primary logical output at the *highest
   mutually-compatible resolution*; let the external hardware upscale.
3. **Primary selection:** first-enumerated output at boot.
4. **Toggle persistence:** every new secondary connection resets to Mirror.
5. **Audio:** route audio to the external video device whenever a secondary is
   in use — in *both* Mirror and External-only modes.
6. **HUD:** always follows the active/primary output.

## Display state machine

Three modes; **exactly one logical output is ever active**, preserving the
one-activity invariant.

| Mode | Trigger | Outputs | HUD on | Mirror proc | Audio |
|------|---------|---------|--------|-------------|-------|
| `SingleInternal` | no secondary present | primary only | primary | — | internal |
| `Mirror` (default) | secondary connects | primary active @ chosen mode; secondary shows wl-mirror window | primary | running | external |
| `ExternalOnly` | HUD toggle from Mirror | primary **disabled**; secondary active @ native | secondary | — | external |

Transitions:
- **secondary connect** → always `Mirror` (decision #4), regardless of prior mode.
- **HUD toggle** → `Mirror ⇄ ExternalOnly`.
- **secondary disconnect** → `SingleInternal`; stop mirror, re-enable internal,
  restore audio to internal, move HUD back to internal.
- **boot** → capture first-enumerated output as `primary`; `SingleInternal` if
  it's the only one, else `Mirror`.

"Primary" is fixed to the first-enumerated (internal) panel. In `ExternalOnly`
the internal is disabled and the secondary is the sole active output, so the
HUD's "active output" = secondary there.

## Component changes

### 1. `shepherd-host-linux/src/sway.rs` — output primitives
Make the sway layer testable by routing all `swaymsg` calls through one
injectable runner (small trait or `fn` pointer), mirroring the
`HostAdapter`/`MockHost` seam. Then add:

- Extend `RawOutput`/a new `DisplayInfo` to decode: `name`, `active`,
  `focused`, `make`/`model`/`serial`, `current_mode`, `modes: Vec<Mode>`,
  `rect`. (Currently only `name`/`scale`/`active`.)
- `get_displays() -> Vec<DisplayInfo>`.
- `enable_output(name)` / `disable_output(name)` → `output <name> enable|disable`.
- `set_output_mode(name, Mode)` → `output <name> mode <w>x<h>@<hz>Hz`.
- `pick_mirror_mode(primary: &DisplayInfo, secondary: &DisplayInfo) -> Mode`:
  highest-pixel-area mode present in **both** mode lists; fallback to the
  primary's preferred/current mode when there is no common mode (TV upscales).
- Unit tests for parsing + `pick_mirror_mode` (common-mode, no-common-mode,
  aspect-mismatch cases), in the existing `#[cfg(test)]` style.

### 2. `shepherd-host-api/src/traits.rs` — controller trait
- `DisplayController` trait (async): `initialize()`, `on_output_changed()`,
  `set_mode(DisplayMode)`, `current() -> DisplayState`. Sits next to
  `HidpiController`.
- `NoOpDisplayController` for tests / HTTP-only contexts (like
  `NoOpHidpiController`).
- New shared types: `DisplayMode { Mirror, ExternalOnly }`,
  `DisplayState { mode, primary: Option<..>, secondary: Option<..> }`.

### 3. `shepherdd/src/display.rs` (new) — the controller
Modeled on `hidpi.rs::XwaylandHidpi` (holds `Arc<IpcServer>` +
`broadcast::Sender<Event>`, guards state under a `Mutex`, mutates outputs,
broadcasts). Responsibilities:

- Hold `primary` (captured first-enumerated at init) + current `DisplayState`.
- Apply each mode transition as an idempotent sequence of `sway.rs` calls:
  - **Mirror:** re-enable internal; `set_output_mode(primary, pick_mirror_mode)`;
    ensure secondary enabled at native; **(re)start wl-mirror** capturing
    `primary` onto the secondary; route audio → external; HUD event.
  - **ExternalOnly:** stop wl-mirror; `set_output_mode(secondary, native)`;
    `disable_output(primary)` (sway migrates the workspace to secondary);
    audio → external; HUD event.
  - **SingleInternal:** stop wl-mirror; re-enable/focus internal; audio →
    internal; HUD event.
- **wl-mirror lifecycle:** own the child process; inject the launcher (a
  `Fn -> Child`) so e2e/tests substitute a stub. Restart on crash. Args roughly
  `wl-mirror <primary-name>`; pin/place via sway rules (below).
- Broadcast `EventPayload::DisplayModeChanged { state }` on every change (IPC +
  `event_tx`, exactly like `broadcast_factor`).

### 4. Hotplug detection — `shepherdd/src/display_watch.rs` (new)
Spawn a persistent `swaymsg -t subscribe -m '["output"]'` child, read its
NDJSON stdout, and call `DisplayController::on_output_changed()` on each event.
Wire into the daemon startup next to `start_monitor` (`main.rs:187`). Debounce
rapid connect/disconnect. This is cleaner and more testable than polling.

### 5. Audio routing — `shepherd-host-linux/src/audio_route.rs` (new, or extend `volume.rs`)
`volume.rs` already drives PipeWire via `wpctl`. Add:
- `route_to_external(connector_hint) -> Result<PrevSink>`: enumerate sinks
  (`pw-dump` / `wpctl status`), pick the available sink whose ALSA
  path/description marks it HDMI/DisplayPort, save the current default, then
  `wpctl set-default <id>`.
- `restore_audio(PrevSink)` on disconnect / SingleInternal.
- Correlation is best-effort (connector→sink mapping is imperfect); on no match,
  leave the default untouched and `warn!`. Guarded by config (below).

### 6. HUD — `shepherd-hud`
- New icon toggle button (Mirror ⇄ ExternalOnly), modeled on the mute toggle in
  `volume.rs`: click → IPC `set_display_mode`; state updates on the
  `DisplayModeChanged` broadcast. Icon: `video-display-symbolic` /
  `computer-symbolic` pair.
- **Visibility:** button only shown when a secondary is present (drive from
  `DisplayState` in the event; query once on startup).
- **Follow active output (#6):** on `DisplayModeChanged`, re-anchor the
  layer-shell surface to the active output via gtk4-layer-shell `set_monitor`.
  In Mirror the HUD sits on the internal and is cloned by wl-mirror; in
  ExternalOnly it moves to the external.
- `state.rs` already subscribes to the event stream — add the new variant.

### 7. API surface
- `shepherd-api/src/events.rs`: `EventPayload::DisplayModeChanged { state: DisplayState }`
  (next to `HudScaleChanged`). Update the `is_*`/match arms as needed.
- `shepherd-management/src/service.rs` (`ManagementService`, `#[management_rpc]`):
  `get_display_state() -> DisplayState` and `set_display_mode(mode) -> DisplayState`.
  Auto-exposed over HTTP + BLE. Add `DisplayController` to
  `DefaultManagementService` deps (Arc), `NoOp` in the HTTP/dispatch test
  fixtures (`shepherd-http/tests/api.rs`, `shepherd-management/tests/dispatch.rs`).

### 8. sway.conf rules
Add rules so the mirror window is contained and never counts as an activity:
```
# wl-mirror: pin fullscreen to the external output, never focus it
for_window [app_id="wl-mirror"] move to output <secondary>, fullscreen enable
```
Ordering matters — it must precede the global
`for_window [app_id="^(?!shepherd-launcher$).*"] fullscreen disable` and the
`for_window [floating] floating disable` / scratchpad rules so it isn't undone
(same lesson as the Steam rules). Keep `focus_on_window_activation smart` so the
mirror can't pull focus. Activities/launcher continue to map to the focused
(primary) workspace, so the invariant holds. Since output names aren't known
ahead of time, shepherdd applies the `move to output` at mirror-start via
`swaymsg` rather than hardcoding the connector in the static config.

### 9. Config — `shepherd-config` + `config.example.toml`
Small `[display]` section, all with safe defaults so existing configs are
unaffected:
- `docking_enabled = true` — master switch for the whole feature.
- `mirror_audio = true` — route audio to external when docked (decision #5).

Update `config.example.toml` and confirm it passes validation (CLAUDE.md).

## Testing

- **Unit:** `pick_mirror_mode`, primary detection, output/mode parsing (sway.rs
  style); `DisplayController` transition sequences against a mock sway runner +
  stub wl-mirror launcher (assert exact swaymsg command sequence per
  transition, idempotency, crash-restart).
- **e2e (`shepherd-e2e`, headless sway):** the headless backend supports
  `create_output` / a second `WLR_HEADLESS_OUTPUTS`. Simulate connect → assert
  `DisplayModeChanged{Mirror}` on the event stream and that the internal stays
  active; `set_display_mode(ExternalOnly)` → assert internal disabled; simulate
  disconnect → `SingleInternal`. wl-mirror can't truly capture headless, so use
  the injected stub launcher and assert it was invoked with the right source.
- Audio routing: unit-test sink selection against captured `pw-dump` JSON
  fixtures; leave live routing to manual hardware verification.

## Suggested PR sequencing

1. **Output primitives + testable seam** in `sway.rs` (parsing, enable/disable,
   set_mode, `pick_mirror_mode`) — pure, fully unit-tested, no behavior change.
2. **API + management RPCs + events** (`DisplayMode`, `DisplayState`,
   `DisplayModeChanged`, trait + NoOp) — wiring only, fixtures updated.
3. **DisplayController + hotplug watcher + sway.conf rules** — the core; e2e
   with stub mirror launcher.
4. **wl-mirror integration** (real launcher, crash-restart) + INSTALL.md dep.
5. **Audio routing.**
6. **HUD toggle + follow-active-output.**

Each PR builds/tests/lints green and `cargo fmt --all` clean.

## Risks / watch-items

- **wl-mirror as a real window on the secondary's workspace** — it is
  infrastructure, not an activity, but the sway rules and focus policy must
  guarantee it can't be focused or spawn siblings. Primary verification target.
- **No common mode** between panels → `pick_mirror_mode` falls back to primary
  preferred; confirm wl-mirror aspect-preserving scaling looks acceptable on a
  4:3/16:10 ↔ 16:9 mismatch (letterbox).
- **Connector→audio-sink correlation** is heuristic; may need per-host tuning.
  Fail soft (leave default, log) so it never breaks docking.
- **Mode-set races** on rapid hotplug — debounce in the watcher; make
  transitions idempotent.
- **`wl-mirror` availability** — add to `docs/INSTALL.md` and the CI image;
  degrade gracefully (log + stay SingleInternal-with-extend-disabled) if absent.

---

## Post-implementation: real-hardware testing & fixes

The feature was implemented and merged onto `main` (rebased over the
auto-brightness PR #88; history docs renumbered 003→004/005 to avoid colliding
with `2026-07-04 003 automatic-screen-brightness.md`). Testing on a real
handheld + external TV then surfaced a series of issues that several of the
plan's "watch-items" anticipated. Each was root-caused and fixed; commit hashes
are on the `u/albert/87/external-monitor-docking` branch.

### CI: deps image build failed (`a2492df`)
Adding `wl-mirror` to `scripts/deps/run.pkgs` invalidated the previously-cached
`RUN ./scripts/shepherd deps install` layer in `.ci/Dockerfile`, exposing a
latent breakage from the version-harmonization work (#83): `scripts/shepherd`
now reads the repo-root `VERSION` file at startup, but the Dockerfile only
`COPY`s `scripts/`. Fix: `COPY VERSION` into the image before the `RUN`. Its
value is irrelevant to what gets installed — only its presence matters.

### 1. Cursor could click the dead mirror surface (`240f5f6`)
In mirror mode the external output sat beside the primary in the layout, so the
pointer could travel onto it and land on the (uninteractive) `wl-mirror`
surface, where clicks went nowhere.

- **First attempt (reverted): overlap the outputs at the same origin.** This is
  the canonical sway mirroring layout and *did* trap the cursor, but it
  destabilized sway's rendering under `wl-mirror`'s screencopy — the primary went
  black with cursor trails (the classic "no full repaint, software cursor
  overdraw" signature). Dropped.
- **Fix: confine the pointer, not the outputs.** sway's `map_to_output` applies
  to pointer devices, so `input type:pointer map_to_output <primary>` keeps a
  relative mouse on the primary; released with `map_to_output *` in
  single-internal / external-only. Outputs stay in their normal side-by-side
  layout (which rendered fine), and `wl-mirror` still shows the cursor on the TV
  because it mirrors the primary.

### 2. HUD vanished after a few toggles; resolution not restored on undock (`7fa0f58`)
Two bugs:
- **HUD vanished** — `set_mode` and `reconcile` weren't serialized, so rapid HUD
  toggles ran `apply()` concurrently, interleaving enable/disable, mode-set,
  `wl-mirror` start/stop (incl. its 500 ms settle) and pointer mapping, corrupting
  the arrangement and the HUD's layer surface. Fix: an `apply_lock` held across
  every reconcile/apply so they run one at a time.
- **Resolution not restored** — mirror mode drives the primary to
  `pick_mirror_mode` (may differ from native). Fix: capture the primary's native
  mode at startup and restore it whenever the external disconnects and the
  primary becomes the sole output.

### 3. Fullscreen activities broke mirroring + resolution
Enumerated the solution space (force composition; re-assert on lifecycle;
coordinate with the HiDPI controller; constrain fullscreen; per-activity
arrangement). Tried them in order:

- **HiDPI coordination first (`bf44579`).** The XWayland HiDPI workaround (#45)
  and the docking controller both mutate sway outputs independently. Gave
  `DisplayManager` a `reassert()` and had `XwaylandHidpi` call it after changing
  scales on apply/restore (i.e. around an activity launch/exit), serialized on
  the same `apply_lock`. Insufficient on its own.
- **Root cause + real fix (`58a98c3`).** The HiDPI hack reconfigures the
  primary's *scale* right as the activity opens, which leaves `wl-mirror` showing
  a black frame or exited — and `reassert` was only *re-pinning* the dead
  process. Factored the bring-up into `establish_mirror` and made it always
  **restart** `wl-mirror` (fresh capture) from both apply and reassert, so it
  recovers after any source-output reconfiguration. Plus
  `WLR_SCENE_DISABLE_DIRECT_SCANOUT=1` in the sway env (dev launcher +
  documented for prod): a fullscreen surface would otherwise direct-scan-out and
  starve `wl-mirror`'s screencopy of frames, blacking the TV even while
  `wl-mirror` is alive. Confirmed working on hardware (incl. Human Resource
  Machine with the native-resolution hack).

### 4. HUD vanished when switching to external-only (`02cc925`)
Distinct from #2's concurrency. Disabling the primary destroys the HUD's
layer surface anchored there, but GTK still believed the window was visible, so
`set_monitor` + `present()` had nothing to remap and the anchor cache stuck.
Fix: re-anchor with an explicit `set_visible(false)` → `set_monitor` →
`set_visible(true)` cycle — GTK4 unmaps/maps synchronously, tearing down the dead
surface and building a fresh one on the live output.

### Lessons
- **Overlapping outputs + screencopy is a trap** — confine the pointer instead.
- **`wl-mirror` is fragile across source-output reconfiguration** — restart it
  (fresh capture) rather than re-pinning; force composition so fullscreen
  activities don't starve its capture.
- **Two independent output-mutating controllers must be serialized** and made to
  re-assert around each other (the `apply_lock` + `reassert()` pattern).
- **gtk4-layer-shell surfaces don't survive their output being disabled** — a
  synchronous hide→set_monitor→show is the reliable way to follow the active
  output.
