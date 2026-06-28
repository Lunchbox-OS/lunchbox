# 2026-06-28 — Android: force-stop helper + preboot (implementation)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Sketch: [2026-06-28 005 android-force-stop-helper-sketch.md](2026-06-28%20005%20android-force-stop-helper-sketch.md)

## Prompt

> keep going -- use `shepherd-waydroid`, lift `is_valid_android_package`, and
> make it so that the helper can do the preboot.

Implements the sketch with the three decisions made: dedicated
`shepherd-waydroid` group, a single shared validator, and a helper that also
performs the privileged step of preboot.

## What landed

### Shared validator — `crates/shepherd-util/src/android.rs`
- `is_valid_android_package` moved here (one source of truth). `shepherd-config`
  now calls `shepherd_util::is_valid_android_package`; the helper uses the same
  function, so the security-critical rule can't drift. Unit tests moved with it;
  config keeps one wiring test that an invalid package is rejected end-to-end.

### Privileged helper — `crates/shepherd-waydroid-helper` (new crate)
std-only but for `shepherd-util` (the validator). Two narrow, fixed actions,
invoked via `pkexec`:
- `force-stop --package <pkg>` → re-validates the package (the trust boundary),
  then `exec`s `waydroid shell am force-stop <pkg>` with a fixed argv, no shell.
- `preboot` → `exec`s `systemctl start waydroid-container` (the unit name is
  hardcoded; the subcommand takes no arguments, so the action can't be aimed
  elsewhere).
README documents the trust boundary.

### polkit — `dist/polkit/`
- `org.shepherd.waydroid.policy`: action `org.shepherd.waydroid.helper`, gated
  on the helper binary path (one action covers both subcommands, safe because
  the binary only does the two fixed things).
- `50-shepherd-waydroid.rules`: grants it password-less to the
  **`shepherd-waydroid`** group.

### Install / docs
- `scripts/lib/install.sh`: `install_waydroid` + a `waydroid` subcommand
  (mirrors `install_firewall`; creates the group, installs helper + polkit,
  reloads polkit). **Not** in `install all` — it's opt-in (needs Waydroid on the
  host).
- `scripts/integration-tests/setup-waydroid-dev.sh` (dev installer).
- `docs/INSTALL.md`: an "Android activities via Waydroid" section.

### Caller + preboot wiring — `crates/shepherd-host-linux/src/waydroid.rs`
- `force_stop` now runs `pkexec <helper> force-stop --package <pkg>` (path
  overridable via `SHEPHERD_WAYDROID_HELPER`). Still best-effort.
- New: `preboot_container` (pkexec `preboot`), `get_prop`/`set_prop`,
  `session_stop`, and `start_session_and_wait(timeout)` — spawns
  `waydroid session start`, scans its stdout for the ready marker with a
  timeout, and leaves the session running (tokio `Child` isn't kill-on-drop).
- `adapter.rs`: `configure_waydroid(multi_window, suspend_when_idle,
  boot_ready_timeout)` (primitives, to keep host-linux free of a
  shepherd-config dep, like `configure_steam`) + `preboot_waydroid()`:
  fire-and-forget task that starts the container (helper), starts the session,
  enforces `persist.waydroid.multi_windows` (set + one restart if needed), and
  enables `persist.waydroid.suspend`.
- `shepherdd/src/main.rs`: `configure_waydroid(...)` at init; at startup
  preboot iff `[service.waydroid] preboot` is set, else iff any Android entry
  exists.

## Verification

- `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all-targets` — all green (0 failures).
- **Helper actions validated live as root** on the box: `preboot` started
  `waydroid-container`; `force-stop` actually killed a launched Calculator
  (PID 1435 → gone), not just closing its window.
- **Install validated live**: `shepherd install waydroid --debug` placed the
  helper (0755 root) + polkit policy/rule, created the `shepherd-waydroid`
  group, reloaded polkit; `pkaction --action-id org.shepherd.waydroid.helper`
  shows the action with the correct `exec.path` annotation.
- Helper arg-parsing / validation smoke-tested directly (bad package, missing
  `--package`, unknown subcommand, extra preboot arg all rejected with exit 2).

### Not verifiable in this environment
- The runtime **pkexec → polkit → helper** grant (needs a real graphical login:
  the `shepherd-waydroid` group only takes effect after re-login, and there's no
  polkit agent in this headless session). The three links either side of it are
  each validated.
- The integrated `preboot_waydroid` orchestration under a live `shepherdd` in
  the kiosk (GNOME on this box, not the nested-Sway runtime). Its building
  blocks — container start, session start + ready-wait, prop get/set, restart —
  each match steps validated manually in Phase 0.

## Status

Issue #2 now has: scoping, a validated host spike, config/plumbing, a runtime
slice (launch/track/stop), and the privileged force-stop + preboot seam. The
remaining gap is a full end-to-end run inside the real kiosk runtime.
