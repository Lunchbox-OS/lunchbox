//! Reading this device's networking, for the management UIs (issue #182).
//!
//! Two backends, in order of how much they can say:
//!
//! 1. **NetworkManager over D-Bus.** Everything: connectivity, per-device
//!    kinds, addresses, gateway, DNS, and the SSID of the network a wireless
//!    interface is associated with. `lunchboxd` already depends on this bus
//!    for the suspend cover and the connectivity re-check, so it is not a new
//!    dependency, and every property read here is readable by an unprivileged
//!    user — no polkit prompt, nothing to configure.
//! 2. **`getifaddrs`.** Interface names and addresses. No SSID, no gateway, no
//!    DNS, no connectivity — but the addresses are the part the ticket is
//!    actually about, so a device with no NetworkManager still answers "where
//!    am I".
//!
//! Falling back is normal, not an error path: the same "missing D-Bus /
//! NetworkManager is non-fatal" bargain `lunchboxd`'s system-event watcher
//! already makes. What must never happen is a status page that hangs, so the
//! D-Bus attempt is bounded by a timeout and the fallback runs on any failure.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use async_trait::async_trait;
use lunchbox_api::{
    AddressFamily, Connectivity, NetworkAddressView, NetworkInterfaceKind, NetworkInterfaceView,
    NetworkSource, WifiView,
};
use lunchbox_host_api::{NetworkInfoProvider, NetworkSnapshot};
use tracing::{debug, warn};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// How long the whole NetworkManager read gets before we give up and use the
/// kernel's interface list instead.
///
/// This sits behind a management RPC that a UI polls while a page is open, so
/// a wedged system bus must degrade the answer rather than stall the caller.
const DBUS_TIMEOUT: Duration = Duration::from_secs(3);

// NetworkManager's `NMConnectivityState`.
const NM_CONNECTIVITY_UNKNOWN: u32 = 0;
const NM_CONNECTIVITY_NONE: u32 = 1;
const NM_CONNECTIVITY_PORTAL: u32 = 2;
const NM_CONNECTIVITY_LIMITED: u32 = 3;
const NM_CONNECTIVITY_FULL: u32 = 4;

// The `NMDeviceType` values we can name. Everything else is `Other`, which is
// reported as a possible way in — being wrong about a veth costs a line in a
// list, while being wrong about a real interface costs the address somebody
// needed.
const NM_DEVICE_TYPE_ETHERNET: u32 = 1;
const NM_DEVICE_TYPE_WIFI: u32 = 2;
const NM_DEVICE_TYPE_BRIDGE: u32 = 13;
const NM_DEVICE_TYPE_TUN: u32 = 16;
const NM_DEVICE_TYPE_VETH: u32 = 20;
const NM_DEVICE_TYPE_WIREGUARD: u32 = 29;
const NM_DEVICE_TYPE_LOOPBACK: u32 = 32;

/// `NM_DEVICE_STATE_ACTIVATED`. Anything short of this is an interface that
/// exists and is not carrying traffic.
const NM_DEVICE_STATE_ACTIVATED: u32 = 100;

/// An object path of `/` is NetworkManager's null: no IP config, no access
/// point. Building a proxy for it succeeds and every read then fails, so it
/// has to be checked rather than caught.
const NM_NULL_PATH: &str = "/";

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    /// `NMConnectivityState`: how much of the internet NM believes it reached.
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;

    /// Every device NM knows about, including ones it does not manage.
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    /// Kernel name, e.g. `wlp3s0`.
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;

    /// `NMDeviceType`.
    #[zbus(property, name = "DeviceType")]
    fn device_type(&self) -> zbus::Result<u32>;

    /// `NMDeviceState`.
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;

    #[zbus(property, name = "Ip4Config")]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;

    #[zbus(property, name = "Ip6Config")]
    fn ip6_config(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait DeviceWireless {
    /// The AP this interface is associated with, or the null path.
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    /// Up to 32 arbitrary octets. 802.11 does not promise a string, and this
    /// one is not decoded as one without checking.
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    /// Signal quality, 0–100.
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;

    /// Channel centre frequency in MHz.
    #[zbus(property)]
    fn frequency(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip4Config {
    /// `aa{sv}` with `address` (string) and `prefix` (uint32) per entry.
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;

    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;

    /// `aa{sv}` with `address` per entry.
    #[zbus(property)]
    fn nameserver_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.IP6Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip6Config {
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;

    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn nameserver_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}

/// The Linux [`NetworkInfoProvider`]: NetworkManager if it answers, the
/// kernel's interface list if it does not.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxNetworkInfo;

impl LinuxNetworkInfo {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NetworkInfoProvider for LinuxNetworkInfo {
    async fn snapshot(&self) -> NetworkSnapshot {
        match tokio::time::timeout(DBUS_TIMEOUT, from_network_manager()).await {
            Ok(Ok(snapshot)) => return snapshot,
            Ok(Err(err)) => {
                debug!(error = %err, "NetworkManager unavailable; reading interfaces directly");
            }
            Err(_) => {
                warn!(
                    timeout_s = DBUS_TIMEOUT.as_secs(),
                    "NetworkManager did not answer in time; reading interfaces directly"
                );
            }
        }

        match interfaces_from_kernel() {
            Ok(interfaces) => NetworkSnapshot {
                connectivity: Connectivity::Unknown,
                source: NetworkSource::Interfaces,
                interfaces,
            },
            Err(err) => {
                warn!(error = %err, "Could not read this device's network interfaces");
                NetworkSnapshot::unavailable()
            }
        }
    }
}

/// The full read-out, from NetworkManager.
///
/// A connection per call rather than a cached one: a status page is polled
/// while somebody is looking at it and idle the rest of the time, and a fresh
/// connection is how an NM or dbus restart heals itself without a reconnect
/// loop to own. Property caching is off for the same reason — this reads each
/// object once and would otherwise subscribe to changes on every access point
/// and IP config it touches.
async fn from_network_manager() -> zbus::Result<NetworkSnapshot> {
    let conn = zbus::Connection::system().await?;
    let nm = NetworkManagerProxy::builder(&conn)
        .cache_properties(CacheProperties::No)
        .build()
        .await?;

    let connectivity = match nm.connectivity().await {
        Ok(NM_CONNECTIVITY_NONE) => Connectivity::None,
        Ok(NM_CONNECTIVITY_PORTAL) => Connectivity::Portal,
        Ok(NM_CONNECTIVITY_LIMITED) => Connectivity::Limited,
        Ok(NM_CONNECTIVITY_FULL) => Connectivity::Full,
        Ok(NM_CONNECTIVITY_UNKNOWN) => Connectivity::Unknown,
        Ok(other) => {
            debug!(
                state = other,
                "Unrecognised NetworkManager connectivity state"
            );
            Connectivity::Unknown
        }
        // Losing the overall state is not worth losing every address over.
        Err(err) => {
            debug!(error = %err, "Could not read NetworkManager connectivity");
            Connectivity::Unknown
        }
    };

    let mut interfaces = Vec::new();
    for path in nm.devices().await? {
        match read_device(&conn, &path).await {
            Ok(iface) => interfaces.push(iface),
            // One unreadable device must not cost the others. A device can
            // also disappear between the list and the read, which is a race
            // rather than a fault.
            Err(err) => debug!(device = %path.as_str(), error = %err, "Skipping unreadable device"),
        }
    }

    Ok(NetworkSnapshot {
        connectivity,
        source: NetworkSource::NetworkManager,
        interfaces,
    })
}

async fn read_device(
    conn: &zbus::Connection,
    path: &OwnedObjectPath,
) -> zbus::Result<NetworkInterfaceView> {
    let device = DeviceProxy::builder(conn)
        .path(path)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;

    let name = device.interface().await?;
    let device_type = device.device_type().await.unwrap_or(0);
    let kind = interface_kind(device_type);
    let up = device.state().await.unwrap_or(0) == NM_DEVICE_STATE_ACTIVATED;

    let mut addresses = Vec::new();
    let mut gateway = None;
    let mut dns = Vec::new();

    if let Ok(ip4) = device.ip4_config().await
        && ip4.as_str() != NM_NULL_PATH
        && let Ok(proxy) = Ip4ConfigProxy::builder(conn)
            .path(&ip4)?
            .cache_properties(CacheProperties::No)
            .build()
            .await
    {
        if let Ok(data) = proxy.address_data().await {
            addresses.extend(parse_addresses(&data, AddressFamily::V4));
        }
        gateway = proxy.gateway().await.ok().filter(|g| !g.is_empty());
        if let Ok(servers) = proxy.nameserver_data().await {
            dns.extend(parse_nameservers(&servers));
        }
    }

    if let Ok(ip6) = device.ip6_config().await
        && ip6.as_str() != NM_NULL_PATH
        && let Ok(proxy) = Ip6ConfigProxy::builder(conn)
            .path(&ip6)?
            .cache_properties(CacheProperties::No)
            .build()
            .await
    {
        if let Ok(data) = proxy.address_data().await {
            addresses.extend(parse_addresses(&data, AddressFamily::V6));
        }
        if gateway.is_none() {
            gateway = proxy.gateway().await.ok().filter(|g| !g.is_empty());
        }
        if let Ok(servers) = proxy.nameserver_data().await {
            dns.extend(parse_nameservers(&servers));
        }
    }

    let wifi = if kind == NetworkInterfaceKind::Wifi {
        read_wifi(conn, path).await
    } else {
        None
    };

    Ok(NetworkInterfaceView {
        name,
        kind,
        up,
        addresses,
        gateway,
        dns,
        wifi,
        reachable: false,
    })
}

/// The network a wireless interface is associated with, if any. Every failure
/// here is "no wifi details", never "no interface".
async fn read_wifi(conn: &zbus::Connection, path: &OwnedObjectPath) -> Option<WifiView> {
    let wireless = DeviceWirelessProxy::builder(conn)
        .path(path)
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()?;

    let ap_path = wireless.active_access_point().await.ok()?;
    if ap_path.as_str() == NM_NULL_PATH {
        // Associated with nothing. The interface still belongs on the list.
        return Some(WifiView {
            ssid: None,
            signal_percent: None,
            frequency_mhz: None,
        });
    }

    let ap = AccessPointProxy::builder(conn)
        .path(&ap_path)
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()?;

    Some(WifiView {
        ssid: ap.ssid().await.ok().and_then(|bytes| decode_ssid(&bytes)),
        signal_percent: ap.strength().await.ok(),
        frequency_mhz: ap.frequency().await.ok(),
    })
}

/// An SSID is up to 32 arbitrary octets, so this can legitimately fail.
///
/// A name we cannot render is reported as no name: mojibake in the one field a
/// parent uses to recognise their own network is worse than an honest blank.
fn decode_ssid(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        // A hidden network beacons a zero-length SSID.
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn interface_kind(device_type: u32) -> NetworkInterfaceKind {
    match device_type {
        NM_DEVICE_TYPE_ETHERNET => NetworkInterfaceKind::Ethernet,
        NM_DEVICE_TYPE_WIFI => NetworkInterfaceKind::Wifi,
        NM_DEVICE_TYPE_BRIDGE | NM_DEVICE_TYPE_VETH => NetworkInterfaceKind::Bridge,
        NM_DEVICE_TYPE_TUN | NM_DEVICE_TYPE_WIREGUARD => NetworkInterfaceKind::Vpn,
        NM_DEVICE_TYPE_LOOPBACK => NetworkInterfaceKind::Loopback,
        _ => NetworkInterfaceKind::Other,
    }
}

/// `AddressData` entries into addresses, dropping any that is missing the two
/// fields that make it one.
fn parse_addresses(
    data: &[HashMap<String, OwnedValue>],
    family: AddressFamily,
) -> Vec<NetworkAddressView> {
    data.iter()
        .filter_map(|entry| {
            Some(NetworkAddressView {
                address: as_string(entry.get("address")?)?,
                prefix: u8::try_from(as_u32(entry.get("prefix")?)?).ok()?,
                family,
            })
        })
        .collect()
}

fn parse_nameservers(data: &[HashMap<String, OwnedValue>]) -> Vec<String> {
    data.iter()
        .filter_map(|entry| as_string(entry.get("address")?))
        .collect()
}

fn as_string(value: &OwnedValue) -> Option<String> {
    match &**value {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn as_u32(value: &OwnedValue) -> Option<u32> {
    match &**value {
        Value::U32(n) => Some(*n),
        Value::U8(n) => Some(u32::from(*n)),
        Value::I32(n) => u32::try_from(*n).ok(),
        _ => None,
    }
}

/// Interfaces and addresses straight from the kernel, for a host with no
/// NetworkManager. No SSID, no gateway, no DNS — those have no answer here,
/// and [`NetworkSource::Interfaces`] is what tells a UI to stop looking for
/// them.
fn interfaces_from_kernel() -> nix::Result<Vec<NetworkInterfaceView>> {
    use nix::ifaddrs::getifaddrs;
    use nix::net::if_::InterfaceFlags;

    // Preserve first-seen order so the result is stable between reads.
    let mut order: Vec<String> = Vec::new();
    let mut by_name: HashMap<String, NetworkInterfaceView> = HashMap::new();

    for ifaddr in getifaddrs()? {
        let name = ifaddr.interface_name.clone();
        let entry = by_name.entry(name.clone()).or_insert_with(|| {
            order.push(name.clone());
            NetworkInterfaceView {
                kind: if ifaddr.flags.contains(InterfaceFlags::IFF_LOOPBACK) {
                    NetworkInterfaceKind::Loopback
                } else {
                    kind_from_name(&name)
                },
                up: ifaddr.flags.contains(InterfaceFlags::IFF_UP)
                    && ifaddr.flags.contains(InterfaceFlags::IFF_RUNNING),
                name,
                addresses: Vec::new(),
                gateway: None,
                dns: Vec::new(),
                wifi: None,
                reachable: false,
            }
        });

        let Some(address) = ifaddr.address.as_ref() else {
            // A packet socket entry (AF_PACKET) has no IP. It still told us
            // the interface exists, which is why the entry is created above.
            continue;
        };

        if let Some(v4) = address.as_sockaddr_in() {
            let prefix = ifaddr
                .netmask
                .as_ref()
                .and_then(|m| m.as_sockaddr_in())
                .map(|m| prefix_len_v4(m.ip()))
                .unwrap_or(32);
            entry.addresses.push(NetworkAddressView {
                address: v4.ip().to_string(),
                prefix,
                family: AddressFamily::V4,
            });
        } else if let Some(v6) = address.as_sockaddr_in6() {
            let prefix = ifaddr
                .netmask
                .as_ref()
                .and_then(|m| m.as_sockaddr_in6())
                .map(|m| prefix_len_v6(m.ip()))
                .unwrap_or(128);
            entry.addresses.push(NetworkAddressView {
                address: v6.ip().to_string(),
                prefix,
                family: AddressFamily::V6,
            });
        }
    }

    Ok(order
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect())
}

/// Interface kind from its name, which is all the kernel offers.
///
/// Predictable network interface names make this far less of a guess than it
/// sounds: `wl*` is wireless, `en*`/`eth*` wired, and the tunnel and bridge
/// prefixes are set by the software that creates them.
fn kind_from_name(name: &str) -> NetworkInterfaceKind {
    const BRIDGES: [&str; 5] = ["br", "docker", "virbr", "lxcbr", "veth"];
    const TUNNELS: [&str; 4] = ["zt", "tun", "tap", "wg"];

    if name.starts_with("wl") {
        NetworkInterfaceKind::Wifi
    } else if name.starts_with("en") || name.starts_with("eth") {
        NetworkInterfaceKind::Ethernet
    } else if TUNNELS.iter().any(|p| name.starts_with(p)) {
        NetworkInterfaceKind::Vpn
    } else if BRIDGES.iter().any(|p| name.starts_with(p)) {
        NetworkInterfaceKind::Bridge
    } else {
        NetworkInterfaceKind::Other
    }
}

/// Leading ones in an IPv6 netmask.
fn prefix_len_v6(mask: Ipv6Addr) -> u8 {
    mask.octets().iter().map(|b| b.count_ones() as u8).sum()
}

/// Leading ones in an IPv4 netmask.
fn prefix_len_v4(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_types_that_matter_are_named() {
        assert_eq!(interface_kind(1), NetworkInterfaceKind::Ethernet);
        assert_eq!(interface_kind(2), NetworkInterfaceKind::Wifi);
        assert_eq!(interface_kind(13), NetworkInterfaceKind::Bridge);
        assert_eq!(interface_kind(32), NetworkInterfaceKind::Loopback);
        // ZeroTier presents as a TUN device; it is a way in, not a bridge.
        assert_eq!(interface_kind(16), NetworkInterfaceKind::Vpn);
        assert_eq!(interface_kind(29), NetworkInterfaceKind::Vpn);
    }

    #[test]
    fn an_unknown_device_type_stays_a_possible_way_in() {
        // Being wrong about a veth costs a line in a list; being wrong about a
        // real interface costs the address somebody needed.
        assert_eq!(interface_kind(9_999), NetworkInterfaceKind::Other);
    }

    #[test]
    fn an_ssid_that_is_not_text_is_reported_as_no_name() {
        // 802.11 carries 32 arbitrary octets. Mojibake in the one field a
        // parent uses to recognise their network is worse than a blank.
        assert_eq!(
            decode_ssid(b"SHEPHERD-04-HP OfficeJet 250").as_deref(),
            Some("SHEPHERD-04-HP OfficeJet 250")
        );
        assert_eq!(decode_ssid(&[0xff, 0xfe, 0x00]), None);
        assert_eq!(decode_ssid(b""), None, "a hidden network beacons no SSID");
    }

    #[test]
    fn addresses_come_out_of_networkmanagers_dictionaries() {
        let entry = HashMap::from([
            (
                "address".to_string(),
                OwnedValue::try_from(Value::from("192.168.0.139")).unwrap(),
            ),
            (
                "prefix".to_string(),
                OwnedValue::try_from(Value::U32(24)).unwrap(),
            ),
        ]);
        let parsed = parse_addresses(&[entry], AddressFamily::V4);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].address, "192.168.0.139");
        assert_eq!(parsed[0].prefix, 24);
    }

    #[test]
    fn a_malformed_address_entry_is_dropped_not_guessed() {
        let no_prefix = HashMap::from([(
            "address".to_string(),
            OwnedValue::try_from(Value::from("192.168.0.139")).unwrap(),
        )]);
        assert!(parse_addresses(&[no_prefix], AddressFamily::V4).is_empty());
    }

    #[test]
    fn netmasks_become_prefix_lengths() {
        assert_eq!(prefix_len_v4(Ipv4Addr::new(255, 255, 255, 0)), 24);
        assert_eq!(prefix_len_v4(Ipv4Addr::new(255, 255, 255, 255)), 32);
        assert_eq!(prefix_len_v4(Ipv4Addr::new(0, 0, 0, 0)), 0);
        assert_eq!(prefix_len_v6("ffff:ffff:ffff:ffff::".parse().unwrap()), 64);
    }

    #[test]
    fn interface_names_are_a_usable_last_resort_for_kind() {
        assert_eq!(kind_from_name("wlp3s0"), NetworkInterfaceKind::Wifi);
        assert_eq!(kind_from_name("enp0s31f6"), NetworkInterfaceKind::Ethernet);
        assert_eq!(kind_from_name("ztks5unao4"), NetworkInterfaceKind::Vpn);
        assert_eq!(kind_from_name("lxcbr0"), NetworkInterfaceKind::Bridge);
        assert_eq!(kind_from_name("weird0"), NetworkInterfaceKind::Other);
    }

    /// Against the real system bus. Ignored by default — CI containers have no
    /// NetworkManager, and this asserts the one thing unit tests cannot: that
    /// the property names and types above match what NM actually publishes.
    ///
    /// `cargo test -p lunchbox-host-linux -- --ignored --nocapture reads_this_device`
    #[tokio::test]
    #[ignore = "needs a running NetworkManager on the system bus"]
    async fn reads_this_device_over_dbus() {
        let snapshot = from_network_manager().await.expect("NetworkManager");
        println!("connectivity: {:?}", snapshot.connectivity);
        for iface in &snapshot.interfaces {
            println!(
                "{:<24} {:?} up={} {:?} gw={:?} dns={:?} wifi={:?}",
                iface.name,
                iface.kind,
                iface.up,
                iface.addresses.iter().map(|a| a.cidr()).collect::<Vec<_>>(),
                iface.gateway,
                iface.dns,
                iface.wifi,
            );
        }
        assert!(
            !snapshot.interfaces.is_empty(),
            "a host running NetworkManager has at least a loopback device"
        );
    }

    #[test]
    fn the_kernel_always_knows_about_loopback() {
        // The fallback has to work on any Linux, including a CI container with
        // no NetworkManager — which is exactly when it is load-bearing.
        let interfaces = interfaces_from_kernel().expect("getifaddrs");
        let lo = interfaces
            .iter()
            .find(|i| i.name == "lo")
            .expect("every Linux has a loopback interface");
        assert_eq!(lo.kind, NetworkInterfaceKind::Loopback);
        assert!(
            lo.addresses.iter().any(|a| a.address == "127.0.0.1"),
            "loopback carries 127.0.0.1, got {:?}",
            lo.addresses
        );
        assert!(
            interfaces.iter().all(|i| i.wifi.is_none()),
            "the kernel cannot report an SSID and must not pretend to"
        );
    }
}
