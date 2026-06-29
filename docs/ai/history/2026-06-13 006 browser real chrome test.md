# 2026-06-13 — Browser activity: real-Chrome gated test (item 4)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Prior: [2026-06-13 005 browser automated tests.md](2026-06-13%20005%20browser%20automated%20tests.md)

## Prompt

> yes, implement item 4

Item 4 from the testability menu: a gated, self-skipping test that verifies the
two assumptions only real Chrome can confirm —
1. the managed-policy path is the one Flatpak Chrome actually reads (so the URL
   allow/blocklist is enforced), and
2. `--user-data-dir` lands where we later wipe.

## What landed

### `crates/shepherd-host-linux/src/browser.rs`

A new `#[ignore]` unit test,
`real_flatpak_chrome_enforces_policy_and_user_data_dir`. Placed in-crate (not
shepherd-e2e) because it needs the crate-private `MANAGED_POLICY_SUBDIR` /
`USER_DATA_SUBDIR` path logic and does **not** need the sway+shepherdd harness —
it talks to Chrome directly. Self-skips (prints `[SKIP]`, returns) when the
flatpak isn't installed, so a normal `cargo test` is unaffected.

Mechanism — network-free and non-intrusive:

- **HOME redirected to a tempdir.** `flatpak run` computes the per-app dir from
  `HOME`, so `HOME=<tmp>` puts both the managed-policy dir and the profile under
  the test root — the user's real `~/.var/app/com.google.Chrome` is never
  touched. `XDG_DATA_HOME` stays at the real `~/.local/share` so flatpak still
  finds the user-installed app (same trick as `firewall_real_flatpak.rs`).
- **Two loopback HTTP servers** each serve a unique marker. Only the *allow*
  origin is in `url_allowlist` (so `build_policy_json` adds the catch-all `"*"`
  blocklist). Both origins are live, which is what makes the result
  unambiguous:
  - allowed origin → `--dump-dom` contains its marker (policy let it through);
  - blocked origin → marker **absent** (policy interstitial). If the policy
    path were wrong, the blocked origin would load and its marker would appear
    — failing the test and pointing at the const.
- **Profile check**: asserts Chrome created `--user-data-dir` at our computed
  path, then `wipe_profile_dir` removes it.

The assertions name the offending const (`MANAGED_POLICY_SUBDIR` /
`USER_DATA_SUBDIR`) in their failure messages so a wrong path is actionable.

### `scripts/integration-tests/test-browser-flatpak.sh`

Orchestrator mirroring `test-firewall-flatpak.sh`: checks `flatpak` + `timeout`
+ that `com.google.Chrome` is installed (instructs `flatpak install -y flathub
com.google.Chrome` otherwise), then runs the `--ignored` test. `SHEPHERD_CHROME_FLATPAK`
overrides the app id (e.g. to test a Chromium flatpak).

## Status

Verified the **skip path** here (Chrome isn't installed on this host): the test
prints `[SKIP] ... flatpak 'com.google.Chrome' not installed` and passes; normal
`cargo test` reports it among the ignored. The **real assertions** were not run
here — they require the flatpak and a session with working flatpak/dbus, i.e.
the manual orchestrator on a configured host, exactly like the firewall flatpak
test.

Caveats noted in the test for whoever runs it first:
- Headless flags are `--headless=new --disable-gpu --no-first-run`; if Chrome
  won't start in a given environment, `--no-sandbox` may be needed.
- It needs a session bus for `flatpak run`; a bare headless CI may lack one.

## Validation

`cargo test -p shepherd-host-linux --lib` (35 pass, 2 ignored — the e2e-less
gated test plus the e2e one), the gated test's skip path exercised,
`cargo clippy`/`cargo fmt` clean.

## Issue #10 testability — done

- Pure logic: unit + golden snapshot.
- Shepherd wiring: adapter tests + e2e boot test (CI-runnable, no Chrome).
- Real Chrome: this gated test (manual, self-skipping).

The two path consts are now backed by an executable check instead of a comment.
