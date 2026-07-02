# 2026-06-13 — Browser activity: `[entries.browser]` config schema (step 2)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Design: [2026-05-01 002 web browser activity.md](2026-05-01%20002%20web%20browser%20activity.md)

## Prompt

> Following up on the notes from the firewall work, review and summarize what
> needs to be done for the browser activity type. → then: "Yes, do step 2".

"Step 2" is the second item in the design doc's suggested implementation
order: the `[entries.browser]` schema in `shepherd-config` — types,
validation, and the `RawEntry → Entry` round-trip. Step 1 (the generic
`[entries.firewall]` mechanism, issue #4) had already landed for
process/snap/flatpak; this branch (`u/albert/10/web-browser`) had no commits
yet.

## What landed

This is config-layer only. No host-side spawn wiring (that's step 3), no
profile management (step 4), no `config.example.toml` entry (step 5).

### `crates/shepherd-config/src/schema.rs`

- New `RawBrowserConfig`: `profile_id`, `mode` (string), `start_url`,
  `url_allowlist`, `url_blocklist`, `disable_dev_tools`/`disable_incognito`/
  `disable_extensions` (default `true` via the existing `default_true`),
  `wipe_on_exit` (default `false`). `mode` defaults to `"kiosk"`.
- New `browser: Option<RawBrowserConfig>` field on `RawEntry`, placed next to
  `firewall` and documented as a composition layer (no new `EntryKind`).
- Parse test `parse_browser_entry`.

### `crates/shepherd-config/src/policy.rs`

- New validated `BrowserPolicy` + `BrowserMode` enum (`Kiosk`/`App`/
  `Windowed`), mirroring how `FirewallPolicy` lives in the config crate
  (not `shepherd-api`).
- `convert_browser_config` trims strings and maps `mode` → `BrowserMode`
  (defaulting to `Kiosk` defensively; validation already rejects bad modes).
- `browser` field added to `Entry` and wired through `Entry::from_raw`.

### `crates/shepherd-config/src/validation.rs`

- `validate_browser` + helpers:
  - `validate_profile_id` — must be a single safe path segment (ASCII
    alphanumeric + `-`/`_`/`.`, not `.`/`..`). It becomes an on-disk
    directory name, so this is the security-relevant check.
  - `validate_http_url` — `start_url` must be `http(s)://<host>` (light check;
    Chrome is the real authority).
  - `validate_url_pattern` — reject empty / whitespace-bearing
    allowlist/blocklist patterns; full pattern semantics left to Chrome.
- Five new unit tests covering accept + each rejection path.

### Constructor churn

`policy::Entry` is built with field-by-field literals in ~11 test/engine
sites (engine.rs ×7, http/tests/api.rs ×2, shepherdd/tests/integration.rs ×2,
plus the two `RawEntry` literals in validation.rs tests). Each got
`browser: None,` — same mechanical pattern the `firewall` field followed.

### `crates/shepherd-config/README.md`

Added a `### Browser` section documenting the schema, defaults, the
`profile_id` persistence/safety rules, and the "Chrome enforces hostnames,
firewall is coarse IP defense-in-depth" split.

## Validation

`cargo build --workspace`, `cargo test -p shepherd-config -p shepherd-core -p
shepherd-http`, `cargo clippy -p shepherd-config --all-targets`, and
`cargo fmt --all` all clean.

## Next (per the design doc)

3. Policy-JSON materialization + Chrome flags in `shepherd-host-linux`.
4. Per-entry user-data-dir + `wipe_on_exit` cleanup in the adapter post-exit.
5. `config.example.toml` school-mode entry (must pass config validation).
