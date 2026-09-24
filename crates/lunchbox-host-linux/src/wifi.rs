//! Scanning for and joining wireless networks through NetworkManager
//! (issue #194).
//!
//! The change half of [`crate::network`]. Split from it because the two halves
//! answer to different privilege: every property read here is readable by an
//! unprivileged user, exactly as #182's reads are, while every *write* needs
//! `settings.modify.system`, which the kiosk user deliberately does not hold.
//!
//! So this module does the reads — and one write, because it is not really a
//! privileged one: **activating a profile that already exists** is gated on
//! `network-control`, which polkit grants to an active local session outright.
//! Saving and deleting a profile need `settings.modify.system` and are
//! forwarded to the state custodian instead (see `lunchboxd`'s `wifi` module).
//!
//! That split is the measured permission table, not a guess: a device whose
//! polkit rules file is missing can still scan and still get back onto a
//! network it knows, which is exactly what its diagnostic promises.
//!
//! ## What was measured, and what it changed
//!
//! The mechanics here were settled against NetworkManager 1.54.3 on Ubuntu
//! 26.04 rather than read out of its documentation. See
//! `docs/ai/history/2026-09-21 004 wifi-against-real-networkmanager (#194).md`.
//! The three that shaped this code:
//!
//! * **Pick a device on `DeviceType == 2` *and* `Managed == true`.** Type
//!   alone is not enough. Virtual radios — `mac80211_hwsim`, and anything a
//!   test bed or a USB dongle leaves unmanaged — are also type 2, and picking
//!   one means scanning a radio that can hear nothing.
//! * **`LastScan` is `CLOCK_BOOTTIME` milliseconds, and `AccessPoint.LastSeen`
//!   is `CLOCK_BOOTTIME` seconds.** Different units on adjacent properties,
//!   and neither is a wall clock, so the age has to be computed against
//!   `clock_gettime(CLOCK_BOOTTIME)` rather than `SystemTime::now`.
//! * **An open network carries no `PRIVACY` bit**, which is the only thing
//!   that distinguishes it from WEP. The classification itself lives in
//!   [`lunchbox_api::WifiSecurity::from_ap_flags`] so both backends agree.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use lunchbox_api::{SavedWifiNetwork, WifiJoinRequest, WifiJoinState, WifiNetwork, WifiSecurity};
use lunchbox_host_api::{WifiController, WifiError, WifiResult, WifiSnapshot};
use tracing::{debug, warn};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};

/// How long a whole NetworkManager read gets before we give up.
///
/// Same bargain as [`crate::network`]: this sits behind an RPC a UI polls
/// while a page is open, so a wedged system bus has to degrade the answer
/// rather than stall the caller.
const DBUS_TIMEOUT: Duration = Duration::from_secs(3);

/// `NMDeviceType::WIFI`. Note that `WIFI_P2P` is 30 and must not be picked —
/// every wireless adapter on this host also has a `p2p-dev-*` device.
const NM_DEVICE_TYPE_WIFI: u32 = 2;

/// `NM_DEVICE_STATE_ACTIVATED`: the device is on a network, address and all.
///
/// What "active" means everywhere in this module. `ActiveAccessPoint` is set
/// as soon as association *starts*, and an active connection exists from the
/// moment one is asked for, so either alone marks a network "Connected" while
/// the device is still trying it -- and, on a device, while it was trying a
/// saved password that went on to fail.
const NM_DEVICE_STATE_ACTIVATED: u32 = 100;

/// `NM_ACTIVE_CONNECTION_STATE_ACTIVATED`, the same thing for an active
/// connection.
const NM_ACTIVE_CONNECTION_STATE_ACTIVATED: u32 = 2;

/// `NM_802_11_MODE_INFRA`. Ad-hoc and AP-mode beacons are not networks a
/// parent can join from a picker.
const NM_802_11_MODE_INFRA: u32 = 2;

/// NetworkManager's null object path. Building a proxy for it succeeds and
/// every read then fails, so it is checked rather than caught.
const NM_NULL_PATH: &str = "/";

/// The same thing as an `ObjectPath`, for the arguments that are typed `o`
/// rather than `s` on the bus.
///
/// Passing a plain string there is accepted by Rust and refused by D-Bus at
/// call time -- `Type of message, "(oos)", does not match expected type
/// "(ooo)"` -- which no unit test can catch.
fn null_path() -> ObjectPath<'static> {
    ObjectPath::try_from(NM_NULL_PATH).expect("\"/\" is a valid object path")
}

/// The `connection.type` of a wireless profile. Others (ethernet, tun, bridge)
/// are filtered out of the saved list.
const NM_SETTING_WIRELESS: &str = "802-11-wireless";

/// Pin the wireless adapter to one interface by name, instead of taking the
/// first managed one.
///
/// This exists so the feature can be exercised against a real NetworkManager
/// without touching the adapter the developer is connected over. A dev box's
/// only real radio usually carries the SSH session, and joining a network on
/// it disconnects whoever is testing — so the integration tests run against
/// `mac80211_hwsim` radios and name one here.
///
/// It is also the escape hatch on a device with two adapters, where "the first
/// managed one" is not the one the household uses. Unset on an installed
/// device, which is the case the code is actually tuned for.
pub const WIFI_INTERFACE_ENV: &str = "LUNCHBOX_WIFI_INTERFACE";

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Activate a profile that already exists.
    ///
    /// Gated on `network-control`, which polkit grants to an active local
    /// session outright — so unlike *writing* a profile, this needs no
    /// custodian and no rules file. Measured on 1.54.3; see the module docs.
    fn activate_connection(
        &self,
        connection: &OwnedObjectPath,
        device: &OwnedObjectPath,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;

    /// Whether the wireless radio is enabled. Reported, never set — toggling
    /// it is out of scope for #194.
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn active_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;

    #[zbus(property, name = "DeviceType")]
    fn device_type(&self) -> zbus::Result<u32>;

    /// Whether NetworkManager is driving this device.
    ///
    /// Load-bearing, not a nicety: an unmanaged adapter is still
    /// `DeviceType == 2`, and scanning one returns nothing for ever.
    #[zbus(property)]
    fn managed(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait DeviceWireless {
    /// Every access point heard on the last scan.
    #[zbus(property)]
    fn access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// The AP this interface is associated with, or the null path.
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;

    /// `CLOCK_BOOTTIME` **milliseconds** at which the last scan finished, or
    /// `-1` for "never scanned". Not a wall clock, and not the same unit as
    /// [`AccessPointProxy::last_seen`].
    #[zbus(property)]
    fn last_scan(&self) -> zbus::Result<i64>;

    /// Ask for a scan. Returns as soon as NetworkManager has accepted it.
    fn request_scan(&self, options: HashMap<&str, zbus::zvariant::Value<'_>>) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    /// `NM80211ApFlags`. Bit 0 is `PRIVACY`, and its *absence* is what marks a
    /// genuinely open network.
    #[zbus(property)]
    fn flags(&self) -> zbus::Result<u32>;

    /// `NM80211ApSecurityFlags` from the WPA information element.
    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;

    /// `NM80211ApSecurityFlags` from the RSN information element.
    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;

    #[zbus(property)]
    fn frequency(&self) -> zbus::Result<u32>;

    /// `NM80211Mode`.
    #[zbus(property)]
    fn mode(&self) -> zbus::Result<u32>;

    /// `CLOCK_BOOTTIME` **seconds** when this AP was last seen. Kept for the
    /// unit difference it documents; not otherwise used.
    #[zbus(property)]
    fn last_seen(&self) -> zbus::Result<i32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SettingsConnection {
    /// The profile, minus its secrets. Secrets come only from `GetSecrets`,
    /// which is gated on `settings.modify.system` — and which nothing in
    /// Lunchbox calls, because no management UI ever reads a password back.
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn uuid(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

/// Reads this device's wireless state over NetworkManager.
///
/// Holds no connection: like [`crate::network::LinuxNetworkInfo`], each call
/// opens one and drops it. A status page polls this a few times a minute, and
/// a cached connection to a service that can restart underneath us costs more
/// than it saves.
pub struct LinuxWifiReader;

impl LinuxWifiReader {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LinuxWifiReader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WifiController for LinuxWifiReader {
    async fn scan(&self) -> WifiResult<()> {
        match tokio::time::timeout(DBUS_TIMEOUT, request_scan()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                // A refusal here is usually "a scan is already running", which
                // is what the caller wanted anyway. Logged, not surfaced: the
                // UI's next poll gets fresh results either way.
                debug!(error = %e, "NetworkManager refused a scan request");
                Ok(())
            }
            Err(_) => Err(WifiError::Backend("NetworkManager did not answer".into())),
        }
    }

    async fn networks(&self) -> WifiSnapshot {
        match tokio::time::timeout(DBUS_TIMEOUT, read_networks()).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(e)) => {
                warn!(error = %e, "could not read wireless networks from NetworkManager");
                WifiSnapshot::unsupported()
            }
            Err(_) => {
                warn!("NetworkManager did not answer a scan-list read in time");
                WifiSnapshot::unsupported()
            }
        }
    }

    async fn saved(&self) -> WifiResult<Vec<SavedWifiNetwork>> {
        match tokio::time::timeout(DBUS_TIMEOUT, read_saved()).await {
            Ok(Ok(saved)) => Ok(saved),
            Ok(Err(e)) => Err(WifiError::Backend(e.to_string())),
            Err(_) => Err(WifiError::Backend("NetworkManager did not answer".into())),
        }
    }

    async fn save(&self, _request: &WifiJoinRequest) -> WifiResult<SavedWifiNetwork> {
        Err(WifiError::NotAuthorized)
    }

    async fn connect(&self, id: &str) -> WifiResult<()> {
        // Not forwarded to the custodian, unlike save and forget. Activating a
        // profile that already exists needs `network-control`, which polkit
        // grants to an active local session outright -- so a device whose
        // rules file is missing can still get back onto a network it knows,
        // which is exactly what the diagnostic about that device promises.
        match tokio::time::timeout(DBUS_TIMEOUT, activate(id)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(WifiError::Backend("NetworkManager did not answer".into())),
        }
    }

    async fn forget(&self, _id: &str) -> WifiResult<bool> {
        Err(WifiError::NotAuthorized)
    }

    async fn join_state(&self) -> WifiJoinState {
        WifiJoinState::Idle
    }

    async fn can_configure(&self) -> bool {
        false
    }
}

/// The first managed wireless device, and a proxy for its wireless interface.
///
/// `Managed` is half the filter. The other half is `DeviceType`, and both are
/// needed: this host carries a `p2p-dev-*` device per adapter (type 30) and,
/// under test, unmanaged `mac80211_hwsim` radios (type 2). Choosing by type
/// alone picks one of the latter and scans a radio that hears nothing.
///
/// Several managed adapters is not a case #194 handles: the first wins, and
/// letting a parent choose is out of scope.
///
/// [`WIFI_INTERFACE_ENV`] overrides the choice, which is what makes this
/// testable at all — see its documentation.
async fn wireless_device(
    conn: &zbus::Connection,
) -> zbus::Result<Option<(OwnedObjectPath, String)>> {
    let wanted = std::env::var(WIFI_INTERFACE_ENV).ok();
    let nm = NetworkManagerProxy::new(conn).await?;
    for path in nm.devices().await? {
        let device = DeviceProxy::builder(conn)
            .path(path.clone())?
            .build()
            .await?;
        if device.device_type().await? != NM_DEVICE_TYPE_WIFI {
            continue;
        }
        if !device.managed().await.unwrap_or(false) {
            debug!(path = %path.as_str(), "skipping an unmanaged wireless device");
            continue;
        }
        let name = device.interface().await.unwrap_or_default();
        if let Some(wanted) = &wanted
            && &name != wanted
        {
            continue;
        }
        return Ok(Some((path, name)));
    }
    if let Some(wanted) = wanted {
        warn!(
            interface = %wanted,
            "{WIFI_INTERFACE_ENV} names an interface NetworkManager is not managing"
        );
    }
    Ok(None)
}

async fn request_scan() -> zbus::Result<()> {
    let conn = zbus::Connection::system().await?;
    let Some((path, _)) = wireless_device(&conn).await? else {
        return Err(zbus::Error::Failure("no managed wireless device".into()));
    };
    let wireless = DeviceWirelessProxy::builder(&conn)
        .path(path)?
        .build()
        .await?;
    wireless.request_scan(HashMap::new()).await
}

/// Activate a saved profile by its UUID.
///
/// Fails with [`WifiError::UnknownNetwork`] rather than a backend error when
/// the id names nothing: a UI left open while somebody else forgot the network
/// deserves to be told which of the two happened.
async fn activate(uuid: &str) -> WifiResult<()> {
    let conn = zbus::Connection::system()
        .await
        .map_err(|e| WifiError::Backend(e.to_string()))?;
    let Some((device, _)) = wireless_device(&conn)
        .await
        .map_err(|e| WifiError::Backend(e.to_string()))?
    else {
        return Err(WifiError::NoAdapter);
    };
    let Some(path) = profile_path(&conn, uuid)
        .await
        .map_err(|e| WifiError::Backend(e.to_string()))?
    else {
        return Err(WifiError::UnknownNetwork);
    };
    let nm = NetworkManagerProxy::new(&conn)
        .await
        .map_err(|e| WifiError::Backend(e.to_string()))?;
    nm.activate_connection(&path, &device, &null_path())
        .await
        .map_err(activation_error)?;
    Ok(())
}

/// An activation failure, with polkit's refusal told apart from the rest.
///
/// `network-control` is granted to an *active local* session. A daemon started
/// from an SSH shell is not one, so a developer sees this refusal where a
/// device would not — and it is the one failure here somebody can act on, so
/// it must not arrive as "internal error".
fn activation_error(error: zbus::Error) -> WifiError {
    let message = error.to_string();
    if message.contains("PermissionDenied") || message.contains("Not authorized") {
        WifiError::NotAuthorized
    } else {
        WifiError::Backend(message)
    }
}

/// The object path of the wireless profile with this UUID.
async fn profile_path(
    conn: &zbus::Connection,
    uuid: &str,
) -> zbus::Result<Option<OwnedObjectPath>> {
    let settings = SettingsProxy::new(conn).await?;
    for path in settings.list_connections().await? {
        let profile = SettingsConnectionProxy::builder(conn)
            .path(path.clone())?
            .build()
            .await?;
        let Ok(config) = profile.get_settings().await else {
            continue;
        };
        let Some(connection) = config.get("connection") else {
            continue;
        };
        if connection.get("type").and_then(as_str).as_deref() != Some(NM_SETTING_WIRELESS) {
            continue;
        }
        if connection.get("uuid").and_then(as_str).as_deref() == Some(uuid) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// `CLOCK_BOOTTIME` in milliseconds — the clock `LastScan` is expressed in.
///
/// Not `SystemTime::now`. `LastScan` counts from boot, so comparing it against
/// the wall clock yields an age in the tens of thousands of seconds, which a
/// UI would render as "last scanned 26 hours ago" on a freshly scanned device.
fn boottime_millis() -> Option<i64> {
    let t = nix::time::clock_gettime(nix::time::ClockId::CLOCK_BOOTTIME).ok()?;
    Some((t.tv_sec() as i64) * 1000 + (t.tv_nsec() as i64) / 1_000_000)
}

async fn read_networks() -> zbus::Result<WifiSnapshot> {
    let conn = zbus::Connection::system().await?;
    let nm = NetworkManagerProxy::new(&conn).await?;
    let radio_enabled = nm.wireless_enabled().await.unwrap_or(false);

    let Some((path, interface)) = wireless_device(&conn).await? else {
        return Ok(WifiSnapshot::unsupported());
    };
    let device = DeviceProxy::builder(&conn)
        .path(path.clone())?
        .build()
        .await?;
    let wireless = DeviceWirelessProxy::builder(&conn)
        .path(path)?
        .build()
        .await?;

    // Which network we are on, so the list can mark it. Read before the APs
    // so a scan arriving mid-read cannot make the active row disappear.
    // *On*, not trying: see `NM_DEVICE_STATE_ACTIVATED`.
    let activated = device.state().await.ok() == Some(NM_DEVICE_STATE_ACTIVATED);
    let active_ssid = match wireless.active_access_point().await {
        Ok(ap) if activated && ap.as_str() != NM_NULL_PATH => read_ap_ssid(&conn, &ap).await,
        _ => None,
    };
    let saved_ssids = read_saved().await.map(|saved| {
        saved
            .into_iter()
            .map(|n| n.ssid)
            .collect::<std::collections::HashSet<_>>()
    });
    let saved_ssids = saved_ssids.unwrap_or_default();

    let mut networks = Vec::new();
    for ap_path in wireless.access_points().await.unwrap_or_default() {
        match read_access_point(&conn, &ap_path, &active_ssid, &saved_ssids).await {
            Some(network) => networks.push(network),
            None => continue,
        }
    }

    let last_scan_age_s = match (wireless.last_scan().await, boottime_millis()) {
        // -1 is NetworkManager's "never scanned since boot".
        (Ok(last), Some(now)) if last >= 0 => Some(now.saturating_sub(last).max(0) as u64 / 1000),
        _ => None,
    };

    debug!(
        interface = %interface,
        access_points = networks.len(),
        radio_enabled,
        "read the wireless scan list"
    );

    Ok(WifiSnapshot {
        supported: true,
        radio_enabled,
        networks,
        last_scan_age_s,
    })
}

async fn read_ap_ssid(conn: &zbus::Connection, path: &OwnedObjectPath) -> Option<String> {
    let ap = AccessPointProxy::builder(conn)
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    decode_ssid(&ap.ssid().await.ok()?)
}

/// One access point, or `None` for one a picker cannot offer.
///
/// Three reasons to leave one out, all of them "a parent could not act on
/// this row":
///
/// * a **hidden** network announces an empty SSID, and manual entry is how you
///   join one;
/// * an SSID that is **not UTF-8** cannot be rendered or retyped — 802.11
///   carries 32 arbitrary octets, not a string;
/// * a beacon that is not **infrastructure** mode is somebody's ad-hoc link or
///   another device's hotspot.
async fn read_access_point(
    conn: &zbus::Connection,
    path: &OwnedObjectPath,
    active_ssid: &Option<String>,
    saved_ssids: &std::collections::HashSet<String>,
) -> Option<WifiNetwork> {
    let ap = AccessPointProxy::builder(conn)
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;

    if ap.mode().await.ok()? != NM_802_11_MODE_INFRA {
        return None;
    }
    let ssid = decode_ssid(&ap.ssid().await.ok()?)?;

    let security = WifiSecurity::from_ap_flags(
        ap.flags().await.unwrap_or(0),
        ap.wpa_flags().await.unwrap_or(0),
        ap.rsn_flags().await.unwrap_or(0),
    );

    Some(WifiNetwork {
        active: active_ssid.as_deref() == Some(ssid.as_str()),
        saved: saved_ssids.contains(&ssid),
        signal_percent: ap.strength().await.unwrap_or(0),
        bands_ghz: band_ghz(ap.frequency().await.unwrap_or(0))
            .map(|b| vec![b])
            .unwrap_or_default(),
        ssid,
        security,
    })
}

/// An SSID that can be shown in a picker, or `None`.
///
/// Empty means hidden; invalid UTF-8 means unrenderable. Both are legal on the
/// air and neither belongs in a list of things to tap.
fn decode_ssid(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// The band a centre frequency falls in, in GHz. Mirrors
/// [`lunchbox_api::WifiView::band_ghz`], which cannot be reused because it
/// reads a whole view.
fn band_ghz(frequency_mhz: u32) -> Option<u8> {
    match frequency_mhz {
        2_400..=2_500 => Some(2),
        4_900..=5_900 => Some(5),
        5_925..=7_125 => Some(6),
        _ => None,
    }
}

/// Every saved wireless profile, with the active one marked.
///
/// Wireless only: `connection.type` filters out ethernet, tun and the bridge
/// profiles a device running containers accumulates.
async fn read_saved() -> zbus::Result<Vec<SavedWifiNetwork>> {
    let conn = zbus::Connection::system().await?;
    let settings = SettingsProxy::new(&conn).await?;
    let active = active_uuids(&conn).await;

    let mut saved = Vec::new();
    for path in settings.list_connections().await? {
        let profile = SettingsConnectionProxy::builder(&conn)
            .path(path.clone())?
            .build()
            .await?;
        let Ok(config) = profile.get_settings().await else {
            continue;
        };
        if let Some(network) = saved_from_settings(&config, &active) {
            saved.push(network);
        }
    }
    Ok(saved)
}

/// The UUIDs of every activated connection, so a saved profile can say
/// whether it is the one in use -- not merely the one being tried; see
/// `NM_DEVICE_STATE_ACTIVATED`.
async fn active_uuids(conn: &zbus::Connection) -> std::collections::HashSet<String> {
    let mut uuids = std::collections::HashSet::new();
    let Ok(nm) = NetworkManagerProxy::new(conn).await else {
        return uuids;
    };
    for path in nm.active_connections().await.unwrap_or_default() {
        let Ok(builder) = ActiveConnectionProxy::builder(conn).path(path) else {
            continue;
        };
        if let Ok(active) = builder.build().await
            && active.state().await.ok() == Some(NM_ACTIVE_CONNECTION_STATE_ACTIVATED)
            && let Ok(uuid) = active.uuid().await
        {
            uuids.insert(uuid);
        }
    }
    uuids
}

/// Turn one NetworkManager profile dictionary into a view, or `None` if it is
/// not a wireless profile.
///
/// Pulled out of the D-Bus walk so it can be tested against the dictionary
/// shapes NetworkManager really produces — including the netplan-normalised
/// ones on Ubuntu, where `permissions` and `autoconnect` are absent because
/// netplan defaults them rather than writing them out.
fn saved_from_settings(
    config: &HashMap<String, HashMap<String, OwnedValue>>,
    active: &std::collections::HashSet<String>,
) -> Option<SavedWifiNetwork> {
    let connection = config.get("connection")?;
    if as_str(connection.get("type")?)? != NM_SETTING_WIRELESS {
        return None;
    }
    let uuid = as_str(connection.get("uuid")?)?;
    let wireless = config.get(NM_SETTING_WIRELESS)?;

    // The SSID is the authority on the name; `connection.id` is a label a
    // person may have renamed, and on this dev device two profiles carry the
    // same id for different networks.
    let ssid = wireless
        .get("ssid")
        .and_then(as_bytes)
        .and_then(|b| decode_ssid(&b))
        .or_else(|| as_str(connection.get("id")?))?;

    let key_mgmt = config
        .get("802-11-wireless-security")
        .and_then(|s| s.get("key-mgmt"))
        .and_then(as_str);

    Some(SavedWifiNetwork {
        active: active.contains(&uuid),
        id: uuid,
        ssid,
        security: security_from_key_mgmt(key_mgmt.as_deref()),
        hidden: wireless.get("hidden").and_then(as_bool).unwrap_or(false),
        // Absent means true: NetworkManager's default, and netplan omits the
        // key entirely on Ubuntu rather than writing `autoconnect=true`.
        autoconnect: connection
            .get("autoconnect")
            .and_then(as_bool)
            .unwrap_or(true),
    })
}

/// A stored profile's `key-mgmt` back into a [`WifiSecurity`].
///
/// The inverse of [`WifiSecurity::key_mgmt`], and deliberately not derived
/// from it: `wpa-eap` and `wpa-eap-suite-b-192` map onto `Enterprise`, which
/// has no `key_mgmt` of its own because these UIs will not write one.
fn security_from_key_mgmt(key_mgmt: Option<&str>) -> WifiSecurity {
    match key_mgmt {
        None | Some("none") => WifiSecurity::Open,
        Some("owe") => WifiSecurity::Owe,
        Some("wpa-psk") => WifiSecurity::WpaPsk,
        Some("sae") => WifiSecurity::Sae,
        Some("wpa-eap") | Some("wpa-eap-suite-b-192") | Some("ieee8021x") => {
            WifiSecurity::Enterprise
        }
        // A profile with a `wep-key0` and no key-mgmt reads as Open above;
        // this arm catches the explicit spelling.
        Some(_) => WifiSecurity::Wep,
    }
}

fn as_str(value: &OwnedValue) -> Option<String> {
    <&str>::try_from(value).map(|s| s.to_string()).ok()
}

fn as_bool(value: &OwnedValue) -> Option<bool> {
    bool::try_from(value).ok()
}

fn as_bytes(value: &OwnedValue) -> Option<Vec<u8>> {
    <Vec<u8>>::try_from(value.try_clone().ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn owned(value: Value<'_>) -> OwnedValue {
        OwnedValue::try_from(value).expect("ownable")
    }

    /// Build the dictionary NetworkManager returns from `GetSettings`.
    fn profile(
        pairs: &[(&str, &[(&str, OwnedValue)])],
    ) -> HashMap<String, HashMap<String, OwnedValue>> {
        pairs
            .iter()
            .map(|(section, entries)| {
                (
                    section.to_string(),
                    entries
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.try_clone().expect("clonable")))
                        .collect(),
                )
            })
            .collect()
    }

    fn no_active() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    #[test]
    fn a_wireless_profile_becomes_a_saved_network() {
        // The shape NetworkManager reported for a profile written through
        // netplan on this dev device: no `permissions`, no `autoconnect`, and
        // an `interface-name` netplan added of its own accord.
        let config = profile(&[
            (
                "connection",
                &[
                    ("type", owned(Value::from("802-11-wireless"))),
                    (
                        "uuid",
                        owned(Value::from("6c16874f-3e20-47c6-a114-4436691a54a7")),
                    ),
                    ("id", owned(Value::from("lunchbox-hwsimtest"))),
                    ("interface-name", owned(Value::from("wlan0"))),
                ],
            ),
            (
                "802-11-wireless",
                &[("ssid", owned(Value::from(&b"lunchbox-hwsimtest"[..])))],
            ),
            (
                "802-11-wireless-security",
                &[("key-mgmt", owned(Value::from("wpa-psk")))],
            ),
        ]);

        let saved = saved_from_settings(&config, &no_active()).expect("a wireless profile");
        assert_eq!(saved.ssid, "lunchbox-hwsimtest");
        assert_eq!(saved.id, "6c16874f-3e20-47c6-a114-4436691a54a7");
        assert_eq!(saved.security, WifiSecurity::WpaPsk);
        assert!(!saved.hidden);
        assert!(!saved.active);
        assert!(
            saved.autoconnect,
            "netplan omits autoconnect rather than writing true, and absent means on"
        );
    }

    #[test]
    fn a_wired_profile_is_not_in_the_wireless_list() {
        // A device running containers accumulates bridge and tun profiles.
        let config = profile(&[(
            "connection",
            &[
                ("type", owned(Value::from("802-3-ethernet"))),
                ("uuid", owned(Value::from("wired-uuid"))),
                ("id", owned(Value::from("Wired connection 1"))),
            ],
        )]);
        assert!(saved_from_settings(&config, &no_active()).is_none());
    }

    #[test]
    fn the_ssid_outranks_the_label_somebody_renamed() {
        // Measured on this dev device: one profile's `connection.id` is
        // "DIRECT-04-HP OfficeJet 250 1" while its SSID is the name without
        // the suffix. The picker has to match the air, not the label.
        let config = profile(&[
            (
                "connection",
                &[
                    ("type", owned(Value::from("802-11-wireless"))),
                    ("uuid", owned(Value::from("d095dc21"))),
                    ("id", owned(Value::from("DIRECT-04-HP OfficeJet 250 1"))),
                ],
            ),
            (
                "802-11-wireless",
                &[(
                    "ssid",
                    owned(Value::from(&b"DIRECT-04-HP OfficeJet 250"[..])),
                )],
            ),
        ]);
        let saved = saved_from_settings(&config, &no_active()).expect("wireless");
        assert_eq!(saved.ssid, "DIRECT-04-HP OfficeJet 250");
    }

    #[test]
    fn an_active_profile_says_so() {
        let config = profile(&[
            (
                "connection",
                &[
                    ("type", owned(Value::from("802-11-wireless"))),
                    ("uuid", owned(Value::from("live-uuid"))),
                    ("id", owned(Value::from("home"))),
                ],
            ),
            (
                "802-11-wireless",
                &[("ssid", owned(Value::from(&b"home"[..])))],
            ),
        ]);
        let active = std::collections::HashSet::from(["live-uuid".to_string()]);
        assert!(
            saved_from_settings(&config, &active)
                .expect("wireless")
                .active
        );
    }

    #[test]
    fn a_hidden_profile_keeps_its_flag() {
        let config = profile(&[
            (
                "connection",
                &[
                    ("type", owned(Value::from("802-11-wireless"))),
                    ("uuid", owned(Value::from("u"))),
                    ("id", owned(Value::from("quiet"))),
                    ("autoconnect", owned(Value::from(false))),
                ],
            ),
            (
                "802-11-wireless",
                &[
                    ("ssid", owned(Value::from(&b"quiet"[..]))),
                    ("hidden", owned(Value::from(true))),
                ],
            ),
        ]);
        let saved = saved_from_settings(&config, &no_active()).expect("wireless");
        assert!(saved.hidden);
        assert!(!saved.autoconnect, "an explicit false is honoured");
    }

    #[test]
    fn stored_key_management_maps_back_to_a_security_kind() {
        assert_eq!(security_from_key_mgmt(None), WifiSecurity::Open);
        assert_eq!(security_from_key_mgmt(Some("none")), WifiSecurity::Open);
        assert_eq!(security_from_key_mgmt(Some("owe")), WifiSecurity::Owe);
        assert_eq!(
            security_from_key_mgmt(Some("wpa-psk")),
            WifiSecurity::WpaPsk
        );
        assert_eq!(security_from_key_mgmt(Some("sae")), WifiSecurity::Sae);
        assert_eq!(
            security_from_key_mgmt(Some("wpa-eap")),
            WifiSecurity::Enterprise
        );
    }

    /// Every kind we will write must survive the round trip, or a saved
    /// network would come back as a different kind than it was stored as.
    #[test]
    fn the_kinds_we_write_round_trip_through_key_management() {
        for security in [WifiSecurity::Owe, WifiSecurity::WpaPsk, WifiSecurity::Sae] {
            let key_mgmt = security.key_mgmt().expect("writable kinds have a key-mgmt");
            assert_eq!(
                security_from_key_mgmt(Some(key_mgmt)),
                security,
                "{security:?} did not survive the round trip"
            );
        }
        // Open is the one whose `key_mgmt` is None by design.
        assert!(WifiSecurity::Open.key_mgmt().is_none());
        assert_eq!(security_from_key_mgmt(None), WifiSecurity::Open);
    }

    #[test]
    fn an_ssid_that_cannot_be_shown_is_left_out() {
        // Empty is a hidden network; invalid UTF-8 is unrenderable. Both are
        // legal on the air and neither belongs in a list of things to tap.
        assert_eq!(decode_ssid(b""), None);
        assert_eq!(decode_ssid(&[0xff, 0xfe, 0x00]), None);
        assert_eq!(decode_ssid(b"home").as_deref(), Some("home"));
    }

    #[test]
    fn frequencies_become_bands() {
        assert_eq!(band_ghz(2412), Some(2));
        assert_eq!(band_ghz(5220), Some(5));
        assert_eq!(band_ghz(6215), Some(6));
        assert_eq!(band_ghz(0), None);
    }

    #[test]
    fn the_boot_clock_is_readable_and_is_not_the_wall_clock() {
        // The bug this guards: comparing LastScan against SystemTime::now
        // yields an age of tens of thousands of seconds on a device that
        // scanned a moment ago.
        let boot = boottime_millis().expect("CLOCK_BOOTTIME is readable on Linux");
        assert!(boot > 0);
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(
            wall > boot,
            "boot time {boot} should be far smaller than wall time {wall}"
        );
    }

    /// Needs a live NetworkManager, so it is ignored by default like
    /// `network`'s equivalent. Run with `--ignored` on a device that has one.
    #[tokio::test]
    #[ignore = "requires a running NetworkManager"]
    async fn reads_this_device_over_dbus() {
        let reader = LinuxWifiReader::new();
        let snapshot = reader.networks().await;
        // Nothing asserted about content: a CI box may have no radio. What is
        // asserted is that the read completes and the fields are coherent.
        if snapshot.supported {
            assert!(
                snapshot.networks.iter().all(|n| !n.ssid.is_empty()),
                "a hidden network must not reach the list"
            );
        } else {
            assert!(snapshot.networks.is_empty());
        }
    }
}
