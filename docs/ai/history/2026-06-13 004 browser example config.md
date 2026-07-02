# 2026-06-13 — Browser activity: example config + docs (step 5)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Design: [2026-05-01 002 web browser activity.md](2026-05-01%20002%20web%20browser%20activity.md)
Prior: [2026-06-13 003 browser profile management.md](2026-06-13%20003%20browser%20profile%20management.md)

## Prompt

> do step 5

The final step in the design's order: a school-mode `config.example.toml`
entry combining flatpak + browser + firewall, and a docs pass. Must pass
config validation.

## What landed

### `config.example.toml`

A new `## === Web browser ===` section with a `chrome-school` entry composing
all three layers:

- `kind = flatpak` / `com.google.Chrome`
- `[entries.browser]`: `profile_id`, `mode = "kiosk"`, `start_url`, a Google
  Workspace `url_allowlist`, empty `url_blocklist`, the lockdown switches, and
  `wipe_on_exit = false`, each with explanatory comments.
- `[entries.firewall]`: shown as a **commented-out** block.

**Why the firewall is commented out.** A default-deny IP firewall needs the
current Google/Workspace CIDRs, which drift and can't be hardcoded honestly.
The browser `url_allowlist` is the authoritative host control and works out of
the box; the firewall is coarse IP-layer defense-in-depth. Shipping it live
with no real ranges would silently block all web traffic — so it's documented
with a pointer to `https://www.gstatic.com/ipranges/goog.json` and left for the
operator to populate and enable. The active entry still demonstrates the
full structure of all three layers in one place.

The entry also sets a weekday availability window, `internet.required` with a
`generate_204` check, and 1h/2h run/daily limits, matching the surrounding
examples.

Validated with the in-tree tool:

```
$ cargo run -p shepherd-config --bin validate-config -- config.example.toml
✓ Configuration is valid
  ...
  - chrome-school [flatpak (com.google.Chrome)]: School
```

### Docs

The schema and host-side behavior were already documented alongside their
implementation: `crates/shepherd-config/README.md` (Browser section, step 2)
and `crates/shepherd-host-linux/README.md` (Browser policy + profile, steps
3-4). The top-level `README.md` is screenshot-driven and has no text-only slot
for an activity type without a capture, so it was left for a real on-device
screenshot later.

## Status of issue #10

Steps 1-5 of the design are implemented:

1. Generic `[entries.firewall]` (issue #4) — pre-existing.
2. `[entries.browser]` schema + validation + round-trip.
3. Managed-policy JSON + Chrome launch flags in the Linux host adapter.
4. Per-profile `--user-data-dir` + `wipe_on_exit` cleanup.
5. Example config + docs (this commit).

## Remaining (out of band for this environment)

- **Manual on-device verification.** The managed-policy path
  (`.../chromium/policies/managed/`) and user-data-dir path
  (`.../google-chrome/<profile_id>/`) are the design's documented locations,
  isolated in consts in `browser.rs`. They need confirming against a real
  Flatpak Chrome (does the policy take effect? does `--user-data-dir` land
  where we wipe?), then adjusting if wrong — the same manual-test pattern the
  firewall work used.
- **Follow-up ideas** noted in the design: periodic refresh of Google/MS IP
  ranges; a config-time warning for `wipe_on_exit` + shared `profile_id`; a
  config-time check that `[entries.browser]` pairs with a Chromium kind.
