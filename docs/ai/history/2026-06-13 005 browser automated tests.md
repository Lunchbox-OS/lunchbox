# 2026-06-13 — Browser activity: automated tests (0+1+2+3)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Prior: [2026-06-13 004 browser example config.md](2026-06-13%20004%20browser%20example%20config.md)

## Prompt

> Suggest some ways to make this automatically testable → "implement 0 + 1 + 2 + 3".

The four items from the suggestion menu:

0. Make the browser root injectable (test safety + the enabler).
1. Golden snapshot of the policy JSON.
2. Adapter wiring test with a fake Chrome (no real browser).
3. e2e boot test through the HTTP API.

## 0 — Injectable browser root

`write_managed_policy` / `user_data_dir` previously called
`dirs::home_dir()` directly, which meant any adapter/e2e test would write into
the developer's **real** `~/.var/app/com.google.Chrome/...`. Both now take a
`root: &Path`; `user_data_dir` is consequently infallible (returns `PathBuf`).

`LinuxHost` gained a `browser_root: PathBuf` field, resolved once via
`resolve_browser_root()`: honor `SHEPHERD_BROWSER_ROOT` if set, else
`dirs::home_dir()`. The env knob mirrors the existing `SHEPHERD_FIREWALL_HELPER`
pattern and is what the e2e test uses to redirect writes. In-process tests set
the field directly. The spawn block skips materialization (with a warning) if
the root is empty.

## 1 — Policy JSON golden snapshot

`policy_json_golden` asserts the entire managed-policy document for a
fully-locked spec, including the `URLBlocklist: ["*"]` injection. Any change to
the security-sensitive key/value mapping now fails loudly.

## 2 — Adapter wiring tests (CI-runnable, no Chrome)

Two `#[tokio::test]`s in `adapter.rs`, exploiting that materialization fires
for `process` kind too:

- `browser_spawn_writes_policy_and_appends_flags` — points `browser_root` at a
  tempdir, spawns a fake-chrome shell script that records its argv, and asserts
  the managed-policy JSON exists at the documented path **and** the recorded
  argv contains `--user-data-dir=<root>/.../google-chrome/school`, `--kiosk`,
  and the start URL.
- `browser_wipe_on_exit_removes_profile_dir` — pre-creates the profile dir,
  spawns `true` with `wipe_on_exit`, runs the real process monitor, and asserts
  the dir is gone after exit.

## 3 — e2e boot test

`crates/shepherd-e2e/tests/browser.rs` —
`browser_materializes_policy_flags_and_wipes_profile`, `#[ignore]` +
multi_thread, mirroring `firewall.rs`. Boots real sway + shepherdd, injects
`SHEPHERD_BROWSER_ROOT` via `shepherdd_env`, launches a `process`-kind browser
entry through `POST /api/v1/sessions`, and asserts all three behaviors through
the full stack: policy written, launch flags in the activity argv, and the
ephemeral profile wiped on exit. The fake chrome records argv then `sleep 1` so
the session is briefly live before the wipe.

**Ran green on a configured host** (`cargo test -p shepherd-e2e --test browser
-- --include-ignored`), after `cargo build -p shepherdd` (the harness runs
`target/debug/shepherdd`, so the daemon must be rebuilt — the first run failed
against a stale binary until rebuilt).

## What this does and doesn't cover

Covers the entire shepherd-side contract end to end without Chrome. Still does
**not** verify Chrome's own behavior — that the managed-policy path is the one
Flatpak Chrome reads, that the allowlist is enforced, that `--user-data-dir`
lands where we wipe. That's suggestion item 4 (a gated, self-skipping
real-Chrome test using `chrome --headless --dump-dom` on a blocked URL), left
for a follow-up; the path consts in `browser.rs` remain the single knobs.

## Validation

`cargo build --workspace`, `cargo test --workspace` (43 binaries; host-linux 35
+ 1 ignored e2e), the e2e test run live on this host, `cargo clippy --workspace
--all-targets`, and `cargo fmt --all` all clean.
