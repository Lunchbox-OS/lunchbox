//! The wireless reader against a real NetworkManager (issue #194).
//!
//! Every test here is `#[ignore]`d. They need a live system bus, and the
//! interesting ones need a wireless adapter whose state a test may disturb —
//! which the developer's own adapter is not, because on a dev box it usually
//! carries the SSH session.
//!
//! ## Running them
//!
//! The safe subset needs nothing but NetworkManager:
//!
//! ```sh
//! cargo test -p lunchbox-host-linux --test wifi_networkmanager -- --ignored
//! ```
//!
//! [`classifies_real_beacons`] additionally needs a test bed of virtual
//! radios, because it asserts on specific networks. Build one with
//! `mac80211_hwsim`, which ships with the Ubuntu kernel:
//!
//! ```sh
//! sudo modprobe mac80211_hwsim radios=4      # wlan0 station, wlan1..3 APs
//! for i in wlan1 wlan2 wlan3; do sudo nmcli device set $i managed no; done
//! # Then run one wpa_supplicant per AP radio with mode=2 (hostapd is not
//! # packaged on 26.04), announcing:
//! #   wlan1  lunchbox-hwsimtest  key_mgmt=WPA-PSK  psk="correcthorse"
//! #   wlan2  lunchbox-wpa3       key_mgmt=SAE      ieee80211w=2
//! #   wlan3  lunchbox-open       key_mgmt=NONE
//! LUNCHBOX_WIFI_INTERFACE=wlan0 \
//!   cargo test -p lunchbox-host-linux --test wifi_networkmanager -- --ignored
//! ```
//!
//! Two traps worth knowing, both paid for once already:
//!
//! * **The APs take about 30 seconds to come up**, not 3. `wpa_supplicant` in
//!   AP mode scans first, and on a shared virtual medium those scans collide
//!   (`CTRL-EVENT-SCAN-FAILED ret=-16`) before `AP-ENABLED`. Poll `wpa_cli
//!   status` for `wpa_state=COMPLETED`; do not sleep and hope.
//! * **Never tear the bed down with `pkill -x wpa_supplicant`.** That kills
//!   NetworkManager's own supplicant and drops the real link. Match on the AP
//!   config filename and skip any process whose argv contains ` -u `.
//!
//! The full recipe, and what each measurement established, is in
//! `docs/ai/history/2026-09-21 004 wifi-against-real-networkmanager (#194).md`.

use lunchbox_api::aggregate_networks;
use lunchbox_host_api::WifiController;
use lunchbox_host_linux::LinuxWifiReader;

/// A read completes and contradicts itself nowhere.
///
/// Safe on any machine: it asserts only on internal consistency, so a CI box
/// with no radio passes by reporting no radio.
#[tokio::test]
#[ignore = "requires a running NetworkManager"]
async fn a_read_is_self_consistent() {
    let reader = LinuxWifiReader::new();
    let snapshot = reader.networks().await;

    if !snapshot.supported {
        assert!(
            snapshot.networks.is_empty(),
            "a device with no adapter cannot have heard anything"
        );
        return;
    }

    for network in &snapshot.networks {
        assert!(
            !network.ssid.is_empty(),
            "a hidden network cannot be offered in a picker"
        );
        assert!(network.signal_percent <= 100);
        assert!(
            network.bands_ghz.iter().all(|b| [2, 5, 6].contains(b)),
            "{:?} reported a band we do not name",
            network.bands_ghz
        );
    }
}

/// The scan age is computed against the boot clock, not the wall clock.
///
/// The bug this exists to catch: `LastScan` is `CLOCK_BOOTTIME` milliseconds,
/// so comparing it against `SystemTime::now` yields an age of tens of
/// thousands of seconds on a device that scanned a moment ago. An uptime's
/// worth of seconds is the signature of getting it wrong.
#[tokio::test]
#[ignore = "requires a running NetworkManager"]
async fn the_scan_age_is_not_an_uptime() {
    let reader = LinuxWifiReader::new();
    let snapshot = reader.networks().await;

    let Some(age) = snapshot.last_scan_age_s else {
        return; // never scanned since boot, which is a legitimate answer
    };
    let uptime = std::fs::read_to_string("/proc/uptime").expect("Linux");
    let uptime_s: f64 = uptime
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .expect("uptime parses");

    assert!(
        (age as f64) <= uptime_s + 1.0,
        "a scan cannot have happened {age}s ago on a device up for {uptime_s}s -- \
         this is what reading LastScan against the wall clock looks like"
    );
}

/// Saved profiles are wireless-only, carry no secret, and are distinguished by
/// id rather than by name.
#[tokio::test]
#[ignore = "requires a running NetworkManager"]
async fn saved_profiles_are_wireless_and_have_no_secrets() {
    let reader = LinuxWifiReader::new();
    let Ok(saved) = reader.saved().await else {
        return; // no adapter, or no permission to list: both legitimate
    };

    let ids: std::collections::HashSet<_> = saved.iter().map(|n| &n.id).collect();
    assert_eq!(
        ids.len(),
        saved.len(),
        "ids must be unique -- names are not: this dev device carries two \
         profiles for one printer's network"
    );

    // Nothing to assert about a password's absence at the type level; this is
    // the belt to that braces. If such a field is ever added, this fails
    // loudly. Matched with the `:` a derived `Debug` puts after a field name,
    // so that `security: WpaPsk` -- a kind, not a key -- does not trip it.
    let rendered = format!("{saved:?}").to_lowercase();
    for leak in ["psk:", "password:", "secret:", "key:"] {
        assert!(
            !rendered.contains(leak),
            "a saved profile grew a `{leak}` field: {rendered}"
        );
    }
}

/// The three security kinds, read off real beacons.
///
/// This is the test the unit tests cannot replace: [`WifiSecurity::from_ap_flags`]
/// is exercised there against flag words copied from a capture, but only a
/// live radio proves those are the words NetworkManager actually reports.
///
/// Needs the `mac80211_hwsim` bed described in the module docs, and
/// `LUNCHBOX_WIFI_INTERFACE` pointing at the station radio. Skips itself —
/// rather than failing — when the bed is not up, so `--ignored` on an ordinary
/// machine stays green.
#[tokio::test]
#[ignore = "requires the mac80211_hwsim test bed; see the module docs"]
async fn classifies_real_beacons() {
    use lunchbox_api::WifiSecurity;

    let reader = LinuxWifiReader::new();
    let snapshot = reader.networks().await;
    let (networks, _) = aggregate_networks(snapshot.networks);

    let find = |ssid: &str| networks.iter().find(|n| n.ssid == ssid).cloned();
    let Some(psk) = find("lunchbox-hwsimtest") else {
        eprintln!(
            "skipping: the hwsim test bed is not up. Networks seen: {:?}",
            networks.iter().map(|n| &n.ssid).collect::<Vec<_>>()
        );
        return;
    };

    assert_eq!(psk.security, WifiSecurity::WpaPsk);

    let sae = find("lunchbox-wpa3").expect("the SAE radio should be announcing");
    assert_eq!(sae.security, WifiSecurity::Sae);

    // The one that needs the PRIVACY bit: an open network sets no PRIVACY,
    // and PRIVACY with empty flag words would be WEP instead.
    let open = find("lunchbox-open").expect("the open radio should be announcing");
    assert_eq!(open.security, WifiSecurity::Open);
    assert!(!open.security.needs_password());
}
