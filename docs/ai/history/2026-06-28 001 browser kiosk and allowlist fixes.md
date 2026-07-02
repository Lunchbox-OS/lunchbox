# 2026-06-28 — Browser activity: kiosk presentation + URL-allowlist format fixes

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Prior: [2026-06-13 002 browser policy materialization.md](2026-06-13%20002%20browser%20policy%20materialization.md),
[2026-06-13 006 browser real chrome test.md](2026-06-13%20006%20browser%20real%20chrome%20test.md)
Design: [2026-05-01 002 web browser activity.md](2026-05-01%20002%20web%20browser%20activity.md)

## Prompt

> After trying out the just-built Web browser activity type via the example
> configuration, I'm noticing two things: `kiosk` was not respected and the
> browser chrome is present, and all of the URLs I tried, including those in the
> whitelist, were blocked by the browser

Two independent bugs, both in territory the existing tests didn't cover: the
e2e test only checks the daemon-side argv/policy *wiring* (no real Chrome), and
the gated `real_flatpak_chrome` test uses `Windowed` mode with a bare
`http://127.0.0.1:PORT` allowlist — so neither kiosk presentation nor glob URL
patterns were ever exercised against real Chrome.

## Root causes

### 1. All URLs blocked — wrong Chrome URL-filter format

The example (and the design-doc sketch it came from) used
`https://*.google.com/*`. Chrome's URLBlocklist/URLAllowlist filter format is
`[scheme://][.]host[:port][/path][@query]`
(<https://chromeenterprise.google/policies/url-blocking/>), where:

- there is **no** `*.` subdomain-wildcard host form — `*` is only valid as the
  *entire* host. A plain hostname already matches all its subdomains, so
  `google.com` matches `classroom.google.com`.
- the path is a **literal prefix match, not a glob**. A trailing `/*` requires
  the URL path to literally begin with `/*`, which never happens.

So every allowlist entry matched nothing, and `build_policy_json`'s authoritative
catch-all (`URLBlocklist: ["*"]`, added whenever the allowlist is non-empty)
blocked everything — including the start URL.

### 2. Kiosk not respected — sway force-disables fullscreen

`sway.conf` (the kiosk compositor config) intentionally denies client
fullscreen for every non-launcher window, to keep the time-remaining HUD's
exclusive zone visible:

```
for_window [app_id="^(?!shepherd-launcher$).*"] fullscreen disable
for_window [class=".*"] fullscreen disable
```

Chrome's `--kiosk` *requests* fullscreen and ties its toolbar-hiding to being
fullscreen. When sway denies the fullscreen, Chrome falls back to a normal
window **with** the toolbar — exactly the reported symptom.

(The HUD uses `Layer::Overlay`, which renders above even fullscreen windows, so
true fullscreen would *not* actually cover it — but the global rule is
conservative and disables fullscreen regardless.)

## Fixes

User chose (of three options) the chromeless-`--app` approach for kiosk.

- **`shepherd-host-linux/src/browser.rs` `chrome_flags`** — `Kiosk` and `App`
  now both emit `--app=<url>`, a toolbar-less window that sway maximizes into
  the area around the HUD. `--kiosk` is no longer used (it can't win against the
  compositor here). Updated unit tests (`kiosk_flags_use_app_window`, new
  `kiosk_mode_without_url_emits_no_flags`, `user_data_dir_flag_comes_first`).
- **`shepherd-e2e/tests/browser.rs`** — argv assertion now expects
  `--app=https://classroom.google.com` instead of separate `--kiosk` + URL.
- **`config.example.toml`** — corrected the `url_allowlist` to scheme+host form
  (`https://google.com`, …, no `*.` prefix, no `/*` path) and rewrote both the
  `mode` and allowlist comments to describe the real Chrome filter format and
  the kiosk==app behavior in this compositor.
- **`shepherd-config/src/schema.rs`** — `RawBrowserConfig.mode` doc updated to
  match (kiosk/app both chromeless `--app`; no `--kiosk`).

## Validation

`cargo test -p shepherd-host-linux --lib browser::` (18 pass, 1 ignored),
`cargo run -p shepherd-config --bin validate-config -- config.example.toml`
(clean), `cargo clippy -p shepherd-host-linux -p shepherd-config --all-targets
-D warnings` clean, `cargo test -p shepherd-e2e --no-run` compiles, `cargo fmt
--all`. The real-Chrome assertions (glob behavior, `--app` presentation) still
need on-device verification — they remain outside the gated test's coverage.

## Follow-up — load-time validation (done, same session)

After the user confirmed the runtime fixes work, `validate_url_pattern`
(`shepherd-config/src/validation.rs`) was hardened to reject the two
unsupported Chrome URL-filter forms that silently match nothing:

- a wildcard host that isn't the whole-host `*` (e.g. `*.google.com`), and
- a `*` in the path (e.g. a trailing `/*`), since the path is a literal prefix.

It parses `[scheme://][.]host[:port][/path][@query]`, allows the catch-all `*`
and `scheme://*`, and only inspects the host and the path (the `@query` tail may
legitimately end in a wildcard). New tests
(`test_validate_browser_rejects_unsupported_url_wildcards`, expanded
`test_validate_browser_accepts_valid`); the prior tests that used the now-invalid
`https://…/*` form were corrected. A malformed allowlist now fails config load
loudly instead of becoming a silent all-block.
