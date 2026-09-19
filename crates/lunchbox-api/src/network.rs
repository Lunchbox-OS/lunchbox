//! Read-only network status for the management UIs (issue #182).
//!
//! Answers one question, asked from a phone that has just paired over BLE and
//! knows nothing about the device's addressing: *where is this thing, and can I
//! reach it?* The web UI shows the same page, but over HTTP the client already
//! typed the address in — the companion is the consumer that cannot.
//!
//! Deliberately read-only. Joining a network, forgetting one, toggling an
//! adapter: none of that is here.
//!
//! **Not** the connectivity checks. Those already ride
//! [`crate::ServiceStateSnapshot::internet_status`], which every client holds
//! and which arrives with `InternetStatusChanged` deltas; duplicating them here
//! would give a UI two sources for one fact. A network page renders both.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};

/// Upper bound on how many interfaces travel on one status.
///
/// This crosses BLE, where a frame caps at 16 KiB
/// (`lunchbox_ble::protocol::MAX_FRAME_BYTES`). A device with more than a
/// handful of interfaces is a device running containers, and the ones past the
/// cap are the ones nobody is trying to reach — but the overflow is reported
/// rather than silently dropped, the same bargain
/// [`crate::DiagnosticSet`] makes.
pub const MAX_NETWORK_INTERFACES: usize = 16;

/// How much of the internet the host believes it can reach.
///
/// Mirrors NetworkManager's connectivity states, which are the only ones any
/// backend here can distinguish. A host with no NetworkManager reports
/// [`Connectivity::Unknown`] — which is honest, and different from
/// [`Connectivity::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Connectivity {
    /// Nobody could say. Not a claim that the device is offline.
    Unknown,
    /// No route to anywhere.
    None,
    /// A captive portal is intercepting traffic. Worth its own state: the
    /// device looks connected and nothing works, which is the single most
    /// confusing failure to debug remotely.
    Portal,
    /// A route exists but the connectivity probe did not complete.
    Limited,
    /// The host reached the internet.
    Full,
}

/// What kind of interface this is. Advisory: it drives presentation and which
/// addresses are offered as ways in, never policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NetworkInterfaceKind {
    /// A wireless interface. The one with a network name a person recognises.
    Wifi,
    /// A wired interface.
    Ethernet,
    /// A tunnel — WireGuard, ZeroTier, OpenVPN. Reachable, and on a device
    /// administered remotely often the *only* thing reachable, which is why
    /// `service.management_api.bind_retry_seconds` exists at all.
    Vpn,
    /// A container or VM bridge (`lxcbr0`, `docker0`). Has an address; that
    /// address is not a way in from the parent's phone.
    Bridge,
    /// The host talking to itself. Never a way in.
    Loopback,
    /// Something we could not name. Reported as a possible way in: being wrong
    /// about a veth costs a line in a list, while being wrong about a real
    /// interface costs the address somebody needed.
    Other,
}

/// Which family an address belongs to. Kept explicit rather than sniffed from
/// the string, so a UI grouping v4 above v6 does not have to count colons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AddressFamily {
    V4,
    V6,
}

/// One address on one interface.
///
/// Address and prefix are separate fields rather than one CIDR string because
/// the address alone is what gets copied into an SSH command, and a UI should
/// not have to split on `/` to offer that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NetworkAddressView {
    /// The address on its own, e.g. `192.168.0.139`.
    pub address: String,
    /// Prefix length in bits, e.g. `24`.
    pub prefix: u8,
    pub family: AddressFamily,
}

impl NetworkAddressView {
    /// `192.168.0.139/24`, for the places that want one string.
    pub fn cidr(&self) -> String {
        format!("{}/{}", self.address, self.prefix)
    }

    /// Whether this address is usable as a way in from another machine.
    ///
    /// Loopback is not. Nor is an IPv6 link-local: `fe80::…` needs a zone
    /// index that means something different on the phone than it does here, so
    /// offering one as a URL would hand somebody an address that cannot work.
    pub fn is_routable(&self) -> bool {
        match self.family {
            AddressFamily::V4 => !self.address.starts_with("127."),
            AddressFamily::V6 => {
                let a = self.address.to_ascii_lowercase();
                a != "::1" && !a.starts_with("fe80:")
            }
        }
    }

    /// The address as it appears inside a URL — IPv6 wants brackets.
    pub fn in_url(&self) -> String {
        match self.family {
            AddressFamily::V4 => self.address.clone(),
            AddressFamily::V6 => format!("[{}]", self.address),
        }
    }
}

/// The wireless network an interface is associated with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WifiView {
    /// The network's name.
    ///
    /// `None` when the interface is not associated — and also when the SSID is
    /// not valid UTF-8, which is legal: 802.11 carries an SSID as up to 32
    /// arbitrary octets, not a string. A name we cannot render is reported as
    /// no name rather than as mojibake.
    pub ssid: Option<String>,
    /// Signal quality, 0–100, as the driver reports it.
    pub signal_percent: Option<u8>,
    /// Channel centre frequency in MHz. A UI can turn 5220 into "5 GHz", which
    /// is the part a person debugging a weak signal actually wants.
    pub frequency_mhz: Option<u32>,
}

impl WifiView {
    /// `Some(2)` or `Some(5)` or `Some(6)` — the band in GHz, for a label.
    /// `None` when there is no frequency or it falls in no band we name.
    pub fn band_ghz(&self) -> Option<u8> {
        match self.frequency_mhz? {
            2_400..=2_500 => Some(2),
            4_900..=5_900 => Some(5),
            5_925..=7_125 => Some(6),
            _ => None,
        }
    }
}

/// One network interface as an administrator sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NetworkInterfaceView {
    /// Kernel name, e.g. `wlp3s0`.
    pub name: String,
    pub kind: NetworkInterfaceKind,
    /// Whether the interface is up and configured.
    pub up: bool,
    pub addresses: Vec<NetworkAddressView>,
    pub gateway: Option<String>,
    /// Nameservers configured for this interface.
    pub dns: Vec<String>,
    /// Present only on a wireless interface.
    pub wifi: Option<WifiView>,
    /// Whether an address here is a plausible way to reach this device from
    /// another machine on the same network.
    ///
    /// Derived, not reported by the host — see
    /// [`NetworkStatusView::new`]. A UI leads with these and folds the rest
    /// away: on a device running containers most interfaces are noise, and the
    /// one a parent needs is the one they will not find by scrolling.
    #[serde(default)]
    pub reachable: bool,
}

impl NetworkInterfaceView {
    /// The one address to offer as a way in to this interface.
    ///
    /// IPv4 if there is one, because it is the address somebody can read off a
    /// screen and retype; otherwise the first routable IPv6. `None` when
    /// nothing here is routable.
    pub fn preferred_address(&self) -> Option<&NetworkAddressView> {
        let routable = || self.addresses.iter().filter(|a| a.is_routable());
        routable()
            .find(|a| a.family == AddressFamily::V4)
            .or_else(|| routable().next())
    }

    /// Whether this interface offers a way in. See [`Self::reachable`], which
    /// is this, computed once and put on the wire.
    fn compute_reachable(&self) -> bool {
        if !self.up {
            return false;
        }
        match self.kind {
            // A container bridge has an address that means nothing off this
            // box, and a loopback address means nothing off this process.
            NetworkInterfaceKind::Loopback | NetworkInterfaceKind::Bridge => false,
            _ => self.addresses.iter().any(|a| a.is_routable()),
        }
    }
}

/// Whether the web management interface is up, and where.
///
/// The reason this is not simply the configured `bind`/`port`: the daemon
/// retries a bind that is not yet available (a ZeroTier interface still coming
/// up at login), and a bind that never succeeds only ever reached a log line.
/// From every UI, a device whose management API never came up looked exactly
/// like one that did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WebListenerState {
    /// `service.management_api` is absent or disabled. Nothing is wrong.
    Disabled,
    /// Configured, and still waiting for its address to exist.
    Binding,
    /// Serving.
    Listening,
    /// Configured and not serving. Somebody should know.
    Failed,
}

/// Where the web management interface is listening, if at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WebListenerView {
    pub state: WebListenerState,
    /// The socket address as configured, e.g. `0.0.0.0:8080`. `None` only when
    /// [`WebListenerState::Disabled`].
    pub addr: Option<String>,
    /// The port on its own, for building a URL against some other address.
    pub port: Option<u16>,
    /// Whether the listener terminates TLS (issue #156).
    ///
    /// Decides the scheme in [`NetworkStatusView::management_urls`], which is
    /// not cosmetic: a device serving HTTPS answers a plaintext request with a
    /// connection reset, so an `http://` URL for it sends a parent to debug
    /// their browser instead of opening their device.
    #[serde(default)]
    pub tls: bool,
    /// Why it is not serving. Only set with [`WebListenerState::Failed`].
    pub error: Option<String>,
}

impl WebListenerView {
    pub fn disabled() -> Self {
        Self {
            state: WebListenerState::Disabled,
            addr: None,
            port: None,
            tls: false,
            error: None,
        }
    }

    /// The scheme a URL against this listener needs.
    pub fn scheme(&self) -> &'static str {
        if self.tls { "https" } else { "http" }
    }
}

/// Where the status came from, so a UI can say why a field is missing rather
/// than rendering an empty box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NetworkSource {
    /// NetworkManager over D-Bus: everything below is available.
    NetworkManager,
    /// The kernel's interface list. Addresses are real; SSID, gateway, DNS and
    /// connectivity are not knowable this way and come back empty.
    Interfaces,
    /// Neither worked.
    Unavailable,
}

/// The whole read-out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NetworkStatusView {
    pub connectivity: Connectivity,
    pub source: NetworkSource,
    pub interfaces: Vec<NetworkInterfaceView>,
    /// Whether [`MAX_NETWORK_INTERFACES`] hid anything. UIs must say so.
    #[serde(default)]
    pub truncated: bool,
    pub management_api: WebListenerView,
    /// URLs that should open the web management interface, most useful first.
    ///
    /// Derived here rather than in each UI so the phone and the browser agree,
    /// and because the derivation is not obvious: a listener bound to
    /// `0.0.0.0` has no address of its own, so the answer is one URL per
    /// reachable address on the box — which is the whole point of the ticket.
    #[serde(default)]
    pub management_urls: Vec<String>,
}

impl NetworkStatusView {
    /// Assemble a status: derive `reachable` per interface, order them so the
    /// ones a person needs come first, apply the cap, and work out the
    /// management URLs.
    ///
    /// Providers hand over what the host told them and nothing more; every
    /// judgement about what it means is made once, here, where it is tested.
    pub fn new(
        connectivity: Connectivity,
        source: NetworkSource,
        mut interfaces: Vec<NetworkInterfaceView>,
        management_api: WebListenerView,
    ) -> Self {
        for iface in &mut interfaces {
            iface.reachable = iface.compute_reachable();
        }

        // Reachable first, then wireless (the one with a name a person
        // recognises), then by name so two snapshots of an unchanged device
        // are equal rather than merely equivalent.
        interfaces.sort_by(|a, b| {
            b.reachable
                .cmp(&a.reachable)
                .then_with(|| {
                    let rank = |k: NetworkInterfaceKind| match k {
                        NetworkInterfaceKind::Wifi => 0,
                        NetworkInterfaceKind::Ethernet => 1,
                        NetworkInterfaceKind::Vpn => 2,
                        NetworkInterfaceKind::Other => 3,
                        NetworkInterfaceKind::Bridge => 4,
                        NetworkInterfaceKind::Loopback => 5,
                    };
                    rank(a.kind).cmp(&rank(b.kind))
                })
                .then_with(|| a.name.cmp(&b.name))
        });

        let truncated = interfaces.len() > MAX_NETWORK_INTERFACES;
        interfaces.truncate(MAX_NETWORK_INTERFACES);

        let management_urls = management_urls(&management_api, &interfaces);

        Self {
            connectivity,
            source,
            interfaces,
            truncated,
            management_api,
            management_urls,
        }
    }

    /// Nothing could be determined, and the web listener is all we know.
    pub fn unavailable(management_api: WebListenerView) -> Self {
        Self::new(
            Connectivity::Unknown,
            NetworkSource::Unavailable,
            Vec::new(),
            management_api,
        )
    }
}

/// The URLs that should reach the web management interface.
///
/// A listener on a concrete address gives exactly one answer. A listener on
/// `0.0.0.0` or `::` gives one per reachable address on the device, because
/// that is what "listening on everything" means to somebody holding a phone.
/// A listener that is not serving gives none — an address that will refuse the
/// connection is worse than no address.
fn management_urls(listener: &WebListenerView, interfaces: &[NetworkInterfaceView]) -> Vec<String> {
    if listener.state != WebListenerState::Listening {
        return Vec::new();
    }
    let Some(port) = listener.port else {
        return Vec::new();
    };
    let Some(ip) = listener.addr.as_deref().and_then(parse_listen_ip) else {
        return Vec::new();
    };

    let scheme = listener.scheme();
    if !ip.is_unspecified() {
        return vec![format!("{scheme}://{}:{}", url_host(&ip), port)];
    }

    // One URL per interface, not one per address. A wireless interface on a
    // network with IPv6 carries a privacy-extension address per rotation on top
    // of its stable one, so "every routable address" is three unreadable
    // hex strings and the one a person wanted. IPv4 first for the same reason:
    // it is the one somebody can retype, and every address is still listed
    // under its interface for whoever needs another.
    interfaces
        .iter()
        .filter(|i| i.reachable)
        .filter_map(|i| i.preferred_address())
        .map(|a| format!("{scheme}://{}:{}", a.in_url(), port))
        .collect()
}

/// The IP out of a listener address, which is a `SocketAddr`'s `Display` form
/// (`0.0.0.0:8080`, `[::]:8080`) — but accept a bare IP too, so a caller that
/// records the configured `bind` rather than the bound socket still works.
fn parse_listen_ip(addr: &str) -> Option<IpAddr> {
    if let Ok(sock) = addr.parse::<SocketAddr>() {
        return Some(sock.ip());
    }
    addr.parse::<IpAddr>().ok()
}

/// An IP as it appears inside a URL — IPv6 wants brackets.
fn url_host(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(_) => ip.to_string(),
        IpAddr::V6(_) => format!("[{ip}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(address: &str) -> NetworkAddressView {
        NetworkAddressView {
            address: address.to_string(),
            prefix: 24,
            family: AddressFamily::V4,
        }
    }

    fn v6(address: &str) -> NetworkAddressView {
        NetworkAddressView {
            address: address.to_string(),
            prefix: 64,
            family: AddressFamily::V6,
        }
    }

    fn iface(
        name: &str,
        kind: NetworkInterfaceKind,
        addresses: Vec<NetworkAddressView>,
    ) -> NetworkInterfaceView {
        NetworkInterfaceView {
            name: name.to_string(),
            kind,
            up: true,
            addresses,
            gateway: None,
            dns: Vec::new(),
            wifi: None,
            reachable: false,
        }
    }

    /// A listener bound to `ip:port`, with `addr` in the form the daemon
    /// actually records: a `SocketAddr`'s `Display`, brackets and all.
    fn listening(ip: &str, port: u16) -> WebListenerView {
        let addr = SocketAddr::new(ip.parse().unwrap(), port);
        WebListenerView {
            state: WebListenerState::Listening,
            addr: Some(addr.to_string()),
            port: Some(port),
            tls: false,
            error: None,
        }
    }

    /// The same listener, serving TLS — which is what a device bound to
    /// anything but loopback does since issue #156.
    fn listening_tls(ip: &str, port: u16) -> WebListenerView {
        WebListenerView {
            tls: true,
            ..listening(ip, port)
        }
    }

    #[test]
    fn loopback_and_container_bridges_are_not_ways_in() {
        // The dev device's actual interface list: the answer a parent needs is
        // the wifi address, and everything else on the box is noise that looks
        // just as much like an address.
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![
                iface("lo", NetworkInterfaceKind::Loopback, vec![v4("127.0.0.1")]),
                iface("lxcbr0", NetworkInterfaceKind::Bridge, vec![v4("10.0.3.1")]),
                iface(
                    "wlx28187845b61d",
                    NetworkInterfaceKind::Wifi,
                    vec![v4("192.168.0.139")],
                ),
            ],
            listening("0.0.0.0", 8080),
        );

        let reachable: Vec<_> = status
            .interfaces
            .iter()
            .filter(|i| i.reachable)
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(reachable, vec!["wlx28187845b61d"]);
        assert_eq!(status.management_urls, vec!["http://192.168.0.139:8080"]);
    }

    #[test]
    fn a_wildcard_bind_offers_one_url_per_reachable_interface() {
        // Including the VPN one: on a remotely administered device that is
        // often the only address that works from where the parent is.
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![
                iface(
                    "wlan0",
                    NetworkInterfaceKind::Wifi,
                    vec![v4("192.168.0.139")],
                ),
                iface(
                    "ztks5unao4",
                    NetworkInterfaceKind::Vpn,
                    vec![v4("10.147.17.8")],
                ),
            ],
            listening("0.0.0.0", 8080),
        );
        assert_eq!(
            status.management_urls,
            vec!["http://192.168.0.139:8080", "http://10.147.17.8:8080"]
        );
    }

    #[test]
    fn a_concrete_bind_offers_exactly_itself() {
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![iface(
                "wlan0",
                NetworkInterfaceKind::Wifi,
                vec![v4("192.168.0.139")],
            )],
            listening("127.0.0.1", 8080),
        );
        assert_eq!(status.management_urls, vec!["http://127.0.0.1:8080"]);
    }

    #[test]
    fn a_tls_listener_advertises_https() {
        // The scheme is not cosmetic: since issue #156 a device bound to
        // anything but loopback serves TLS, and an `http://` URL for it gets a
        // connection reset rather than a redirect.
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![
                iface(
                    "wlan0",
                    NetworkInterfaceKind::Wifi,
                    vec![v4("192.168.0.139")],
                ),
                iface(
                    "eth0",
                    NetworkInterfaceKind::Ethernet,
                    vec![v6("2001:db8::5")],
                ),
            ],
            listening_tls("0.0.0.0", 8080),
        );
        assert_eq!(
            status.management_urls,
            vec!["https://192.168.0.139:8080", "https://[2001:db8::5]:8080"]
        );
    }

    #[test]
    fn a_concrete_tls_bind_advertises_https_too() {
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![iface(
                "wlan0",
                NetworkInterfaceKind::Wifi,
                vec![v4("192.168.0.139")],
            )],
            listening_tls("192.168.0.139", 8080),
        );
        assert_eq!(status.management_urls, vec!["https://192.168.0.139:8080"]);
    }

    #[test]
    fn a_listener_that_is_not_serving_offers_no_url() {
        // An address that will refuse the connection is worse than none: it
        // sends somebody debugging their phone instead of their device.
        for state in [
            WebListenerState::Disabled,
            WebListenerState::Binding,
            WebListenerState::Failed,
        ] {
            let status = NetworkStatusView::new(
                Connectivity::Full,
                NetworkSource::NetworkManager,
                vec![iface(
                    "wlan0",
                    NetworkInterfaceKind::Wifi,
                    vec![v4("192.168.0.139")],
                )],
                WebListenerView {
                    state,
                    addr: Some("0.0.0.0:8080".into()),
                    port: Some(8080),
                    tls: false,
                    error: None,
                },
            );
            assert!(
                status.management_urls.is_empty(),
                "{state:?} must not advertise a URL"
            );
        }
    }

    #[test]
    fn ipv6_urls_are_bracketed_and_link_local_is_left_out() {
        // fe80:: needs a zone index, and the phone's zone is not the device's.
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![iface(
                "wlan0",
                NetworkInterfaceKind::Wifi,
                vec![v6("fe80::1"), v6("2001:db8::5")],
            )],
            listening("::", 8080),
        );
        assert_eq!(status.management_urls, vec!["http://[2001:db8::5]:8080"]);
    }

    #[test]
    fn one_url_per_interface_not_one_per_address() {
        // The shape the dev device actually returns: a wireless interface with
        // a stable IPv6 address and two privacy-extension temporaries beside
        // its IPv4 one. Five URLs, four of them unreadable, is a worse answer
        // than the one a person came for.
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![
                iface(
                    "wlan0",
                    NetworkInterfaceKind::Wifi,
                    vec![
                        v4("192.168.0.139"),
                        v6("fdc8:7de5:153f:3389:6065:7fb0:1743:2e1e"),
                        v6("fdc8:7de5:153f:3389:cea0:f537:5cfd:ca6a"),
                        v6("fe80::f72b:9c0:98b0:e2f"),
                    ],
                ),
                iface(
                    "ztks5unao4",
                    NetworkInterfaceKind::Vpn,
                    vec![v4("172.27.154.85"), v6("fe80::b066:94ff:fe26:4b79")],
                ),
            ],
            listening("0.0.0.0", 8080),
        );
        assert_eq!(
            status.management_urls,
            vec!["http://192.168.0.139:8080", "http://172.27.154.85:8080"],
            "the two ways in, not every address on them"
        );
        // The rest are still on the page, under their interface.
        assert_eq!(status.interfaces[0].addresses.len(), 4);
    }

    #[test]
    fn an_interface_with_only_a_link_local_address_is_not_a_way_in() {
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![iface(
                "wlan0",
                NetworkInterfaceKind::Wifi,
                vec![v6("fe80::1")],
            )],
            listening("::", 8080),
        );
        assert!(!status.interfaces[0].reachable);
        assert!(status.management_urls.is_empty());
    }

    #[test]
    fn a_down_interface_is_not_a_way_in_however_many_addresses_it_has() {
        let mut down = iface(
            "wlan0",
            NetworkInterfaceKind::Wifi,
            vec![v4("192.168.0.139")],
        );
        down.up = false;
        let status = NetworkStatusView::new(
            Connectivity::None,
            NetworkSource::NetworkManager,
            vec![down],
            listening("0.0.0.0", 8080),
        );
        assert!(!status.interfaces[0].reachable);
        assert!(status.management_urls.is_empty());
    }

    #[test]
    fn reachable_interfaces_sort_first_and_the_order_is_stable() {
        // A client diffing two reads must see real changes, not reordering.
        let build = || {
            NetworkStatusView::new(
                Connectivity::Full,
                NetworkSource::NetworkManager,
                vec![
                    iface("lo", NetworkInterfaceKind::Loopback, vec![v4("127.0.0.1")]),
                    iface("zt0", NetworkInterfaceKind::Vpn, vec![v4("10.147.17.8")]),
                    iface(
                        "wlan0",
                        NetworkInterfaceKind::Wifi,
                        vec![v4("192.168.0.139")],
                    ),
                    iface(
                        "eth0",
                        NetworkInterfaceKind::Ethernet,
                        vec![v4("192.168.0.9")],
                    ),
                ],
                listening("0.0.0.0", 8080),
            )
        };
        let names: Vec<_> = build().interfaces.iter().map(|i| i.name.clone()).collect();
        assert_eq!(names, vec!["wlan0", "eth0", "zt0", "lo"]);
        assert_eq!(build(), build());
    }

    #[test]
    fn the_interface_cap_admits_itself() {
        let many: Vec<_> = (0..MAX_NETWORK_INTERFACES + 4)
            .map(|i| iface(&format!("veth{i:02}"), NetworkInterfaceKind::Other, vec![]))
            .collect();
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            many,
            WebListenerView::disabled(),
        );
        assert_eq!(status.interfaces.len(), MAX_NETWORK_INTERFACES);
        assert!(
            status.truncated,
            "a UI must be able to say it is not showing all"
        );
    }

    #[test]
    fn bands_are_named_only_where_we_know_them() {
        let at = |mhz| {
            WifiView {
                ssid: None,
                signal_percent: None,
                frequency_mhz: Some(mhz),
            }
            .band_ghz()
        };
        assert_eq!(at(2_412), Some(2));
        assert_eq!(at(5_220), Some(5));
        assert_eq!(at(6_195), Some(6));
        assert_eq!(at(900), None);
    }

    #[test]
    fn addresses_render_both_ways() {
        assert_eq!(v4("192.168.0.139").cidr(), "192.168.0.139/24");
        assert_eq!(v6("2001:db8::5").in_url(), "[2001:db8::5]");
        assert_eq!(v4("192.168.0.139").in_url(), "192.168.0.139");
    }

    #[test]
    fn the_status_round_trips_as_json() {
        let status = NetworkStatusView::new(
            Connectivity::Full,
            NetworkSource::NetworkManager,
            vec![NetworkInterfaceView {
                wifi: Some(WifiView {
                    ssid: Some("SHEPHERD-04-HP OfficeJet 250".into()),
                    signal_percent: Some(60),
                    frequency_mhz: Some(5_220),
                }),
                ..iface(
                    "wlan0",
                    NetworkInterfaceKind::Wifi,
                    vec![v4("192.168.0.139")],
                )
            }],
            listening("0.0.0.0", 8080),
        );
        let json = serde_json::to_string(&status).unwrap();
        let parsed: NetworkStatusView = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, status);
    }
}
