# Automatic screen brightness

Implements Forgejo issue #81 ("Automatic screen brightness"), which had an
empty body. Direction was clarified with the user before implementing.

## Clarified requirements

The device has a real ambient light sensor (IIO `als` device,
`in_illuminance_raw`) alongside `intel_backlight`. The user chose:

- **Sensing basis:** ambient light sensor (ALS), **clamped by the existing
  brightness restrictions** (min/max, per-entry-or-global). There is no
  separate global day/night schedule in the codebase — the "schedule" is the
  existing per-entry `[entries.brightness]` restriction, which is already
  time-gated because a bedtime activity is only launchable inside its
  availability window. Auto brightness therefore feeds its ALS-computed target
  through the **same** `BrightnessRestrictions::clamp_brightness` path manual
  sets already use, so bedtime dimming still caps it.
- **Manual interaction:** phone-style. Auto runs continuously; a manual slider
  drag temporarily overrides, and auto resumes on the next significant ambient
  light change. The slider stays visible.
- **Enablement:** config default (`[service.brightness.auto]`) plus a runtime
  on/off toggle reachable from the HUD (IPC), the HTTP management API, and BLE,
  persisted in the store so it survives restarts.

## Architecture

The manual brightness pipeline (added in
`2026-05-22 001 brightness-slider.md`) is mirrored. Since PR #65 the transports
(IPC/HTTP/BLE) are thin adapters over `shepherd_management::dispatch_json`, so
new RPCs only touch the `ManagementService` trait + impl and the codegen.

Layers:

- **`shepherd-host-api`** — new read-only `LightSensor` trait
  (`capabilities()`, `read_lux()`) + `LightSensorCapabilities`. Sync, because
  an ALS read is one small sysfs read and keeping it sync lets the pure policy
  be tested without a runtime.
- **`shepherd-host-linux`** — `LinuxLightSensor`. Scans
  `/sys/bus/iio/devices/iio:deviceN/` for `in_illuminance_raw`, preferring a
  device named `als`; lux = `(raw + offset) * scale` per the IIO ABI. Files are
  world-readable so no helper is needed (same access model as the backlight
  read).
- **`shepherd-management/auto_brightness.rs`** — the pure, hardware-free core:
  `AutoBrightnessCurve` (logarithmic lux→percent, because perceived brightness
  and indoor→outdoor lux both scale with log illuminance) and
  `AutoBrightnessState` (on/off + the manual-override state machine). A manual
  override holds until ambient light changes by a ratio (`LUX_RESUME_RATIO`);
  a hysteresis band (`APPLY_MIN_DELTA`) suppresses per-tick jitter. Fully unit
  tested.
- **`shepherd-config`** — `[service.brightness.auto]` → `RawAutoBrightnessConfig`
  → `AutoBrightnessPolicy` (device-global; `enabled` + curve + `poll_interval`)
  stored on `Policy.auto_brightness`. Per-entry auto is intentionally not a
  thing (documented; ignored if written). Validation checks percent ranges,
  `dim_lux < bright_lux`, and non-zero poll interval.
- **`shepherd-api`** — `BrightnessInfo` gains `auto_available` / `auto_enabled`
  (serde-default so the wire stays backward compatible); `BrightnessChanged`
  event gains `auto_enabled` so all subscribers stay in sync.
- **`shepherd-store`** — a generic `settings(key, value)` table with
  `get_setting`/`set_setting`; auto state persists under
  `AUTO_BRIGHTNESS_SETTING_KEY`.
- **`shepherd-management`** — `DefaultManagementService` gains
  `light_sensor: Option<Arc<dyn LightSensor>>` and
  `auto_brightness: Arc<Mutex<AutoBrightnessState>>`. New RPCs
  `set_auto_brightness` / `toggle_auto_brightness`. `set_brightness` now
  registers a manual override. `auto_brightness_tick` (inherent, driven by the
  daemon's poll loop and once on enable) samples the sensor, resolves the curve
  + restrictions under one engine lock, and applies via the shared clamp.
  Restriction resolution is extracted to `resolve_brightness_restrictions` and
  shared with the RPC path. Note: auto applies subject to the **clamp** (min/max)
  but not the `allow_change` gate — that gate is about the child changing
  brightness, whereas auto is a parent-enabled system behavior.
- **`shepherdd`** — constructs `LinuxLightSensor`, computes the initial enabled
  state (store value else config default; forced off with no sensor), builds
  the service as a concrete `Arc<DefaultManagementService>` (so the loop can
  call the inherent tick) then shares it as `Arc<dyn ManagementService>`, and
  spawns the poll loop (respecting the shutdown watch).
- **`shepherd-hud`** — an "A" `ToggleButton` next to the slider, visible only
  when `auto_available`, guarded against feedback loops. The slider still
  drives `set_brightness` (which registers the override).
- **`shepherd-webui`** — TS types + client `setAutoBrightness` + an
  "Automatic brightness" switch on the admin brightness card.

## Locking

Both the RPC path and the tick acquire the engine lock and the auto-brightness
lock, but never hold both simultaneously (engine is always released before
auto), so there is no lock-ordering cycle.

## Testing / verification

- Pure curve + override state machine: unit tests in `auto_brightness.rs`.
- Store settings roundtrip: unit test in `sqlite.rs`.
- Full RPC behavior (enable persists + applies, toggle flips, no-sensor
  rejected) through `dispatch_json` against a real service over an in-memory
  store + mock sensor: `shepherd-management/tests/dispatch.rs`.
- Config validation + `config.example.toml` passes `validate-config`.
- `rpc-codegen` regenerated (`docs/rpc-schema.json`, `RpcMethods.kt`,
  `rpc-methods.generated.ts`); drift test green.
- Real hardware: a throwaway example confirmed `LinuxLightSensor` detects the
  `als` device and reads lux (~4 in a dim room), then was removed.
- `cargo build --workspace`, `cargo test --workspace --exclude shepherd-e2e`,
  `cargo clippy --all-targets -D warnings`, `cargo fmt`, and webui `tsc` all
  green.
