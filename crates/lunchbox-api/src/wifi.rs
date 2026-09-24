//! Choosing a wireless network from the management UIs (issue #194).
//!
//! The change side of [`crate::network`], which reads. A parent whose device
//! has no network reaches for the companion over BLE — the one transport that
//! still works when there is nothing to route — picks an SSID, types a key,
//! and watches the status page go green.
//!
//! Three things shape every type here.
//!
//! **No call waits for the network.** Association plus DHCP took 3 to 45
//! seconds across the measurements in
//! `docs/ai/history/2026-09-21 004 wifi-against-real-networkmanager (#194).md`,
//! and the companion's RPC timeout is 15. So a join returns as soon as
//! NetworkManager has accepted it, and the outcome arrives later, on a
//! [`WifiScanView`] the UI is already polling for its list.
//!
//! **A secret only ever travels inwards.** [`WifiJoinRequest`] carries a
//! password; nothing here carries one back. [`SavedWifiNetwork`] deliberately
//! has no field for it, and the `Debug` written by hand below is what keeps
//! the one that does out of logs.
//!
//! **A failure says which failure.** "Could not connect" is the message that
//! makes a parent retype a password that was always right. The reasons in
//! [`WifiJoinFailure`] are the ones NetworkManager actually distinguishes,
//! measured rather than guessed.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Upper bound on how many networks travel on one scan.
///
/// This crosses BLE, where a frame caps at 16 KiB
/// (`lunchbox_ble::protocol::MAX_FRAME_BYTES`). An entry is about 150 bytes of
/// JSON, so 40 is roughly 6 KiB — room to spare, and more distinct SSIDs than
/// a home has. The overflow is reported through [`WifiScanView::truncated`]
/// rather than silently dropped, the same bargain [`crate::DiagnosticSet`]
/// makes.
pub const MAX_WIFI_NETWORKS: usize = 40;

/// Longest SSID 802.11 allows: 32 octets.
pub const MAX_SSID_BYTES: usize = 32;

/// WPA-PSK passphrase bounds, from IEEE 802.11i. Either 8–63 printable ASCII
/// characters, or exactly 64 hex digits (the raw PMK).
pub const MIN_PSK_CHARS: usize = 8;
pub const MAX_PSK_CHARS: usize = 63;
pub const PMK_HEX_CHARS: usize = 64;

/// How a network is protected.
///
/// Derived from an access point's beacon flags, which is the only thing a scan
/// can tell us. The mapping is measured against real beacons — see
/// [`WifiSecurity::from_ap_flags`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WifiSecurity {
    /// No protection at all. Joinable with no key.
    Open,
    /// Enhanced Open (OWE). Encrypted, still no key to type.
    Owe,
    /// WPA/WPA2 Personal. Also the right choice for a WPA2/WPA3 transition
    /// access point, because every card that can see one can join it this way.
    WpaPsk,
    /// WPA3 Personal (SAE).
    Sae,
    /// 802.1X. Recognised so it can be shown as unsupported; see
    /// [`WifiSecurity::joinable`].
    Enterprise,
    /// WEP. Recognised for the same reason, and it is not coming back.
    Wep,
}

impl WifiSecurity {
    /// Whether this device can join such a network from these UIs.
    ///
    /// Enterprise needs identity, certificates and phase-2 auth — a form of
    /// its own, deliberately out of scope for v1 — and WEP is long broken.
    /// Both are still *shown*, marked unsupported, because a network missing
    /// from the list looks like a device that cannot see it. Admin mode
    /// (#154) is the way in for both.
    pub fn joinable(self) -> bool {
        !matches!(self, Self::Enterprise | Self::Wep)
    }

    /// Whether joining needs a password.
    pub fn needs_password(self) -> bool {
        matches!(self, Self::WpaPsk | Self::Sae)
    }

    /// NetworkManager's `key-mgmt` for a profile of this kind, or `None` for
    /// the kinds we refuse to write.
    pub fn key_mgmt(self) -> Option<&'static str> {
        match self {
            Self::Open => None,
            Self::Owe => Some("owe"),
            Self::WpaPsk => Some("wpa-psk"),
            Self::Sae => Some("sae"),
            Self::Enterprise | Self::Wep => None,
        }
    }

    /// Classify an access point from its NetworkManager beacon flags.
    ///
    /// `flags` is `NM80211ApFlags`; `wpa` and `rsn` are
    /// `NM80211ApSecurityFlags` from the WPA and RSN information elements.
    ///
    /// The order matters. A transition access point advertises PSK *and* SAE
    /// in one RSN element, and gets [`WifiSecurity::WpaPsk`] because that is
    /// the mode every client can associate with; SAE is chosen only when it is
    /// the sole option. 802.1X outranks both, because a network that wants a
    /// certificate cannot be joined with a passphrase whatever else it offers.
    ///
    /// The last arm is the subtle one, and it is why `flags` is a parameter at
    /// all: an open network carries **no** `PRIVACY` bit, so `PRIVACY` with
    /// both flag words empty is precisely WEP. Measured — a genuinely open
    /// access point reported `Flags=0x0002` (WPS only), while WPA2 and SAE
    /// ones reported `0x0003`.
    pub fn from_ap_flags(flags: u32, wpa: u32, rsn: u32) -> Self {
        const PRIVACY: u32 = 0x1;
        const KEY_MGMT_PSK: u32 = 0x100;
        const KEY_MGMT_802_1X: u32 = 0x200;
        const KEY_MGMT_SAE: u32 = 0x400;
        const KEY_MGMT_OWE: u32 = 0x800;
        const KEY_MGMT_OWE_TM: u32 = 0x1000;
        const KEY_MGMT_EAP_SUITE_B_192: u32 = 0x2000;

        let both = wpa | rsn;
        if both & (KEY_MGMT_802_1X | KEY_MGMT_EAP_SUITE_B_192) != 0 {
            Self::Enterprise
        } else if both & KEY_MGMT_PSK != 0 {
            Self::WpaPsk
        } else if both & KEY_MGMT_SAE != 0 {
            Self::Sae
        } else if both & (KEY_MGMT_OWE | KEY_MGMT_OWE_TM) != 0 {
            Self::Owe
        } else if flags & PRIVACY != 0 {
            Self::Wep
        } else {
            Self::Open
        }
    }
}

/// One network in a scan, aggregated across every access point announcing it.
///
/// A home mesh puts the same SSID on three radios in two bands; a picker that
/// listed each would ask a parent to choose between three identical rows. So
/// entries are merged by (name, security) and the best signal wins.
///
/// No BSSID, matching #182's stance on MAC addresses: it is not a fact anybody
/// picking a network needs, and it identifies hardware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WifiNetwork {
    /// The network's name. Always valid UTF-8 here: an SSID is up to 32
    /// arbitrary octets, and one that is not text cannot be shown in a picker
    /// or typed into the manual form, so it is left out of the scan entirely.
    pub ssid: String,
    pub security: WifiSecurity,
    /// Signal quality 0–100, the strongest among the access points announcing
    /// this network.
    pub signal_percent: u8,
    /// Which bands it was heard on, in GHz, ascending — `[2]`, `[5]`, or
    /// `[2, 5]` for a network on both.
    pub bands_ghz: Vec<u8>,
    /// Whether a saved profile already exists for this network.
    pub saved: bool,
    /// Whether this is the network the device is on right now.
    pub active: bool,
}

/// A saved profile, as a list of known networks shows it.
///
/// **Never carries a secret.** There is no field for one, and no method
/// returns one: a stored password is write-only from every management UI. A
/// parent who has forgotten theirs re-types it; the device will not read it
/// back to them, because the same call would read it back to anything else
/// that could reach the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SavedWifiNetwork {
    /// Stable handle for [`connect`](WifiJoinRequest) and forget.
    ///
    /// NetworkManager's connection UUID, and not the SSID, because an SSID is
    /// not unique: this dev device carries two profiles for one printer's
    /// network, written by GNOME months apart with different security.
    pub id: String,
    pub ssid: String,
    pub security: WifiSecurity,
    /// Saved for a network that does not broadcast its name.
    pub hidden: bool,
    /// Whether NetworkManager may join this on its own.
    pub autoconnect: bool,
    /// Whether this profile is the active one.
    pub active: bool,
}

/// Everything one poll of the wireless state returns.
///
/// Scan list and join progress arrive together deliberately. The UI is already
/// polling this to refresh signal strengths while a parent looks at the list;
/// making the join's outcome ride along means no second timer, no second
/// endpoint, and no window where the list has updated but the result has not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WifiScanView {
    /// Whether this device has a wireless adapter at all. Everything below is
    /// empty when it does not, and a UI says so rather than showing an empty
    /// list that looks like a failed scan.
    pub supported: bool,
    /// Whether the radio is on. Reported, never changed — toggling it is out
    /// of scope, and a device switched off at the rfkill has to be fixed where
    /// it is.
    pub radio_enabled: bool,
    /// Networks in range, strongest first, capped at [`MAX_WIFI_NETWORKS`].
    pub networks: Vec<WifiNetwork>,
    /// Whether [`MAX_WIFI_NETWORKS`] hid anything.
    #[serde(default)]
    pub truncated: bool,
    /// How long ago the last scan completed. `None` when no scan has run since
    /// boot.
    ///
    /// Seconds, and derived on the host, because NetworkManager reports this
    /// as a `CLOCK_BOOTTIME` reading that means nothing on the phone reading
    /// it.
    pub last_scan_age_s: Option<u64>,
    /// What the most recent join is doing. [`WifiJoinState::Idle`] when none
    /// has been asked for since the daemon started.
    pub join: WifiJoinState,
    /// Whether this device may write a profile at all.
    ///
    /// False on a device whose custodian holds no NetworkManager grant. The
    /// UIs use it to disable the forms up front and point at the remedy,
    /// rather than letting a parent type a password into a box that was never
    /// going to work. The matching Health diagnostic says the same thing in
    /// the place people look for problems.
    pub can_configure: bool,
}

/// Why a join ended without a network.
///
/// Every variant is a distinct thing to *do* about it, which is the only
/// reason to distinguish them. Measured against NetworkManager 1.54.3 —
/// association failures surface on the device's `StateChanged`, never on the
/// active connection, which reports a generic disconnect for all of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WifiJoinFailureKind {
    /// The key was refused. NetworkManager reason 7, `no-secrets`.
    ///
    /// With no secret agent in the kiosk session there is nothing to re-prompt,
    /// so a wrong key fails instead of hanging — which is what makes this
    /// reportable at all.
    WrongPassword,
    /// No access point with that name answered. Reason 53, `ssid-not-found`.
    /// Out of range, switched off, or — for a network that does not broadcast
    /// — saved without `hidden` set.
    NotFound,
    /// Associated, and then no address. Reason 5, `ip-config-unavailable`.
    ///
    /// The key was right and the radio link came up; DHCP did not answer.
    /// Distinct from [`Self::WrongPassword`] because the thing to check is the
    /// router, not the key — and a parent told "wrong password" here will
    /// retype a correct one until they give up.
    NoAddress,
    /// The device may not write a profile: no polkit grant, and no custodian
    /// to borrow one from.
    NotAuthorized,
    /// It was refused before NetworkManager saw it.
    Rejected,
    /// Anything else. `detail` names it as NetworkManager named it, so a log
    /// is useful even for a case nobody anticipated.
    Other,
}

/// Why a join ended without a network, and — where there is one — the
/// backend's own word for it.
///
/// A kind plus an optional detail rather than an enum carrying payloads. The
/// payload-carrying shape has no Kotlin equivalent the generator can render,
/// and this one is easier to switch on in both UIs anyway: a UI maps `kind` to
/// a sentence a parent can act on, and shows `detail` only where it has
/// nothing better to say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WifiJoinFailure {
    pub kind: WifiJoinFailureKind,
    /// The backend's own description, for
    /// [`WifiJoinFailureKind::Rejected`] and
    /// [`WifiJoinFailureKind::Other`]. `None` for the kinds whose meaning is
    /// already in the kind.
    pub detail: Option<String>,
}

impl WifiJoinFailure {
    /// A failure whose kind says everything.
    pub fn of(kind: WifiJoinFailureKind) -> Self {
        Self { kind, detail: None }
    }

    /// A failure that needs the backend's own words.
    pub fn detailed(kind: WifiJoinFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: Some(detail.into()),
        }
    }
}

/// What the most recent join is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WifiJoinState {
    /// None has been asked for since the daemon started.
    Idle,
    /// Asked for, and not yet settled.
    Connecting { ssid: String },
    /// On the network.
    Connected { ssid: String },
    /// Over, without a network.
    Failed {
        ssid: String,
        reason: WifiJoinFailure,
    },
}

impl WifiJoinState {
    /// Whether this is still going. A UI keeps polling while it is.
    pub fn in_progress(&self) -> bool {
        matches!(self, Self::Connecting { .. })
    }

    /// This state, or [`Self::Idle`] when the radio says it is out of date.
    ///
    /// A join's outcome is recorded once, when it settles, and the radio moves
    /// on without it: NetworkManager autoconnects, a parent disconnects from
    /// the host, a saved network is forgotten. Reported as it stood, a settled
    /// state went on saying "no network called X answered" above a list with
    /// the device connected to X -- seen on a device, minutes after the
    /// failure. So a settled state the live list contradicts is dropped:
    ///
    /// * `Failed` for the network the device is now on, and
    /// * `Connected` to a network the device is no longer on.
    ///
    /// Anything else stands, including a failure for one network while the
    /// device is on another -- that is still news. `Connecting` is left
    /// alone; it settles on its own.
    ///
    /// `networks` must be a live read. A backend that could not read the
    /// radio returns nothing, and "nothing is active" would then clear a
    /// perfectly good `Connected`, so the caller skips this for such a read.
    pub fn reconciled(self, networks: &[WifiNetwork]) -> Self {
        let on = |ssid: &str| networks.iter().any(|n| n.active && n.ssid == ssid);
        match &self {
            Self::Failed { ssid, .. } if on(ssid) => Self::Idle,
            Self::Connected { ssid } if !on(ssid) => Self::Idle,
            _ => self,
        }
    }

    /// The network this is about, for a UI that shows one line of status.
    pub fn ssid(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Connecting { ssid } | Self::Connected { ssid } | Self::Failed { ssid, .. } => {
                Some(ssid)
            }
        }
    }
}

/// Save a network, and maybe join it now.
///
/// One request for both because they differ by one bool and every other field
/// is shared — and because "remember this" and "get on it" are the same act
/// from a parent's side, distinguished only by where they are standing.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WifiJoinRequest {
    pub ssid: String,
    pub security: WifiSecurity,
    /// The key. `None` for [`WifiSecurity::Open`] and [`WifiSecurity::Owe`],
    /// which have none.
    pub password: Option<String>,
    /// Whether this network does not broadcast its name. Set from the manual
    /// form; a network picked from a scan was broadcasting by definition.
    #[serde(default)]
    pub hidden: bool,
    /// `true` joins now. `false` only remembers it — what the web leads with,
    /// because joining from a browser can cut off the browser.
    pub connect: bool,
}

/// Hand-written so a password cannot reach a log through a derived `Debug`.
///
/// Every other type here derives it. This one holds the single secret in the
/// whole API, and a `#[derive(Debug)]` on it would put that secret one
/// `tracing::debug!("{req:?}")` away from disk — a line nobody would look
/// twice at in review.
impl fmt::Debug for WifiJoinRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WifiJoinRequest")
            .field("ssid", &self.ssid)
            .field("security", &self.security)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("hidden", &self.hidden)
            .field("connect", &self.connect)
            .finish()
    }
}

/// Why a request was refused before NetworkManager ever saw it.
///
/// Validation happens on the daemon, not in either UI, because there are two
/// UIs and a third caller is a `curl`. A rejected request is reported as a
/// typed error rather than attempted and left to fail obscurely twenty seconds
/// later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiRequestError {
    /// Empty, or longer than [`MAX_SSID_BYTES`] octets.
    SsidLength,
    /// A network that needs a key was sent without one.
    PasswordMissing,
    /// A key was sent for a network that has none. Refused rather than
    /// dropped: it means the caller believes something false about the
    /// network, and joining anyway would confirm it.
    PasswordNotAllowed,
    /// Not 8–63 characters, and not 64 hex digits.
    PskLength,
    /// A passphrase with something other than printable ASCII in it. 802.11i
    /// allows no more, and a key with a smart quote in it — which is what a
    /// phone keyboard produces — fails at association with a reason that
    /// looks exactly like a wrong password.
    PskCharacters,
    /// Empty SAE password.
    SaePasswordEmpty,
    /// Enterprise or WEP, neither of which these UIs write.
    SecurityUnsupported,
}

impl WifiRequestError {
    /// A sentence for a person, not a code for a log.
    pub fn message(self) -> &'static str {
        match self {
            Self::SsidLength => "A network name must be 1 to 32 characters.",
            Self::PasswordMissing => "This network needs a password.",
            Self::PasswordNotAllowed => "This network does not take a password.",
            Self::PskLength => {
                "A Wi-Fi password must be 8 to 63 characters, or exactly 64 hexadecimal digits."
            }
            Self::PskCharacters => {
                "A Wi-Fi password may only contain ordinary keyboard characters."
            }
            Self::SaePasswordEmpty => "This network needs a password.",
            Self::SecurityUnsupported => {
                "This device cannot join that kind of network. Use admin mode to set it up."
            }
        }
    }
}

impl fmt::Display for WifiRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for WifiRequestError {}

impl WifiJoinRequest {
    /// Check this request before anything acts on it.
    ///
    /// Bounds come from IEEE 802.11i rather than from what NetworkManager
    /// happens to accept, so the message a parent reads is about their network
    /// and not about our plumbing.
    pub fn validate(&self) -> Result<(), WifiRequestError> {
        let ssid_len = self.ssid.len();
        if ssid_len == 0 || ssid_len > MAX_SSID_BYTES {
            return Err(WifiRequestError::SsidLength);
        }
        if !self.security.joinable() {
            return Err(WifiRequestError::SecurityUnsupported);
        }

        match (self.security.needs_password(), self.password.as_deref()) {
            (true, None) => return Err(WifiRequestError::PasswordMissing),
            (false, Some(_)) => return Err(WifiRequestError::PasswordNotAllowed),
            (false, None) => return Ok(()),
            (true, Some(_)) => {}
        }

        let password = self.password.as_deref().unwrap_or_default();
        match self.security {
            WifiSecurity::WpaPsk => validate_psk(password),
            WifiSecurity::Sae => {
                // SAE takes any non-empty password: it is not bound by the
                // PSK's 8..=63 rule, and WPA3 is the reason a short or
                // non-ASCII passphrase is legal at all.
                if password.is_empty() {
                    Err(WifiRequestError::SaePasswordEmpty)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
}

/// 8–63 printable ASCII, or exactly 64 hex digits.
fn validate_psk(password: &str) -> Result<(), WifiRequestError> {
    if password.len() == PMK_HEX_CHARS && password.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(());
    }
    if password.len() < MIN_PSK_CHARS || password.len() > MAX_PSK_CHARS {
        return Err(WifiRequestError::PskLength);
    }
    // 802.11i says each character is ASCII 32..=126. A phone keyboard's curly
    // apostrophe is not, and the association failure it causes is reported as
    // a wrong password — so it is caught here, where we can say what is wrong.
    if !password.chars().all(|c| (' '..='~').contains(&c)) {
        return Err(WifiRequestError::PskCharacters);
    }
    Ok(())
}

/// Merge access points into one entry per (name, security).
///
/// Takes what a scan saw and returns what a picker shows: strongest signal
/// wins, bands accumulate, and the list is ordered by signal so the network a
/// parent is standing next to is at the top. Active networks sort first
/// regardless, because "the one you are on" is the row people look for.
///
/// Lives here rather than in the host backend so both the real NetworkManager
/// reader and a test double produce the same list from the same beacons.
pub fn aggregate_networks(mut seen: Vec<WifiNetwork>) -> (Vec<WifiNetwork>, bool) {
    let mut merged: Vec<WifiNetwork> = Vec::new();
    for network in seen.drain(..) {
        match merged
            .iter_mut()
            .find(|m| m.ssid == network.ssid && m.security == network.security)
        {
            Some(existing) => {
                existing.signal_percent = existing.signal_percent.max(network.signal_percent);
                existing.saved |= network.saved;
                existing.active |= network.active;
                for band in network.bands_ghz {
                    if !existing.bands_ghz.contains(&band) {
                        existing.bands_ghz.push(band);
                    }
                }
                existing.bands_ghz.sort_unstable();
            }
            None => merged.push(network),
        }
    }

    merged.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then(b.signal_percent.cmp(&a.signal_percent))
            .then(a.ssid.cmp(&b.ssid))
    });

    let truncated = merged.len() > MAX_WIFI_NETWORKS;
    merged.truncate(MAX_WIFI_NETWORKS);
    (merged, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag words are the ones measured off real beacons on 1.54.3; see
    /// the history note. `0x0188` and `0x0488` are verbatim from that capture.
    #[test]
    fn beacon_flags_become_a_security_kind() {
        // Measured: WPA2-PSK access point.
        assert_eq!(
            WifiSecurity::from_ap_flags(0x0003, 0x0000, 0x0188),
            WifiSecurity::WpaPsk
        );
        // Measured: SAE access point.
        assert_eq!(
            WifiSecurity::from_ap_flags(0x0003, 0x0000, 0x0488),
            WifiSecurity::Sae
        );
        // Measured: genuinely open access point — note no PRIVACY bit.
        assert_eq!(
            WifiSecurity::from_ap_flags(0x0002, 0x0000, 0x0000),
            WifiSecurity::Open
        );
    }

    #[test]
    fn a_transition_access_point_is_joined_as_wpa2() {
        // PSK and SAE in one RSN element. Every card can do the first.
        let both = 0x100 | 0x400;
        assert_eq!(
            WifiSecurity::from_ap_flags(0x0003, 0x0000, both),
            WifiSecurity::WpaPsk
        );
    }

    #[test]
    fn privacy_with_no_key_management_is_wep_not_open() {
        // The distinction that needs `flags`: WEP sets PRIVACY and advertises
        // neither WPA nor RSN. Shown as unsupported; never as a free network.
        let wep = WifiSecurity::from_ap_flags(0x0001, 0x0000, 0x0000);
        assert_eq!(wep, WifiSecurity::Wep);
        assert!(!wep.joinable());
    }

    #[test]
    fn enterprise_outranks_a_passphrase_it_also_offers() {
        let mixed = 0x100 | 0x200;
        assert_eq!(
            WifiSecurity::from_ap_flags(0x0003, 0x0000, mixed),
            WifiSecurity::Enterprise
        );
        assert!(!WifiSecurity::Enterprise.joinable());
    }

    #[test]
    fn owe_needs_no_password() {
        assert_eq!(
            WifiSecurity::from_ap_flags(0x0000, 0x0000, 0x0800),
            WifiSecurity::Owe
        );
        assert!(WifiSecurity::Owe.joinable());
        assert!(!WifiSecurity::Owe.needs_password());
    }

    fn psk(ssid: &str, password: Option<&str>) -> WifiJoinRequest {
        WifiJoinRequest {
            ssid: ssid.into(),
            security: WifiSecurity::WpaPsk,
            password: password.map(Into::into),
            hidden: false,
            connect: true,
        }
    }

    #[test]
    fn a_passphrase_is_checked_against_80211i_not_against_taste() {
        assert!(psk("home", Some("12345678")).validate().is_ok());
        assert!(psk("home", Some(&"x".repeat(63))).validate().is_ok());
        assert!(psk("home", Some(&"a".repeat(64))).validate().is_ok()); // raw PMK
        assert_eq!(
            psk("home", Some("short")).validate(),
            Err(WifiRequestError::PskLength)
        );
        assert_eq!(
            psk("home", Some(&"x".repeat(65))).validate(),
            Err(WifiRequestError::PskLength)
        );
    }

    #[test]
    fn a_curly_quote_is_caught_here_rather_than_blamed_on_the_password() {
        // What a phone keyboard substitutes for an apostrophe. Left alone it
        // fails at association with reason 7, which reads as "wrong password".
        let err = psk("home", Some("it\u{2019}s a secret")).validate();
        assert_eq!(err, Err(WifiRequestError::PskCharacters));
    }

    #[test]
    fn sixty_four_characters_that_are_not_hex_are_a_passphrase() {
        // Length alone must not be read as a raw PMK. 64 characters is past
        // the 8..=63 passphrase range, so a 64-character key is legal only as
        // hex — and one that is not hex has to be rejected rather than
        // quietly stored as a key nobody meant to set.
        let not_hex = "z".repeat(64);
        assert_eq!(
            psk("home", Some(&not_hex)).validate(),
            Err(WifiRequestError::PskLength)
        );
    }

    #[test]
    fn a_password_for_an_open_network_is_refused_not_ignored() {
        let request = WifiJoinRequest {
            ssid: "cafe".into(),
            security: WifiSecurity::Open,
            password: Some("hunter2".into()),
            hidden: false,
            connect: true,
        };
        assert_eq!(
            request.validate(),
            Err(WifiRequestError::PasswordNotAllowed)
        );
    }

    #[test]
    fn sae_takes_a_short_password_that_wpa2_would_refuse() {
        let request = WifiJoinRequest {
            ssid: "home".into(),
            security: WifiSecurity::Sae,
            password: Some("hi".into()),
            hidden: false,
            connect: true,
        };
        assert!(request.validate().is_ok());
    }

    #[test]
    fn ssid_bounds_are_octets_not_characters() {
        // Eleven emoji is 44 bytes, over the 32-octet limit, though it is
        // eleven "characters" to the person typing it.
        let long = "\u{1f600}".repeat(11);
        assert!(long.chars().count() < MAX_SSID_BYTES);
        assert_eq!(
            psk(&long, Some("12345678")).validate(),
            Err(WifiRequestError::SsidLength)
        );
        assert_eq!(
            psk("", Some("12345678")).validate(),
            Err(WifiRequestError::SsidLength)
        );
    }

    #[test]
    fn enterprise_is_refused_with_a_sentence_about_admin_mode() {
        let request = WifiJoinRequest {
            ssid: "eduroam".into(),
            security: WifiSecurity::Enterprise,
            password: Some("something".into()),
            hidden: false,
            connect: true,
        };
        let err = request.validate().unwrap_err();
        assert_eq!(err, WifiRequestError::SecurityUnsupported);
        assert!(err.message().contains("admin mode"));
    }

    #[test]
    fn a_password_never_reaches_a_debug_line() {
        let request = psk("home", Some("correcthorsebattery"));
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("correcthorsebattery"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
        // The rest of the request is still useful in a log.
        assert!(rendered.contains("home"), "{rendered}");
    }

    fn net(ssid: &str, signal: u8, band: u8) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            security: WifiSecurity::WpaPsk,
            signal_percent: signal,
            bands_ghz: vec![band],
            saved: false,
            active: false,
        }
    }

    #[test]
    fn one_mesh_on_two_bands_is_one_row() {
        let (merged, truncated) = aggregate_networks(vec![
            net("home", 55, 2),
            net("home", 80, 5),
            net("home", 40, 5),
        ]);
        assert!(!truncated);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].signal_percent, 80);
        assert_eq!(merged[0].bands_ghz, vec![2, 5]);
    }

    #[test]
    fn the_same_name_with_different_security_stays_two_rows() {
        // This device really does carry two profiles for one printer, written
        // months apart as `sae` and `wpa-psk`. Merging them would offer a
        // parent one row that joins the wrong one half the time.
        let mut sae = net("DIRECT-04-HP", 60, 2);
        sae.security = WifiSecurity::Sae;
        let (merged, _) = aggregate_networks(vec![net("DIRECT-04-HP", 50, 2), sae]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn the_network_you_are_on_sorts_above_a_stronger_one() {
        let mut active = net("home", 30, 2);
        active.active = true;
        let (merged, _) = aggregate_networks(vec![net("neighbour", 95, 2), active]);
        assert_eq!(merged[0].ssid, "home");
    }

    #[test]
    fn saved_and_active_survive_the_merge() {
        let mut a = net("home", 40, 2);
        a.saved = true;
        let mut b = net("home", 70, 5);
        b.active = true;
        let (merged, _) = aggregate_networks(vec![a, b]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].saved && merged[0].active);
    }

    #[test]
    fn a_crowded_band_is_capped_and_says_so() {
        let many: Vec<_> = (0..MAX_WIFI_NETWORKS + 5)
            .map(|i| net(&format!("net-{i:02}"), (i % 100) as u8, 2))
            .collect();
        let (merged, truncated) = aggregate_networks(many);
        assert_eq!(merged.len(), MAX_WIFI_NETWORKS);
        assert!(truncated);
    }

    #[test]
    fn a_full_scan_still_fits_a_ble_frame() {
        // The reason for MAX_WIFI_NETWORKS. 16 KiB is
        // lunchbox_ble::protocol::MAX_FRAME_BYTES, which this crate cannot
        // depend on, so the number is repeated with its source named.
        const MAX_FRAME_BYTES: usize = 16 * 1024;
        let networks: Vec<_> = (0..MAX_WIFI_NETWORKS)
            .map(|i| WifiNetwork {
                ssid: format!("{:\u{1f600}<10}{i}", "net"),
                security: WifiSecurity::WpaPsk,
                signal_percent: 88,
                bands_ghz: vec![2, 5],
                saved: true,
                active: false,
            })
            .collect();
        let view = WifiScanView {
            supported: true,
            radio_enabled: true,
            networks,
            truncated: true,
            last_scan_age_s: Some(12),
            join: WifiJoinState::Failed {
                ssid: "a network with a fairly long name".into(),
                reason: WifiJoinFailure::detailed(
                    WifiJoinFailureKind::Other,
                    "an unusually wordy reason",
                ),
            },
            can_configure: true,
        };
        let encoded = serde_json::to_vec(&view).expect("serialises");
        assert!(
            encoded.len() < MAX_FRAME_BYTES,
            "a full scan is {} bytes, over the {MAX_FRAME_BYTES} byte BLE frame",
            encoded.len()
        );
    }

    #[test]
    fn join_state_tags_itself_for_a_typescript_client() {
        let json = serde_json::to_string(&WifiJoinState::Connecting {
            ssid: "home".into(),
        })
        .unwrap();
        assert!(json.contains(r#""state":"connecting""#), "{json}");
    }

    fn failed(ssid: &str) -> WifiJoinState {
        WifiJoinState::Failed {
            ssid: ssid.into(),
            reason: WifiJoinFailure::of(WifiJoinFailureKind::NotFound),
        }
    }

    fn on(ssid: &str) -> WifiNetwork {
        WifiNetwork {
            active: true,
            ..net(ssid, 80, 5)
        }
    }

    #[test]
    fn a_failure_for_the_network_the_device_is_on_is_dropped() {
        // Seen on a device: "No network called X answered" above a list with
        // X marked Connected, because NetworkManager autoconnected it after
        // the join that failed.
        assert_eq!(
            failed("home").reconciled(&[on("home"), net("next door", 40, 2)]),
            WifiJoinState::Idle
        );
    }

    #[test]
    fn a_failure_for_another_network_still_stands() {
        // The parent tried "cafe" and the device fell back to "home". The
        // failure is still the thing they need to read.
        assert_eq!(failed("cafe").reconciled(&[on("home")]), failed("cafe"));
        assert_eq!(failed("cafe").reconciled(&[]), failed("cafe"));
    }

    #[test]
    fn connected_to_a_network_the_device_left_is_dropped() {
        let connected = WifiJoinState::Connected {
            ssid: "home".into(),
        };
        assert_eq!(
            connected.clone().reconciled(&[net("home", 80, 5)]),
            WifiJoinState::Idle
        );
        assert_eq!(connected.clone().reconciled(&[on("home")]), connected);
    }

    #[test]
    fn a_join_in_progress_is_never_second_guessed() {
        let connecting = WifiJoinState::Connecting {
            ssid: "home".into(),
        };
        assert_eq!(connecting.clone().reconciled(&[]), connecting);
        assert_eq!(connecting.clone().reconciled(&[on("home")]), connecting);
        assert_eq!(
            WifiJoinState::Idle.reconciled(&[on("home")]),
            WifiJoinState::Idle
        );
    }

    #[test]
    fn only_connecting_is_in_progress() {
        assert!(WifiJoinState::Connecting { ssid: "a".into() }.in_progress());
        assert!(!WifiJoinState::Idle.in_progress());
        assert!(!WifiJoinState::Connected { ssid: "a".into() }.in_progress());
    }
}
