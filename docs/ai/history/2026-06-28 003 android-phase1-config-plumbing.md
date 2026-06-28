# 2026-06-28 — Android activity kind: Phase 1 (config + plumbing)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Scope: [2026-06-28 001 android-activity-kind-scoping.md](2026-06-28%20001%20android-activity-kind-scoping.md)
Phase 0: [2026-06-28 002 android-phase0-host-spike.md](2026-06-28%20002%20android-phase0-host-spike.md)

## Prompt

> write your findings into the history, commit, and go as far as you can.

Phase 1 from the scoping doc: land `EntryKind::Android` and the
`[service.waydroid]` schema — config-layer only, no host spawn wiring (that's
Phase 2). The kind parses and validates, but the Linux adapter does not yet
launch it and `linux_full()` does **not** advertise the capability, so a launch
is gracefully rejected as unsupported until Phase 2.

## What landed

A new `Android { package_name, args }` entry kind, mirroring the existing
external-app-manager kinds (Steam/Flatpak). `args` is reserved for future
intent extras and defaults to empty; there is deliberately **no `env`** field
(meaningless for a containerized Android app). Plus a service-level
`[service.waydroid]` block (Waydroid runs one global container shared by all
Android entries, so its knobs are service-level, not per-entry).

### `crates/shepherd-api/src/types.rs`
- `EntryKind::Android { package_name: String, args: Vec<String> }` and
  `EntryKindTag::Android` (placed after `Flatpak`, grouping the app-manager
  kinds; `Custom` stays last). `EntryKind::tag()` arm added.

### `crates/shepherd-config/src/schema.rs`
- `RawEntryKind::Android` (same shape).
- New `RawWaydroidConfig` (`preboot: Option<bool>`, `multi_window`,
  `suspend_when_idle`, `boot_ready_timeout_seconds`) and a
  `waydroid: Option<RawWaydroidConfig>` field on `RawServiceConfig`.
- `parse_android_entry` test.

### `crates/shepherd-config/src/validation.rs`
- `validate_entry` arm validates `package_name` via new
  `is_valid_android_package`: ≥2 dot-separated segments, each starting with an
  ASCII letter and otherwise `[A-Za-z0-9_]`. This is the security-relevant
  check — the value is spliced into `waydroid app launch <pkg>` /
  `am force-stop <pkg>`, so it must be free of whitespace/shell metacharacters.
- Accept + reject unit tests (incl. `com.app;rm -rf.x` rejected).

### `crates/shepherd-config/src/policy.rs`
- `convert_entry_kind` Android arm (raw → validated).
- New validated `WaydroidConfig` + `WaydroidConfig::from_raw` resolving
  defaults (`multi_window`/`suspend_when_idle` default true,
  `boot_ready_timeout` default 60 s; `preboot` stays `Option` because its
  default is dynamic — "preboot iff any Android entry exists", decided in
  Phase 2). Wired into `ServiceConfig` + its `Default`.

### `crates/shepherd-host-api/src/capabilities.rs`
- **Unchanged on purpose.** `linux_full()` does *not* list
  `EntryKindTag::Android` yet — Phase 1 keeps the kind unspawnable. Flipping
  this on is a Phase 2 step, together with the adapter wiring.

### `crates/shepherd-host-linux/src/adapter.rs`
- `spawn()` Android arm returns `HostError::UnsupportedKind` (keeps the
  exhaustive match compiling; the capability gate rejects launches earlier).

### UI / tooling
- `shepherd-launcher-ui/src/tile.rs`: fallback tile icon `"phone"` for
  `EntryKindTag::Android`.
- `shepherd-config/src/icon.rs`: Android joins the no-autodetect group (icons
  live in the container; entries should set `icon` explicitly).
- `validate-config` summary: `android (<package_name>)`.

### `config.example.toml`
- `[service.waydroid]` block and an `android-calculator` example entry
  (`com.android.calculator2`, the built-in LineageOS app validated in Phase 0).
  `validate-config` passes (15 entries) and prints
  `android (com.android.calculator2)`.

## Verification

- `cargo fmt --all` clean.
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test --all-targets` green (new: 1 parse test, 2 package-validator
  tests; no regressions). Pre-existing hardware-gated tests remain `ignored`.
- `validate-config config.example.toml` → valid.

## Next: Phase 2 (runtime vertical slice)

Per the scoping doc and Phase 0 findings: a `waydroid` host module (session
start + log-based readiness wait, prop set, `app launch` as the session user,
root `am force-stop`), the adapter `spawn()`/`stop()` Android path, window-watch
exit detection (design B1 — match the `waydroid.<pkg>` toplevel via
`sway::list_windows`, emit `Exited` on destroy), preboot wired off
`[service.waydroid]`, and finally advertising `EntryKindTag::Android` in
`linux_full()`. Target: launch the Calculator entry end-to-end on the dev box.
