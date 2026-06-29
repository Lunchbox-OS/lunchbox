# 2026-06-13 — Browser activity: policy materialization + Chrome flags (step 3)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Design: [2026-05-01 002 web browser activity.md](2026-05-01%20002%20web%20browser%20activity.md)
Prior: [2026-06-13 001 browser config schema.md](2026-06-13%20001%20browser%20config%20schema.md)

## Prompt

> commit this then continue with step 3

"Step 3" in the design's implementation order: materialize the Chromium
managed-policy JSON in `shepherd-host-linux` before each spawn, including the
kiosk/app-mode flags as Chrome command-line args. Profile management
(`--user-data-dir`, `wipe_on_exit`) stays in step 4.

## What landed

### `shepherd-api` — shared `BrowserMode`

Added `BrowserMode` (`Kiosk`/`App`/`Windowed`) to `types.rs`, alongside
`InputCompatMode`. Both `shepherd-config`'s `BrowserPolicy` and the new
`shepherd-host-api` `BrowserSpec` use it, so there is one source of truth for
the mode vocabulary. This replaced the config-local `BrowserMode` introduced
in step 2 (config already depends on `shepherd-api`).

### `shepherd-host-api` — `BrowserSpec`

New `BrowserSpec` struct + `browser: Option<BrowserSpec>` field on
`SpawnOptions`. Mirrors `BrowserPolicy` (plus a `policy_id` = entry id used as
the JSON filename stem), kept separate from the config crate exactly like
`FirewallSpec` — the host-api layer must not depend on `shepherd-config`.

### `shepherd-host-linux/src/browser.rs` (new)

- `write_managed_policy(spec)` — writes
  `~/.var/app/com.google.Chrome/config/chromium/policies/managed/<policy_id>.json`
  (regenerated each spawn). `policy_id` is sanitized to a safe filename stem.
- `build_policy_json(spec)` — `URLAllowlist`/`URLBlocklist` +
  `DeveloperToolsAvailability=2` / `IncognitoModeAvailability=1` /
  `ExtensionInstallBlocklist=["*"]`, each emitted only when enabled.
- `chrome_flags(spec)` — `--kiosk <url>` / `--app=<url>` / bare `<url>`.
- 11 unit tests.

**Key behavior decision — authoritative allowlist.** A Chromium `URLAllowlist`
does *not* restrict on its own; it only carves exceptions out of
`URLBlocklist`. So when `url_allowlist` is non-empty, `build_policy_json`
injects a catch-all `"*"` into `URLBlocklist` (ahead of any user blocklist
entries, de-duped). Without this, a kiosk allowlist would be a no-op — the
opposite of the feature's intent. Documented in code + the crate README.

### `shepherd-host-linux/src/adapter.rs`

In `spawn`, after building the argv and before the firewall block: if
`options.browser` is set and the kind is `Flatpak`/`Process`, write the policy
JSON (log-and-continue on failure) and append `chrome_flags` to the argv. The
argv was rebound `mut`. Placing this *before* the firewall block means the
flags ride inside the process-kind firewall helper's wrapped argv too. A
browser policy on a non-Chromium kind is warned and ignored.

### Spawn sites

`shepherdd/src/main.rs` (IPC `Launch`) and
`shepherd-http/src/handlers/sessions.rs` (management API) both convert
`Entry.browser` (`BrowserPolicy`) → `SpawnOptions.browser` (`BrowserSpec`),
inlined the same way the existing `FirewallSpec` conversion is, and pass
`browser` in both the capture and non-capture `SpawnOptions` literals.

## Notes / deferred

- **Managed-policy path is unverified on a real device.** It's the path from
  the design doc, isolated in the `MANAGED_POLICY_SUBDIR` const with a comment
  — the single knob to change if on-device testing shows Flatpak Chrome reads
  managed policy elsewhere.
- **Step 3 launches the default profile.** `--user-data-dir` and `wipe_on_exit`
  are step 4, so `profile_id` has no on-disk effect yet (it's carried through
  `BrowserSpec` ready for step 4).
- **No config-time "browser requires flatpak" check.** Mismatches are caught
  at spawn (warn + ignore). A validation rule could be a small follow-up.

## Validation

`cargo build --workspace`, `cargo test` for shepherd-api/config/host-linux/
http/shepherdd, `cargo clippy` on the touched crates, and `cargo fmt --all`
all clean. New: 11 browser unit tests.

## Next

4. Per-entry `--user-data-dir` + `wipe_on_exit` cleanup in the adapter
   post-exit path.
5. `config.example.toml` school-mode entry (must pass config validation).
