# 2026-06-28 — Design sketch: privileged Waydroid force-stop helper

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Phase 2: [2026-06-28 004 android-phase2-runtime-slice.md](2026-06-28%20004%20android-phase2-runtime-slice.md)

## Prompt

> sketch the force-stop helper.

## Problem

Phase 2 ends an Android session by closing the app's Wayland toplevel
(user-level, validated on the bench). That removes the app from screen but
leaves the Android process **cached** inside the container. Reclaiming it
needs `waydroid shell am force-stop <pkg>`, which requires **root** — and
`shepherdd` runs **unprivileged**. So `waydroid::force_stop` is currently
best-effort and silently no-ops off the kiosk.

This sketches a privileged seam to make force-stop work, modeled exactly on the
existing `shepherd-firewall-helper` (pkexec + polkit + a strict-validation
std-only helper binary). Nothing here is implemented yet.

## Trust boundary (the whole point)

A new root code path is security-sensitive, so state the model up front:

- **Caller:** `shepherdd`, as the unprivileged kiosk user, a member of a
  granted unix group.
- **Untrusted input:** the **package name** is the only variable. Even though
  config is operator-written, the helper must not trust its caller (a future
  caller, or a compromised/buggy shepherdd, must not get arbitrary root exec).
- **Privileged action:** exactly `waydroid shell am force-stop <pkg>` — nothing
  configurable but the (validated) package.

### Specific risks & mitigations

1. **Option / argument injection** (`pkg = "-X"` or with spaces/metachars).
   → The helper **re-validates** the package against the same strict rule as
   config (`^[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z][A-Za-z0-9_]*)+$`): ≥2 segments,
   each letter-initial, `[A-Za-z0-9_]` only. That forbids leading `-`,
   whitespace, `/`, `;`, `$`, quotes — everything dangerous. And the helper
   builds a **fixed argv with no shell**, so there is no interpolation even if
   validation regressed.
2. **Running arbitrary `am`/shell commands.** → `am force-stop` is hardcoded;
   only the package is passed.
3. **`waydroid shell` as root is a broad surface** (whole waydroid python + the
   container's `am`). This is inherent to using the supported interface; the
   trade is a much smaller helper than reimplementing `nsenter`/`lxc-attach` +
   `am` ourselves. We already trust the `waydroid` binary to manage the
   container. Documented, accepted.
4. **Caller authenticity.** polkit gates the action to one unix group
   (`shepherd-waydroid`), password-less, like the firewall rule. Optionally the
   helper can assert `$PKEXEC_UID` is the expected kiosk uid for defense in
   depth (the firewall helper does an analogous `--uid == $PKEXEC_UID` check).

### Alternatives considered (rejected)

- **`pkexec waydroid …` directly** via a polkit rule on `/usr/bin/waydroid`:
  grants root to *every* waydroid subcommand (incl. `shell` → arbitrary root
  command in the container). The helper narrows it to one validated action.
- **Run shepherdd as root:** violates the existing unprivileged design.
- **`nsenter`/`lxc-attach` + `am` instead of `waydroid shell`:** more fragile,
  bigger reimplementation; defer unless `waydroid shell` proves unsuitable.

## Components

### 1. New crate `crates/shepherd-waydroid-helper` (std-only, like the firewall one)

`src/main.rs` (sketch):

```rust
//! Privileged helper for the Android (Waydroid) activity kind.
//! Invoked by shepherdd via pkexec. One action: force-stop an app.
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

const HELPER_NAME: &str = "shepherd-waydroid-helper";

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{HELPER_NAME}: {}", msg.as_ref());
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("force-stop") => force_stop(args),
        Some(other) => die(format!("unknown subcommand '{other}'")),
        None => die("missing subcommand (expected 'force-stop')"),
    }
}

fn force_stop(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut package = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--package" => {
                package = Some(args.next().unwrap_or_else(|| die("--package needs value")))
            }
            other => die(format!("unexpected arg '{other}'")),
        }
    }
    let package = package.unwrap_or_else(|| die("--package is required"));
    if !is_valid_android_package(&package) {
        die(format!("invalid package name '{package}'"));
    }
    // Optional hardening: assert PKEXEC_UID is the expected kiosk uid.

    // Fixed argv, no shell. exec() replaces this process with waydroid;
    // it only returns on failure.
    let err = Command::new("waydroid")
        .args(["shell", "am", "force-stop"])
        .arg(&package)
        .exec();
    die(format!("failed to exec waydroid: {err}"));
}

/// Same rule as shepherd-config's validator — the security-critical check.
fn is_valid_android_package(pkg: &str) -> bool { /* ≥2 segments, letter-initial, [A-Za-z0-9_] */ }
```

Unit tests: the validator's accept/reject vectors (reuse config's, incl.
`com.app;rm -rf.x` rejected), and arg-parse rejects unknown args / missing
`--package`.

**Validation source of truth.** The rule lives in `shepherd-config`
(`is_valid_android_package`) today. To avoid a security-critical rule drifting
in two places, **lift it to `shepherd-util`** (already a shared, low-level dep)
and call it from both config and the helper. The firewall helper instead
*duplicates* its IP validation to stay zero-dep; either is defensible, but for
a security check I lean to one shared implementation + shared test vectors.

### 2. polkit action — `dist/polkit/org.shepherd.waydroid.policy`

```xml
<action id="org.shepherd.waydroid.force-stop">
  <description>Force-stop an Android app for shepherd-launcher</description>
  <message>Authentication is required to stop an Android app.</message>
  <defaults>
    <allow_any>auth_admin_keep</allow_any>
    <allow_inactive>auth_admin_keep</allow_inactive>
    <allow_active>auth_admin_keep</allow_active>
  </defaults>
  <annotate key="org.freedesktop.policykit.exec.path">/usr/libexec/shepherd-waydroid-helper</annotate>
  <annotate key="org.freedesktop.policykit.exec.allow_gui">false</annotate>
</action>
```

(Action gates the binary as a whole, regardless of argv — same as the firewall
policy. Safe because the binary itself only does one validated thing.)

### 3. polkit rule — `dist/polkit/50-shepherd-waydroid.rules`

```js
polkit.addRule(function(action, subject) {
    if (action.id === "org.shepherd.waydroid.force-stop" &&
        subject.isInGroup("shepherd-waydroid")) {
        return polkit.Result.YES;
    }
});
```

A **dedicated** group (`shepherd-waydroid`), separate from `shepherd-firewall`,
keeps least privilege: a kiosk that uses Android but not the firewall (or vice
versa) grants only what it needs.

### 4. Caller side — `crates/shepherd-host-linux/src/waydroid.rs`

Replace the direct invocation with the pkexec helper (path overridable for dev,
mirroring `SHEPHERD_FIREWALL_HELPER`):

```rust
const DEFAULT_WAYDROID_HELPER_PATH: &str = "/usr/libexec/shepherd-waydroid-helper";
fn waydroid_helper_path() -> String {
    std::env::var("SHEPHERD_WAYDROID_HELPER")
        .unwrap_or_else(|_| DEFAULT_WAYDROID_HELPER_PATH.to_string())
}

/// Best-effort: reclaim the cached Android process via the privileged helper.
pub async fn force_stop(package: &str) {
    let result = Command::new("pkexec")
        .arg(waydroid_helper_path())
        .args(["force-stop", "--package", package])
        .status()
        .await;
    match result {
        Ok(s) if s.success() => debug!(package, "force-stopped Android app"),
        Ok(s) => debug!(package, %s, "force-stop helper did not succeed (polkit denied / not installed?)"),
        Err(e) => warn!(package, error = %e, "failed to invoke force-stop helper"),
    }
}
```

Semantics stay **best-effort** (returns `()`): the window close already ended
the session; a missing helper / denied polkit just means the cached process
lingers until the container's idle-suspend reclaims it. So Android keeps working
on a box without the helper installed — force-stop is a clean-up enhancement,
not a hard dependency. (No change to `adapter.rs`; it already calls
`waydroid::force_stop`.)

### 5. Install / CI / docs

- Workspace member + CI builds `shepherd-waydroid-helper`.
- Install (extend `shepherd install` and add a dev script à la
  `scripts/integration-tests/setup-firewall-dev.sh`):
  - helper → `/usr/libexec/shepherd-waydroid-helper` (root:root, 0755)
  - policy → `/usr/share/polkit-1/actions/org.shepherd.waydroid.policy`
  - rules → `/etc/polkit-1/rules.d/50-shepherd-waydroid.rules`
  - `groupadd --system shepherd-waydroid` + add the kiosk user (re-login needed)
- `docs/INSTALL.md`: the group-setup step (mirror the firewall section).
- Optional integration test `scripts/integration-tests/test-waydroid-force-stop.sh`
  (needs waydroid + group + polkit), like `test-firewall-snap.sh`.

## Open questions for review

1. **Group strategy:** dedicated `shepherd-waydroid` (recommended) vs. a single
   shared `shepherd` group across helpers?
2. **Validation sharing:** lift `is_valid_android_package` into `shepherd-util`
   (recommended, one source of truth) vs. duplicate-with-tests like the
   firewall helper's IP validation?
3. **pkexec env:** pkexec strips env; confirm root `waydroid`/`lxc-attach`
   resolve under pkexec's minimal PATH (they live in `/usr/bin`/`/usr/sbin`).
   The firewall helper sidesteps this by passing env explicitly; force-stop
   shouldn't need app env, but verify on the bench.
4. **`$PKEXEC_UID` assertion:** worth the extra defense-in-depth, or is the
   group-gated polkit rule + strict package validation enough?
5. **Scope creep:** keep the helper to `force-stop` only, or design it now to
   also host a future privileged `preboot`/`set-prop` (e.g. starting
   `waydroid-container` needs root too)? Recommend keeping it single-purpose
   until preboot's design firms up.
