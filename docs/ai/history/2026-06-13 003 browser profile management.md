# 2026-06-13 — Browser activity: profile management (step 4)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Design: [2026-05-01 002 web browser activity.md](2026-05-01%20002%20web%20browser%20activity.md)
Prior: [2026-06-13 002 browser policy materialization.md](2026-06-13%20002%20browser%20policy%20materialization.md)

## Prompt

> continue with step 4

"Step 4" in the design's order: per-entry user-data-dir + optional
`wipe_on_exit` cleanup in the Linux host adapter's post-exit path.

## What landed

### `shepherd-host-linux/src/browser.rs`

- `user_data_dir(spec)` / `user_data_dir_at(home, profile_id)` — resolve the
  per-profile dir
  `~/.var/app/com.google.Chrome/config/google-chrome/<profile_id>/`. The path
  string is identical inside and outside the flatpak sandbox (flatpak passes
  `~/.var/app/<id>` through unchanged), so it works for both flatpak and
  `process` Chromium. `profile_id` is run through the same `sanitize_filename`
  as `policy_id` for defense-in-depth.
- `chrome_flags` gained a `user_data_dir: Option<&Path>` parameter; when set it
  prepends `--user-data-dir=<path>` before the mode/URL flags.
- `wipe_profile_dir(dir)` — `remove_dir_all`, tolerant of a missing dir,
  logging other errors. Wiping happens in the adapter, never in Chrome (which
  can't be trusted to clear its own state under crashes).
- Tests: user-data-dir path/sanitization, the `--user-data-dir` flag ordering,
  and a tempdir-backed wipe test (recursive remove + missing-dir no-op). 14
  browser tests total.

### `shepherd-host-linux/src/adapter.rs`

- New `profile_wipes: Arc<Mutex<HashMap<u32, PathBuf>>>` field, keyed by the
  activity's pid (same shape as `sidecars`).
- In `spawn`, the browser block now resolves the user-data-dir, passes it to
  `chrome_flags`, and — when `wipe_on_exit` is set — records the dir against
  the new pid (alongside the sidecar registration).
- The process monitor wipes the recorded dir right after `reap_sidecars` when
  the pid exits. Keying on the monitored pid means that for flatpak the wipe
  waits until the `flatpak run`/Chrome instance is actually gone (a unique
  user-data-dir makes that pid the profile's primary), so we never delete a
  live profile. `remove` from the map makes the wipe fire exactly once whether
  the exit was natural or via `stop`.

## Why the monitor, not `stop`

`stop` kills the process but doesn't remove it from `processes`; the monitor
detects the exit, removes it, emits `Exited`, and reaps sidecars. Hooking the
wipe there covers both natural exits and explicit stops with one code path and
guarantees the process is fully dead first (`try_wait` returned `Some`).

## Caveats

- `wipe_on_exit` with a **shared** `profile_id` deletes the shared dir when the
  first such activity exits. That's a self-contradictory config (ephemeral +
  shared); not guarded here. A config-time warning could be a follow-up.
- The user-data-dir path shares the unverified-on-device caveat with the
  managed-policy path (step 3); both are isolated in consts.

## Validation

`cargo build --workspace`, `cargo test --workspace` (42 binaries pass; 14
browser unit tests), `cargo clippy --workspace --all-targets` (clean), and
`cargo fmt --all` all clean.

## Next

5. `config.example.toml` school-mode entry combining flatpak + browser +
   firewall, and a final docs pass — must pass `validate-config`.
