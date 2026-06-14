# 2026-06-13 — Browser activity: per-user policy injection (real-Chrome fix)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Prior: [2026-06-13 006 browser real chrome test.md](2026-06-13%20006%20browser%20real%20chrome%20test.md)

## Prompt

> Set up this environment for running the test and run it → (the gated test
> failed) → "I don't like that this has the potential to make Chrome unusable
> on multi-user systems. Since it's running as a Flatpak, see about ways to
> force it to use a different policy path." → "implement item 4".

## What the gated test caught

Installing `com.google.Chrome` and running the item-4 gated test immediately
falsified the design's central assumption. The Flathub Chrome launch wrapper
(`/app/bin/chrome`) **ignores the per-user config dir for policy**. It reads
managed policy only from the host's machine-wide `/etc/opt/chrome/policies/`
(via its `host-etc` permission → `/run/host/etc`, symlinked into the sandbox).

Verified directly:

| | committed assumption | reality |
|---|---|---|
| managed-policy path | `~/.var/app/.../chromium/policies/managed/` | **`/etc/opt/chrome/policies/managed/`** (root-owned, machine-wide) |
| `--user-data-dir` / profile / wipe | `~/.var/app/.../google-chrome/<profile>/` | ✅ correct (sandbox-reachable; a host `/tmp` path is not) |
| allowlist entries | bare patterns | should be scheme-qualified (`http://host:port`) |

Writing to `/etc/opt/chrome/policies/managed/` would have needed root **and**
hijacked Chrome for every user on the machine — which the user rejected.

## The fix: inject into the sandbox's own /etc

The wrapper does `mkdir -p /etc/opt/chrome/policies/managed` *inside the
sandbox* — an ephemeral, per-launch, writable filesystem. So instead of the
host `/etc`, launch Chrome through a shim that seeds the sandbox's `/etc` from
our per-user policy file and then execs the normal launcher:

```
flatpak run --command=bash --env=SHEPHERD_POLICY=<file> com.google.Chrome \
  -c 'mkdir -p /etc/opt/chrome/policies/managed;
      ln -sf "$SHEPHERD_POLICY" /etc/opt/chrome/policies/managed/shepherd.json;
      exec /app/bin/chrome "$@"' bash <chrome flags>
```

Per-user, per-launch, no root, host `/etc` never touched. Verified end-to-end.

## Code changes

- **`browser.rs`** — reworked around the injection:
  - policy file now lives at `…/config/shepherd-policies/<id>.json`
    (`write_policy_file`); `chrome_flatpak_argv` builds the shim launch;
    `is_supported_browser_flatpak` gates on `com.google.Chrome`.
  - Browser support is now **flatpak Chrome only** (the `process`-kind
    generalization was dropped — non-flatpak Chromium has the same root-owned
    `/etc` problem and was never actually functional).
  - `user_data_dir` / `wipe_profile_dir` unchanged (were already correct).
- **`adapter.rs`** — the browser block rebuilds the argv via
  `chrome_flatpak_argv` for the Chrome flatpak; other kinds warn + ignore.
- **Tests** — unit tests for `chrome_flatpak_argv` + the gate; the e2e test
  (`shepherd-e2e/tests/browser.rs`) now uses a **stub `flatpak`** to assert the
  full daemon wiring (policy written, injection argv, wipe) with no real
  Chrome; the gated real-Chrome test now drives the real injection.

## Two debugging findings (real Chrome)

1. **A single-threaded test HTTP server wedged headless Chrome.** Headless
   Chrome opens speculative preconnect sockets that send nothing; the test's
   blocking `read()` stalled on them. Fixed with thread-per-connection + a read
   timeout.
2. **`DeveloperToolsAvailability=2` breaks `--dump-dom`.** Headless Chrome
   drives itself over the DevTools protocol, so disabling DevTools cripples the
   headless probe. This is headless-only — it is correct and desirable for the
   real windowed/kiosk launch — so the gated test simply omits that one
   lockdown key while still exercising the others.

## Validation

- Gated real-Chrome test: **pass in 1.12s** — allowlisted origin renders,
  blocked origin hits "Your organization doesn't allow you to view this site",
  host `/etc/opt/chrome` stays absent, profile created + wiped.
- `cargo test --workspace` (43 binaries), unit tests (35 + 2 ignored), the
  stub-flatpak e2e, `cargo clippy --workspace --all-targets`, `cargo fmt` —
  all clean.

## Follow-ups

- The example config / schema are unchanged (only the host-side mechanism
  moved). The allowlist-needs-scheme nuance is worth a docs note for operators.
