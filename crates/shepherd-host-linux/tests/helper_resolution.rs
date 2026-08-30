//! The regression test for the `$PATH` hijack in issue #144.
//!
//! An integration test rather than a unit test, deliberately: it poisons the
//! process's `PATH`, and unit tests share one process with every other test in
//! the crate — several of which spawn `sleep` and `sh` by name. An integration
//! test file is its own binary, so the poisoning cannot reach them.

use std::os::unix::fs::PermissionsExt;

/// On a stock 26.04 + GDM host the kiosk user sets the session's environment by
/// writing `~/.pam_environment`, which GDM's PAM stack reads (`user_readenv=1`).
/// While shepherd resolved helpers through `$PATH`, that let any activity put
/// its own `systemd-run` in front of the real one — and since a helper is a
/// direct child of the daemon, the substitute would run **in the daemon's own
/// cgroup**, which the management socket accepts as `Admin`. It would also turn
/// the activity-isolation wrapper into a no-op, so nothing would fail loudly.
///
/// The fix is that `resolve` does not read the environment at all.
#[test]
fn a_poisoned_path_cannot_redirect_a_helper() {
    let decoy_dir = std::env::temp_dir().join("shepherd-poisoned-path-test");
    std::fs::create_dir_all(&decoy_dir).expect("create decoy dir");
    let planted = decoy_dir.join("systemd-run");
    std::fs::write(&planted, b"#!/bin/sh\nexit 0\n").expect("plant decoy");
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755))
        .expect("chmod decoy");

    // Exactly what an activity would arrange: its own directory, first.
    let poisoned = format!(
        "{}:{}",
        decoy_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: this test binary is single-threaded at this point and owns its
    // own process, which is why the test lives here rather than in the crate's
    // unit tests.
    unsafe { std::env::set_var("PATH", &poisoned) };

    let found = shepherd_host_linux::helpers::resolve("systemd-run");

    assert_ne!(
        found, planted,
        "a $PATH entry redirected a helper; an activity could put its own binary \
         in shepherd's cgroup (issue #144)"
    );
    assert!(
        found.is_absolute(),
        "resolved to {found:?}, a bare name $PATH would still get to interpret"
    );
    assert!(
        found.starts_with("/usr/") || found.starts_with("/bin") || found.starts_with("/snap/bin"),
        "resolved to {found:?}, outside the trusted directories"
    );

    let _ = std::fs::remove_file(&planted);
}

/// The environment must not be able to name a binary either — the same file
/// that sets `PATH` sets `SHEPHERD_*_BIN` and `SHEPHERD_FIREWALL_HELPER`.
#[test]
fn environment_overrides_are_ignored_unless_development_enables_them() {
    // SAFETY: as above.
    unsafe { std::env::set_var("SHEPHERD_FIREWALL_HELPER", "/tmp/evil-helper") };
    assert_ne!(
        shepherd_host_linux::firewall_helper_path(),
        "/tmp/evil-helper",
        "the environment named the privileged helper on a hardened daemon"
    );

    shepherd_host_linux::helpers::set_trust_environment(true);
    assert_eq!(
        shepherd_host_linux::firewall_helper_path(),
        "/tmp/evil-helper",
        "a development session should still be able to point at a built helper"
    );
    shepherd_host_linux::helpers::set_trust_environment(false);
    unsafe { std::env::remove_var("SHEPHERD_FIREWALL_HELPER") };
}
