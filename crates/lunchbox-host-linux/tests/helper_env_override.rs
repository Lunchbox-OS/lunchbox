//! Environment overrides must not name a binary on a device (issue #144).
//!
//! Its own test binary, not a case in `helper_resolution.rs`: the trust flag is
//! process-global and `resolve` consults it, so two tests that disagree about
//! it would decide each other's outcome by running order.

/// The environment must not be able to name a binary either — the same file
/// that sets `PATH` sets `LUNCHBOX_*_BIN` and `LUNCHBOX_FIREWALL_HELPER`.
#[test]
fn environment_overrides_are_ignored_unless_development_enables_them() {
    // SAFETY: as above.
    unsafe { std::env::set_var("LUNCHBOX_FIREWALL_HELPER", "/tmp/evil-helper") };
    assert_ne!(
        lunchbox_host_linux::firewall_helper_path(),
        "/tmp/evil-helper",
        "the environment named the privileged helper on a hardened daemon"
    );

    lunchbox_host_linux::helpers::set_trust_environment(true);
    assert_eq!(
        lunchbox_host_linux::firewall_helper_path(),
        "/tmp/evil-helper",
        "a development session should still be able to point at a built helper"
    );
    lunchbox_host_linux::helpers::set_trust_environment(false);
    unsafe { std::env::remove_var("LUNCHBOX_FIREWALL_HELPER") };
}
