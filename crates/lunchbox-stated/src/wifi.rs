//! Writing wireless profiles, at a uid the activities do not have (issue #194).
//!
//! ## Why this is the custodian's job
//!
//! Writing a NetworkManager profile needs polkit's
//! `org.freedesktop.NetworkManager.settings.modify.system`, and joining one
//! needs `org.freedesktop.NetworkManager.network-control` too. Ubuntu grants
//! the first to local, active members of `sudo` or `netdev`, and the kiosk user is in
//! neither — deliberately, because **every activity runs as the kiosk user**.
//! Adding it to `netdev` would hand the grant to every activity, and that
//! grant is not narrow: it was measured to be sufficient on its own for
//! `GetSecrets`, which reads back every saved password. A child with a shell
//! would learn the house WiFi key.
//!
//! So the grant goes here instead, to a uid no activity has, and lunchboxd
//! asks. What this widens, said plainly: `lunchbox-state` may change **any**
//! NetworkManager setting, not only wireless ones — polkit cannot express the
//! narrower permission. The mitigation is the shape of the socket, not the
//! shape of the rule: the protocol accepts
//! [`lunchbox_api::WifiJoinRequest`] and two UUIDs, and **never a settings
//! dictionary**, so there is no way to ask this daemon to write a setting it
//! was not written to write.
//!
//! ## The join is owned here, end to end
//!
//! The scope note proposed splitting it — custodian starts the activation,
//! lunchboxd watches it, custodian persists on success — to keep each request
//! short. This does it all here instead, because the requests stay short
//! anyway: [`WifiCustodian::save`] returns as soon as NetworkManager has
//! *accepted* the activation, and the watching happens on a task of the
//! custodian's own. The alternative spreads one stateful transaction across
//! two processes and makes the custodian remember which volatile profiles it
//! created, on behalf of a client that could crash between the two calls and
//! leave a profile nobody persists or removes.
//!
//! ## Measured, not assumed
//!
//! Everything about NetworkManager's behaviour here was established against
//! 1.54.3; see
//! `docs/ai/history/2026-09-21 004 wifi-against-real-networkmanager (#194).md`.
//! The four that shaped this code:
//!
//! * **The failure reason is on the `Device`, not the active connection.** The
//!   active connection reports `DEACTIVATED / DEVICE_DISCONNECTED` for a wrong
//!   password, a missing network and a dead DHCP server alike.
//! * **A join must be volatile first, then persisted.** A profile written
//!   straight to disk with a wrong password autoconnects forever; a volatile
//!   one NetworkManager discards itself when the activation fails.
//! * **Saving over an existing SSID must not leave two.** `AddConnection2`
//!   will happily create a second profile with the *same name* and a new
//!   UUID, which no list can tell apart. A save for later updates in place; a
//!   join tries the new password on a volatile profile and replaces the saved
//!   one only once it has worked.
//! * **Subscribe before activating.** A join can settle in 3 seconds, so a
//!   watcher started afterwards can miss the whole thing.
//!
//! And two that only an installed device showed (see the #218 validation
//! note):
//!
//! * **Only an accepted activation is a join.** The watcher is *spawned* once
//!   NetworkManager has accepted the call, off a stream subscribed before it.
//!   Marking the join in progress any earlier left a refused call reporting
//!   `connecting` for good.
//! * **Only the newest join may report.** Device signals are per device, not
//!   per join, so a watcher left over from an earlier attempt sees the next
//!   one's outcome — or times out in the middle of it. Each join carries a
//!   generation, and a superseded watcher stops without writing anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use lunchbox_api::{
    SavedWifiNetwork, WifiJoinFailure, WifiJoinFailureKind, WifiJoinRequest, WifiJoinState,
};
use lunchbox_state_proto::WifiAuthorityReply;
use tracing::{debug, info, warn};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

/// The polkit action that gates writing a system connection profile.
const ACTION_MODIFY_SYSTEM: &str = "org.freedesktop.NetworkManager.settings.modify.system";

/// The polkit action that gates activating one, `AddAndActivateConnection2`
/// included.
///
/// polkit grants this to any active local session, which is why every
/// measurement taken from a logged-in shell passed without it. This daemon has
/// no session, so it needs the grant as much as the first: without it a
/// network can be saved for later and never joined.
const ACTION_NETWORK_CONTROL: &str = "org.freedesktop.NetworkManager.network-control";

/// Every action the custodian's wireless requests need, in the order a
/// refusal names them.
const ACTIONS: [&str; 2] = [ACTION_MODIFY_SYSTEM, ACTION_NETWORK_CONTROL];

/// Where the rule that grants it lives, for the message a missing grant
/// produces.
const RULES_FILE: &str = "/etc/polkit-1/rules.d/50-lunchbox-network.rules";

const NM_DEVICE_TYPE_WIFI: u32 = 2;
const NM_SETTING_WIRELESS: &str = "802-11-wireless";
const NM_NULL_PATH: &str = "/";

/// `NM_NULL_PATH` as an `ObjectPath`, for the arguments D-Bus types `o`.
///
/// A plain `&str` compiles and then fails at call time with `Type of message,
/// "(oos)", does not match expected type "(ooo)"`. Nothing short of a real bus
/// catches it.
fn null_path() -> ObjectPath<'static> {
    ObjectPath::try_from(NM_NULL_PATH).expect("\"/\" is a valid object path")
}

/// `NM_SETTINGS_UPDATE2_FLAG_TO_DISK`.
const UPDATE2_TO_DISK: u32 = 0x1;

/// `NMDeviceState` values this code reacts to.
const NM_DEVICE_STATE_ACTIVATED: u32 = 100;
const NM_DEVICE_STATE_FAILED: u32 = 120;

/// `NMDeviceStateReason`, the ones a join can end on.
///
/// These four are the measured outcomes. Note that 8 (`SUPPLICANT_DISCONNECT`)
/// is **not** here: it appears on the wrong-password path as an intermediate
/// `config -> need-auth` hop, never as a terminal reason, and treating it as
/// one would report a failure while the supplicant was still retrying.
const REASON_IP_CONFIG_UNAVAILABLE: u32 = 5;
const REASON_NO_SECRETS: u32 = 7;
const REASON_SSID_NOT_FOUND: u32 = 53;

/// How long a join is watched before it is called a timeout.
///
/// The measured spread is 3 to 45 seconds, the last being a DHCP server that
/// never answers. 90 gives that headroom twice over; past it, something is
/// wrong that a longer wait will not fix, and a UI stuck on "connecting"
/// forever is worse than one that says it gave up.
const JOIN_TIMEOUT: Duration = Duration::from_secs(90);

/// How long any single NetworkManager call gets.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Add a profile and activate it in one call, so there is no window in
    /// which a half-made profile exists.
    ///
    /// `options` carries `persist`, which is the whole reason this is used
    /// instead of `AddConnection2` followed by `ActivateConnection`.
    ///
    /// Returns `(connection settings path, active connection path, result)` —
    /// **settings first**. Getting that order wrong subscribes a watcher to
    /// the wrong object and yields no signals at all, silently.
    #[zbus(name = "AddAndActivateConnection2")]
    fn add_and_activate_connection2(
        &self,
        settings: HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &OwnedObjectPath,
        specific_object: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<(
        OwnedObjectPath,
        OwnedObjectPath,
        HashMap<String, OwnedValue>,
    )>;

    fn activate_connection(
        &self,
        connection: &OwnedObjectPath,
        device: &OwnedObjectPath,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;
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

    #[zbus(property)]
    fn managed(&self) -> zbus::Result<bool>;

    /// `(new_state, old_state, reason)`.
    ///
    /// **The** signal for this feature: the reason a join failed is here and
    /// nowhere else.
    #[zbus(signal)]
    fn state_changed(&self, new_state: u32, old_state: u32, reason: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(name = "AddConnection2")]
    fn add_connection2(
        &self,
        settings: HashMap<&str, HashMap<&str, Value<'_>>>,
        flags: u32,
        args: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<(OwnedObjectPath, HashMap<String, OwnedValue>)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SettingsConnection {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;

    #[zbus(name = "Update2")]
    fn update2(
        &self,
        settings: HashMap<&str, HashMap<&str, Value<'_>>>,
        flags: u32,
        args: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<HashMap<String, OwnedValue>>;

    fn delete(&self) -> zbus::Result<()>;
}

/// Writes wireless profiles, and remembers how the last join went.
pub struct WifiCustodian {
    conn: zbus::Connection,
    /// What [`lunchbox_state_proto::StateRequest::WifiJoinProgress`] answers.
    ///
    /// A mutex rather than a channel because there is exactly one interesting
    /// value — the newest — and a client that polls wants that, not a history
    /// it has to drain.
    join: Arc<Mutex<JoinSlot>>,
    authority: WifiAuthorityReply,
}

impl WifiCustodian {
    /// Connect to NetworkManager and ask polkit, once, whether writing and
    /// joining a profile will actually be permitted.
    ///
    /// The check is `AllowUserInteraction = 0`: this daemon has no session,
    /// seat or agent, so a check that could prompt would hang rather than
    /// answer. Same reasoning as [`crate::polkit`], whose [`Authority`] this
    /// deliberately mirrors rather than reuses — that one asks about a
    /// different action and produces a different remedy.
    ///
    /// [`Authority`]: crate::polkit::Authority
    pub async fn new(conn: zbus::Connection) -> Self {
        let authority = match check_authority(&conn).await {
            Ok(refused) if refused.is_empty() => WifiAuthorityReply {
                granted: true,
                reason: None,
            },
            Ok(refused) => WifiAuthorityReply {
                granted: false,
                reason: Some(format!(
                    "polkit refuses {} to this daemon's uid, so a Wi-Fi network cannot be \
                     saved and joined from the management UIs. Install the current \
                     {RULES_FILE} (issue #194)",
                    refused.join(" and ")
                )),
            },
            Err(e) => {
                // Not knowing is not the same as knowing it will fail, and the
                // same judgement `polkit::Authority::Unknown` makes: report it
                // as available and let the attempt produce the real error.
                warn!(error = %e, "Could not ask polkit about Wi-Fi configuration");
                WifiAuthorityReply {
                    granted: true,
                    reason: Some(format!(
                        "could not ask polkit whether Wi-Fi networks may be saved ({e}); the \
                         attempt will be made anyway"
                    )),
                }
            }
        };
        if authority.granted {
            debug!("The custodian may write Wi-Fi profiles");
        } else {
            warn!(
                reason = ?authority.reason,
                "The custodian may not write Wi-Fi profiles"
            );
        }
        Self {
            conn,
            join: Arc::new(Mutex::new(JoinSlot {
                generation: 0,
                state: WifiJoinState::Idle,
            })),
            authority,
        }
    }

    pub fn authority(&self) -> WifiAuthorityReply {
        self.authority.clone()
    }

    pub fn join_progress(&self) -> WifiJoinState {
        self.join.lock().expect("join state lock").state.clone()
    }

    /// Write a profile, and join it if asked.
    ///
    /// Returns once the profile exists and — for a join — once NetworkManager
    /// has accepted the activation. The outcome of the join arrives later,
    /// through [`Self::join_progress`].
    pub async fn save(&self, request: &WifiJoinRequest) -> Result<SavedWifiNetwork> {
        // Validated again here, not only in lunchboxd. This socket is a
        // privilege boundary: everything on the other side of it is a client,
        // and a client that has been compromised is exactly the one that
        // would send an unvalidated request.
        request
            .validate()
            .map_err(|e| anyhow::anyhow!("{}", e.message()))?;

        let (device, interface) = self
            .wireless_device()
            .await?
            .context("this device has no wireless adapter NetworkManager manages")?;

        let existing = self.find_by_ssid(&request.ssid).await?;

        match (existing, request.connect) {
            // Update in place, for later. The measured alternative is two
            // profiles with the same name and different UUIDs, which a list
            // cannot distinguish and which leaves the old, wrong password in
            // play. Not tried first, by the same rule as a new network saved
            // for later: the parent asked for it to be remembered, not joined.
            (Some((path, uuid)), false) => {
                let settings = profile_settings(request, &uuid);
                let profile = SettingsConnectionProxy::builder(&self.conn)
                    .path(path.clone())?
                    .build()
                    .await?;
                with_timeout(profile.update2(borrow(&settings), UPDATE2_TO_DISK, HashMap::new()))
                    .await
                    .context("updating the existing profile")?;
                info!(ssid = %request.ssid, "updated an existing Wi-Fi profile in place");
                Ok(saved_view(request, uuid, false))
            }

            // Remember it, without touching the network. What the web UI
            // leads with, because joining from a browser can cut the browser
            // off.
            (None, false) => {
                let uuid = new_uuid();
                let settings = profile_settings(request, &uuid);
                let settings_proxy = SettingsProxy::new(&self.conn).await?;
                with_timeout(settings_proxy.add_connection2(
                    borrow(&settings),
                    UPDATE2_TO_DISK,
                    HashMap::new(),
                ))
                .await
                .context("saving the profile")?;
                info!(ssid = %request.ssid, "saved a Wi-Fi profile for later");
                Ok(saved_view(request, uuid, false))
            }

            // Join. Volatile first, whether or not the network is already
            // saved: a profile written straight to disk with a wrong password
            // autoconnects forever, while NetworkManager discards a failed
            // volatile one itself.
            //
            // For a network that *is* saved, the new password is tried on a
            // volatile profile of its own and the saved one is left alone
            // until the join succeeds. Updating it in place first -- what
            // this did originally -- let a typo replace a password that
            // worked, on the very device that needed it.
            (existing, true) => {
                let uuid = new_uuid();
                let settings = profile_settings(request, &uuid);
                let nm = NetworkManagerProxy::new(&self.conn).await?;
                let mut options = HashMap::new();
                options.insert("persist", Value::from("volatile"));

                // Subscribe before activating: a join can settle in 3 seconds
                // and a watcher started afterwards misses it entirely.
                let stream = self.subscribe(&device).await?;

                let (conn_path, _active, _) = with_timeout(nm.add_and_activate_connection2(
                    borrow(&settings),
                    &device,
                    &null_path(),
                    options,
                ))
                .await
                .context("asking NetworkManager to join the network")?;

                // The watcher persists the profile once the join succeeds,
                // and only then retires the one it replaces. Nothing is
                // persisted or removed if the join fails, which is the point
                // of volatile.
                self.start_watching(
                    stream,
                    &request.ssid,
                    &interface,
                    Some(Persist {
                        path: conn_path.clone(),
                        replaces: existing.map(|(path, _)| path),
                    }),
                );

                // `uuid` is the trial profile's id. It becomes the saved
                // network's id if the join succeeds; if it fails, the saved
                // list still carries the old one, which is what a UI reads
                // next.
                info!(ssid = %request.ssid, %interface, "joining a Wi-Fi network");
                Ok(saved_view(request, uuid, true))
            }
        }
    }

    /// Join a network this device already has a profile for.
    pub async fn connect(&self, id: &str) -> Result<()> {
        let (device, interface) = self
            .wireless_device()
            .await?
            .context("this device has no wireless adapter NetworkManager manages")?;
        let (path, ssid) = self
            .find_by_uuid(id)
            .await?
            .context("no saved network with that id")?;

        let stream = self.subscribe(&device).await?;
        let nm = NetworkManagerProxy::new(&self.conn).await?;
        with_timeout(nm.activate_connection(&path, &device, &null_path()))
            .await
            .context("activating the saved profile")?;
        self.start_watching(stream, &ssid, &interface, None);
        info!(%ssid, %interface, "joining a saved Wi-Fi network");
        Ok(())
    }

    /// Delete a saved profile.
    ///
    /// On Ubuntu this has a side effect worth knowing about: NetworkManager's
    /// settings backend is netplan, and a delete makes netplan rewrite the
    /// whole of `/etc/netplan` — during the investigation it stripped a
    /// comment from `01-network-manager-all.yaml` and removed
    /// `00-installer-config.yaml` outright. Nothing here can prevent that;
    /// it is recorded so the next person to see a mangled `/etc/netplan` knows
    /// where to look. See the history note, and issue #194.
    pub async fn forget(&self, id: &str) -> Result<bool> {
        let Some((path, ssid)) = self.find_by_uuid(id).await? else {
            return Ok(false);
        };
        let profile = SettingsConnectionProxy::builder(&self.conn)
            .path(path)?
            .build()
            .await?;
        with_timeout(profile.delete())
            .await
            .context("deleting the profile")?;
        info!(%ssid, "forgot a Wi-Fi network");
        Ok(true)
    }

    /// Subscribe to the device's state changes, ahead of activating.
    ///
    /// The stream queues what arrives until [`Self::start_watching`] reads it,
    /// so nothing between the activation call and the watcher is lost.
    async fn subscribe(&self, device: &OwnedObjectPath) -> Result<StateChangedStream> {
        let proxy = DeviceProxy::builder(&self.conn)
            .path(device.clone())?
            .build()
            .await?;
        Ok(proxy.receive_state_changed().await?)
    }

    /// Mark a join NetworkManager has accepted as in progress, and follow it
    /// to its end on a task of its own.
    ///
    /// Called only *after* the activation call succeeded. Before it, there is
    /// no join: a refused call is an error its caller sees, and must not leave
    /// the state saying `connecting` with nothing behind it to ever change
    /// that.
    ///
    /// `persist` is the volatile profile to write to disk once the join
    /// succeeds, when there is one, and the saved profile it replaces.
    fn start_watching(
        &self,
        stream: StateChangedStream,
        ssid: &str,
        interface: &str,
        persist: Option<Persist>,
    ) {
        let generation = {
            let mut slot = self.join.lock().expect("join state lock");
            slot.generation += 1;
            slot.state = WifiJoinState::Connecting {
                ssid: ssid.to_string(),
            };
            slot.generation
        };

        let join = self.join.clone();
        let conn = self.conn.clone();
        let ssid = ssid.to_string();
        let interface = interface.to_string();
        tokio::spawn(async move {
            watch_join(
                conn,
                stream,
                Join {
                    slot: join,
                    generation,
                },
                ssid,
                interface,
                persist,
            )
            .await;
        });
    }

    /// The first managed wireless device, and its interface name.
    ///
    /// `Managed` as well as `DeviceType`, for the reason the reader states:
    /// an unmanaged radio is still type 2, and writing a profile bound to one
    /// produces a profile that never activates.
    async fn wireless_device(&self) -> Result<Option<(OwnedObjectPath, String)>> {
        let nm = NetworkManagerProxy::new(&self.conn).await?;
        for path in nm.devices().await? {
            let device = DeviceProxy::builder(&self.conn)
                .path(path.clone())?
                .build()
                .await?;
            if device.device_type().await? != NM_DEVICE_TYPE_WIFI {
                continue;
            }
            if !device.managed().await.unwrap_or(false) {
                continue;
            }
            let name = device.interface().await.unwrap_or_default();
            return Ok(Some((path, name)));
        }
        Ok(None)
    }

    /// The wireless profile for an SSID, if there is one, as
    /// `(object path, uuid)`.
    ///
    /// Matched on the SSID rather than `connection.id`, because id is a label
    /// that can be renamed and is not unique.
    async fn find_by_ssid(&self, ssid: &str) -> Result<Option<(OwnedObjectPath, String)>> {
        Ok(self
            .find(|config| (ssid_of(config)? == ssid).then_some(()))
            .await?
            .map(|(path, config)| (path, uuid_of(&config).unwrap_or_default())))
    }

    /// The wireless profile with this UUID, if there is one, as
    /// `(object path, ssid)`.
    ///
    /// The SSID, not the UUID back again: callers name the network in the join
    /// state and in the log, and a parent reads the first as "connecting to
    /// <name>". Returning the UUID here once put a UUID in both.
    async fn find_by_uuid(&self, uuid: &str) -> Result<Option<(OwnedObjectPath, String)>> {
        Ok(self
            .find(|config| (uuid_of(config)? == uuid).then_some(()))
            .await?
            .map(|(path, config)| (path, ssid_of(&config).unwrap_or_default())))
    }

    /// Walk the saved wireless profiles, returning the first `predicate`
    /// accepts, with its settings.
    async fn find<F>(&self, predicate: F) -> Result<Option<(OwnedObjectPath, Profile)>>
    where
        F: Fn(&Profile) -> Option<()>,
    {
        let settings = SettingsProxy::new(&self.conn).await?;
        for path in settings.list_connections().await? {
            let profile = SettingsConnectionProxy::builder(&self.conn)
                .path(path.clone())?
                .build()
                .await?;
            let Ok(config) = profile.get_settings().await else {
                continue;
            };
            let Some(connection) = config.get("connection") else {
                continue;
            };
            if connection.get("type").and_then(owned_str).as_deref() != Some(NM_SETTING_WIRELESS) {
                continue;
            }
            if predicate(&config).is_some() {
                return Ok(Some((path, config)));
            }
        }
        Ok(None)
    }
}

/// A UUID for a profile this daemon is about to create.
///
/// Chosen here rather than left to NetworkManager and read back, because
/// reading it back is a `GetSettings` on a profile NetworkManager is in the
/// middle of activating -- measured holding its reply for exactly six seconds
/// while the device was busy, past lunchboxd's five-second call timeout. The
/// join went ahead and the parent was told it had failed.
fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// What a successful join writes down.
struct Persist {
    /// The volatile profile the join ran on.
    path: OwnedObjectPath,
    /// The saved profile for the same network, retired once `path` is on
    /// disk. `None` for a network that was not saved before.
    replaces: Option<OwnedObjectPath>,
}

/// The newest join's state, and which join that is.
struct JoinSlot {
    /// Bumped by every join NetworkManager accepts.
    generation: u64,
    state: WifiJoinState,
}

/// One watcher's handle on the slot: allowed to write only while its join is
/// still the newest.
struct Join {
    slot: Arc<Mutex<JoinSlot>>,
    generation: u64,
}

impl Join {
    fn is_current(&self) -> bool {
        self.slot.lock().expect("join state lock").generation == self.generation
    }

    /// Record how the join ended, unless a newer one has started since.
    ///
    /// Checked under the same lock as the write, so a join that starts while
    /// this one is finishing cannot have its `connecting` overwritten.
    fn settle(&self, state: WifiJoinState) -> bool {
        let mut slot = self.slot.lock().expect("join state lock");
        if slot.generation != self.generation {
            return false;
        }
        slot.state = state;
        true
    }
}

/// Follow one join to its end, then record what happened.
///
/// Reads the **device's** state changes. The active connection's are useless
/// for this: measured, every failure reports `DEACTIVATED /
/// DEVICE_DISCONNECTED` there, whatever the cause.
///
/// A watcher whose join has been superseded stops at the next signal without
/// persisting or reporting anything: the signals are the *device's*, so what
/// it would be reading from then on is the newer join's outcome.
async fn watch_join(
    conn: zbus::Connection,
    mut stream: StateChangedStream,
    join: Join,
    ssid: String,
    interface: String,
    persist: Option<Persist>,
) {
    use futures_util::StreamExt;

    let outcome = tokio::time::timeout(JOIN_TIMEOUT, async {
        while let Some(signal) = stream.next().await {
            if !join.is_current() {
                return None;
            }
            let Ok(args) = signal.args() else { continue };
            let (new_state, reason) = (args.new_state, args.reason);
            debug!(
                %ssid, %interface, new_state, reason,
                "a wireless device changed state during a join"
            );
            match new_state {
                NM_DEVICE_STATE_ACTIVATED => return Some(Ok(())),
                NM_DEVICE_STATE_FAILED => return Some(Err(reason)),
                _ => continue,
            }
        }
        None
    })
    .await;

    let state = match outcome {
        Ok(Some(Ok(()))) => {
            // Success: turn the volatile profile into a saved one. Only now —
            // a profile persisted before this point would autoconnect forever
            // with a password that does not work.
            if let Some(persist) = persist.filter(|_| join.is_current()) {
                match persist_profile(&conn, &persist.path).await {
                    Ok(()) => {
                        info!(%ssid, "saved the profile for the network we just joined");
                        // Only now, with the new one on disk: removed any
                        // earlier, a failed save would leave no profile at all.
                        if let Some(old) = persist.replaces {
                            match delete_profile(&conn, &old).await {
                                Ok(()) => info!(%ssid, "retired the profile it replaces"),
                                Err(e) => warn!(
                                    %ssid, error = %e,
                                    "joined and saved, but the old profile for this network \
                                     is still there too"
                                ),
                            }
                        }
                    }
                    // The device *is* on the network; only remembering it
                    // failed. Reported as connected, because it is, with the
                    // failure in the log rather than in the parent's face. The
                    // old profile, if any, stays: it is all that is saved.
                    Err(e) => warn!(%ssid, error = %e, "joined, but could not save the profile"),
                }
            }
            WifiJoinState::Connected { ssid }
        }
        Ok(Some(Err(reason))) => WifiJoinState::Failed {
            ssid,
            reason: failure_from_reason(reason),
        },
        // The signal stream ended without a verdict. Not a failure we can
        // name, and not a success.
        Ok(None) => WifiJoinState::Failed {
            ssid,
            reason: WifiJoinFailure::detailed(
                WifiJoinFailureKind::Other,
                "NetworkManager stopped reporting on this device",
            ),
        },
        Err(_) => WifiJoinState::Failed {
            ssid,
            reason: WifiJoinFailure::detailed(
                WifiJoinFailureKind::Other,
                format!("gave up after {} seconds", JOIN_TIMEOUT.as_secs()),
            ),
        },
    };
    if join.settle(state.clone()) {
        debug!(?state, "a join settled");
    } else {
        debug!(
            ?state,
            "a join settled after a newer one started; not reported"
        );
    }
}

/// `Update2(…, TO_DISK)` on a profile NetworkManager currently holds in
/// memory.
///
/// An empty settings dictionary means "keep what you have, just write it
/// down", which is what was measured to work: the profile keeps the password
/// the successful association proved correct.
async fn persist_profile(conn: &zbus::Connection, path: &OwnedObjectPath) -> Result<()> {
    let profile = SettingsConnectionProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await?;
    with_timeout(profile.update2(HashMap::new(), UPDATE2_TO_DISK, HashMap::new())).await?;
    Ok(())
}

/// Delete a saved profile. Used to retire the one a successful join replaced.
async fn delete_profile(conn: &zbus::Connection, path: &OwnedObjectPath) -> Result<()> {
    let profile = SettingsConnectionProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await?;
    with_timeout(profile.delete()).await
}

/// A device state reason, as the thing a parent should do about it.
///
/// The three named reasons are the measured outcomes on 1.54.3. Anything else
/// travels as its number, so a log is useful for a case nobody anticipated.
fn failure_from_reason(reason: u32) -> WifiJoinFailure {
    match reason {
        REASON_NO_SECRETS => WifiJoinFailure::of(WifiJoinFailureKind::WrongPassword),
        REASON_SSID_NOT_FOUND => WifiJoinFailure::of(WifiJoinFailureKind::NotFound),
        REASON_IP_CONFIG_UNAVAILABLE => WifiJoinFailure::of(WifiJoinFailureKind::NoAddress),
        other => WifiJoinFailure::detailed(
            WifiJoinFailureKind::Other,
            format!("NetworkManager reported reason {other}"),
        ),
    }
}

/// The reply for a save, built from the request rather than read back.
///
/// Reading it back would cost a round trip to say what we just said, and on
/// Ubuntu would say something slightly different: netplan normalises a profile
/// on the way to disk, adding an `interface-name` nobody asked for and
/// dropping `autoconnect` because it defaults it.
fn saved_view(request: &WifiJoinRequest, uuid: String, active: bool) -> SavedWifiNetwork {
    SavedWifiNetwork {
        id: uuid,
        ssid: request.ssid.clone(),
        security: request.security,
        hidden: request.hidden,
        autoconnect: true,
        active,
    }
}

/// The NetworkManager settings dictionary for a profile.
///
/// Matches the shape GNOME writes on this device: a *system* profile, with
/// `permissions` empty so it is not tied to one logged-in user, and
/// `psk-flags = 0` so NetworkManager stores the secret itself rather than
/// asking an agent that does not exist in the kiosk session.
///
/// `uuid` is the profile's own: the existing one when updating in place,
/// where carrying it is what makes `Update2` an update rather than a
/// rejection, and a [`new_uuid`] otherwise.
fn profile_settings(
    request: &WifiJoinRequest,
    uuid: &str,
) -> HashMap<String, HashMap<String, OwnedValue>> {
    let mut connection: HashMap<String, OwnedValue> = HashMap::new();
    connection.insert("id".into(), own(Value::from(request.ssid.clone())));
    connection.insert("type".into(), own(Value::from(NM_SETTING_WIRELESS)));
    connection.insert("autoconnect".into(), own(Value::from(true)));
    // An empty permissions list is what makes this a system profile, usable
    // at the greeter and by every user, rather than one that only works while
    // a particular account is logged in.
    connection.insert("permissions".into(), own(Value::from(Vec::<String>::new())));
    connection.insert("uuid".into(), own(Value::from(uuid.to_string())));

    let mut wireless: HashMap<String, OwnedValue> = HashMap::new();
    wireless.insert(
        "ssid".into(),
        own(Value::from(request.ssid.as_bytes().to_vec())),
    );
    wireless.insert("mode".into(), own(Value::from("infrastructure")));
    if request.hidden {
        wireless.insert("hidden".into(), own(Value::from(true)));
    }

    let mut settings = HashMap::new();
    settings.insert("connection".to_string(), connection);
    settings.insert(NM_SETTING_WIRELESS.to_string(), wireless);

    // DHCP both ways. Manual addressing is explicitly out of scope for #194.
    for family in ["ipv4", "ipv6"] {
        let mut ip: HashMap<String, OwnedValue> = HashMap::new();
        ip.insert("method".into(), own(Value::from("auto")));
        settings.insert(family.to_string(), ip);
    }

    if let Some(key_mgmt) = request.security.key_mgmt() {
        let mut security: HashMap<String, OwnedValue> = HashMap::new();
        security.insert("key-mgmt".into(), own(Value::from(key_mgmt)));
        if let Some(password) = &request.password {
            security.insert("psk".into(), own(Value::from(password.clone())));
            // 0 is NM_SETTING_SECRET_FLAG_NONE: NetworkManager keeps the
            // secret. Any other flag means "ask an agent", and the kiosk
            // session has none — the association would fail with a reason that
            // reads exactly like a wrong password.
            security.insert("psk-flags".into(), own(Value::from(0u32)));
        }
        settings.insert("802-11-wireless-security".to_string(), security);
    }

    settings
}

/// A profile's settings, as `GetSettings` returns them.
type Profile = HashMap<String, HashMap<String, OwnedValue>>;

/// A profile's UUID.
fn uuid_of(config: &Profile) -> Option<String> {
    owned_str(config.get("connection")?.get("uuid")?)
}

/// A wireless profile's SSID, when it is text.
fn ssid_of(config: &Profile) -> Option<String> {
    let bytes = config
        .get(NM_SETTING_WIRELESS)?
        .get("ssid")
        .and_then(owned_bytes)?;
    String::from_utf8(bytes).ok()
}

/// Borrow an owned settings dictionary as the `a{sa{sv}}` the proxy wants.
fn borrow(
    settings: &HashMap<String, HashMap<String, OwnedValue>>,
) -> HashMap<&str, HashMap<&str, Value<'_>>> {
    settings
        .iter()
        .map(|(section, entries)| {
            (
                section.as_str(),
                entries
                    .iter()
                    .map(|(k, v)| (k.as_str(), (**v).clone()))
                    .collect(),
            )
        })
        .collect()
}

fn own(value: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(value).expect("these values are all ownable")
}

fn owned_str(value: &OwnedValue) -> Option<String> {
    <&str>::try_from(value).map(|s| s.to_string()).ok()
}

fn owned_bytes(value: &OwnedValue) -> Option<Vec<u8>> {
    <Vec<u8>>::try_from(value.try_clone().ok()?).ok()
}

async fn with_timeout<T>(future: impl std::future::Future<Output = zbus::Result<T>>) -> Result<T> {
    match tokio::time::timeout(CALL_TIMEOUT, future).await {
        Ok(result) => Ok(result?),
        Err(_) => anyhow::bail!("NetworkManager did not answer within {CALL_TIMEOUT:?}"),
    }
}

/// Whether an error is NetworkManager refusing on polkit's behalf.
///
/// Told apart by the D-Bus error *name*, which NetworkManager keeps stable
/// across its interfaces (`org.freedesktop.NetworkManager.PermissionDenied`,
/// `….Settings.PermissionDenied`, `….Settings.Connection.PermissionDenied`),
/// rather than by the sentence, which it does not. Walks the whole chain
/// because every call here is wrapped in a `context`.
pub fn is_polkit_refusal(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| match cause.downcast_ref::<zbus::Error>() {
            Some(zbus::Error::MethodError(name, _, _)) => is_refusal_name(name.as_str()),
            Some(zbus::Error::FDO(fdo)) => matches!(**fdo, zbus::fdo::Error::AccessDenied(_)),
            _ => false,
        })
}

fn is_refusal_name(name: &str) -> bool {
    name.starts_with("org.freedesktop.NetworkManager") && name.ends_with(".PermissionDenied")
}

/// Ask polkit whether this daemon may write and join a system connection
/// profile, returning the actions it refuses.
///
/// Both, not only the write: a rule that grants one and not the other offers
/// a parent a form that saves the network and then cannot join it.
async fn check_authority(conn: &zbus::Connection) -> Result<Vec<&'static str>> {
    let mut refused = Vec::new();
    for action in ACTIONS {
        if !crate::polkit::check_action(conn, action).await? {
            refused.push(action);
        }
    }
    Ok(refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_api::WifiSecurity;

    fn request(security: WifiSecurity, password: Option<&str>) -> WifiJoinRequest {
        WifiJoinRequest {
            ssid: "home".into(),
            security,
            password: password.map(Into::into),
            hidden: false,
            connect: true,
        }
    }

    #[test]
    fn a_psk_profile_matches_the_shape_gnome_writes() {
        let settings = profile_settings(&request(WifiSecurity::WpaPsk, Some("12345678")), "u-1");

        let connection = &settings["connection"];
        assert_eq!(owned_str(&connection["type"]).unwrap(), "802-11-wireless");
        assert_eq!(owned_str(&connection["id"]).unwrap(), "home");
        assert!(bool::try_from(&connection["autoconnect"]).unwrap());
        assert!(
            connection.contains_key("permissions"),
            "an empty permissions list is what makes this a system profile"
        );
        assert_eq!(
            owned_str(&connection["uuid"]).unwrap(),
            "u-1",
            "the profile carries the uuid it was given, so nothing has to read it back"
        );

        let wireless = &settings["802-11-wireless"];
        assert_eq!(owned_bytes(&wireless["ssid"]).unwrap(), b"home");
        assert_eq!(owned_str(&wireless["mode"]).unwrap(), "infrastructure");
        assert!(
            !wireless.contains_key("hidden"),
            "a network picked from a scan was broadcasting by definition"
        );

        let security = &settings["802-11-wireless-security"];
        assert_eq!(owned_str(&security["key-mgmt"]).unwrap(), "wpa-psk");
        assert_eq!(owned_str(&security["psk"]).unwrap(), "12345678");
        assert_eq!(
            u32::try_from(&security["psk-flags"]).unwrap(),
            0,
            "anything but 0 asks an agent the kiosk session does not have"
        );

        assert_eq!(owned_str(&settings["ipv4"]["method"]).unwrap(), "auto");
        assert_eq!(owned_str(&settings["ipv6"]["method"]).unwrap(), "auto");
    }

    #[test]
    fn an_update_in_place_carries_the_existing_uuid() {
        // Without it Update2 is a rejection rather than an update, and the
        // caller falls back to creating a duplicate.
        let settings = profile_settings(
            &request(WifiSecurity::WpaPsk, Some("12345678")),
            "6c16874f-3e20-47c6-a114-4436691a54a7",
        );
        assert_eq!(
            owned_str(&settings["connection"]["uuid"]).unwrap(),
            "6c16874f-3e20-47c6-a114-4436691a54a7"
        );
    }

    #[test]
    fn an_open_network_gets_no_security_section() {
        let settings = profile_settings(&request(WifiSecurity::Open, None), "u-1");
        assert!(
            !settings.contains_key("802-11-wireless-security"),
            "an open network has no key management to write"
        );
    }

    #[test]
    fn enhanced_open_is_encrypted_with_no_password() {
        let settings = profile_settings(&request(WifiSecurity::Owe, None), "u-1");
        let security = &settings["802-11-wireless-security"];
        assert_eq!(owned_str(&security["key-mgmt"]).unwrap(), "owe");
        assert!(
            !security.contains_key("psk"),
            "OWE has nothing to type and nothing to store"
        );
    }

    #[test]
    fn wpa3_writes_sae() {
        let settings = profile_settings(&request(WifiSecurity::Sae, Some("a-secret")), "u-1");
        assert_eq!(
            owned_str(&settings["802-11-wireless-security"]["key-mgmt"]).unwrap(),
            "sae"
        );
    }

    #[test]
    fn a_hidden_network_says_so_or_it_is_never_found() {
        let mut req = request(WifiSecurity::WpaPsk, Some("12345678"));
        req.hidden = true;
        let settings = profile_settings(&req, "u-1");
        assert!(bool::try_from(&settings["802-11-wireless"]["hidden"]).unwrap());
    }

    /// The table that only hardware could establish. Each number was read off
    /// a real failure and cross-checked against the reason string
    /// NetworkManager logged; see the history note.
    #[test]
    fn measured_device_reasons_become_something_a_parent_can_act_on() {
        assert_eq!(
            failure_from_reason(7).kind,
            WifiJoinFailureKind::WrongPassword,
            "reason 7 no-secrets"
        );
        assert_eq!(
            failure_from_reason(53).kind,
            WifiJoinFailureKind::NotFound,
            "reason 53 ssid-not-found"
        );
        assert_eq!(
            failure_from_reason(5).kind,
            WifiJoinFailureKind::NoAddress,
            "reason 5 ip-config-unavailable"
        );
    }

    #[test]
    fn a_supplicant_disconnect_is_not_reported_as_a_wrong_password() {
        // Reason 8 appears on the wrong-password path as an intermediate
        // config -> need-auth hop, never as the terminal reason. Treating it
        // as one would report a failure while the supplicant was still
        // retrying -- and 1.54 took up to 25 seconds to give up.
        let failure = failure_from_reason(8);
        assert_eq!(failure.kind, WifiJoinFailureKind::Other);
        assert!(failure.detail.is_some());
    }

    #[test]
    fn an_unrecognised_reason_keeps_its_number() {
        let failure = failure_from_reason(4242);
        assert_eq!(failure.kind, WifiJoinFailureKind::Other);
        assert!(
            failure.detail.as_deref().unwrap().contains("4242"),
            "an unanticipated reason has to be greppable: {failure:?}"
        );
    }

    fn slot() -> Arc<Mutex<JoinSlot>> {
        Arc::new(Mutex::new(JoinSlot {
            generation: 0,
            state: WifiJoinState::Idle,
        }))
    }

    /// What `start_watching` does to the slot, without a bus.
    fn start(slot: &Arc<Mutex<JoinSlot>>, ssid: &str) -> Join {
        let mut guard = slot.lock().unwrap();
        guard.generation += 1;
        guard.state = WifiJoinState::Connecting { ssid: ssid.into() };
        Join {
            slot: slot.clone(),
            generation: guard.generation,
        }
    }

    fn failed(ssid: &str) -> WifiJoinState {
        WifiJoinState::Failed {
            ssid: ssid.into(),
            reason: WifiJoinFailure::detailed(WifiJoinFailureKind::Other, "gave up"),
        }
    }

    #[test]
    fn a_superseded_join_cannot_overwrite_the_newer_one() {
        // Measured on a device: a watcher left over from a join whose
        // activation was refused timed out 90 s later and reported "gave up"
        // over a join that had started two seconds before and went on to
        // succeed.
        let slot = slot();
        let old = start(&slot, "first");
        let new = start(&slot, "second");

        assert!(!old.is_current());
        assert!(
            !old.settle(failed("first")),
            "a superseded join must not report"
        );
        assert_eq!(
            slot.lock().unwrap().state,
            WifiJoinState::Connecting {
                ssid: "second".into()
            }
        );

        assert!(new.is_current());
        assert!(new.settle(WifiJoinState::Connected {
            ssid: "second".into()
        }));
        assert_eq!(
            slot.lock().unwrap().state,
            WifiJoinState::Connected {
                ssid: "second".into()
            }
        );
    }

    #[test]
    fn a_settled_join_still_reports_when_nothing_newer_started() {
        let slot = slot();
        let join = start(&slot, "home");
        assert!(join.settle(failed("home")));
        assert_eq!(slot.lock().unwrap().state, failed("home"));
    }

    #[test]
    fn every_networkmanager_permission_error_is_a_refusal() {
        // Measured on 1.54.3: a join without network-control failed with the
        // first of these, "Not authorized to control networking". The other
        // two are what the settings interfaces raise for the same reason.
        for name in [
            "org.freedesktop.NetworkManager.PermissionDenied",
            "org.freedesktop.NetworkManager.Settings.PermissionDenied",
            "org.freedesktop.NetworkManager.Settings.Connection.PermissionDenied",
        ] {
            assert!(is_refusal_name(name), "{name}");
        }
        for name in [
            "org.freedesktop.NetworkManager.UnknownConnection",
            "org.freedesktop.NetworkManager.Device.NotAllowed",
            "org.example.Other.PermissionDenied",
        ] {
            assert!(!is_refusal_name(name), "{name}");
        }
    }

    #[test]
    fn an_error_that_is_not_from_the_bus_is_not_a_refusal() {
        let error = anyhow::anyhow!("PermissionDenied, in words only")
            .context("asking NetworkManager to join the network");
        assert!(
            !is_polkit_refusal(&error),
            "a refusal is recognised by its error name, never by a sentence"
        );
    }

    #[test]
    fn a_profile_is_named_by_its_ssid_and_identified_by_its_uuid() {
        // find_by_uuid once handed its caller the UUID where the SSID was
        // expected, and the join state then read "connecting to <uuid>".
        let mut settings =
            profile_settings(&request(WifiSecurity::WpaPsk, Some("12345678")), "u-1");
        assert_eq!(ssid_of(&settings).as_deref(), Some("home"));
        assert_eq!(uuid_of(&settings).as_deref(), Some("u-1"));

        settings.remove("connection");
        assert_eq!(uuid_of(&settings), None);
    }

    #[test]
    fn a_saved_view_reports_what_was_asked_for() {
        let req = request(WifiSecurity::Sae, Some("a-secret"));
        let view = saved_view(&req, "uuid-1".into(), true);
        assert_eq!(view.id, "uuid-1");
        assert_eq!(view.ssid, "home");
        assert_eq!(view.security, WifiSecurity::Sae);
        assert!(view.active);
        // And carries no secret, which is the type's whole promise.
        assert!(!format!("{view:?}").contains("a-secret"));
    }

    #[test]
    fn the_profile_dictionary_never_mentions_a_password_for_a_keyless_network() {
        for security in [WifiSecurity::Open, WifiSecurity::Owe] {
            let settings = profile_settings(&request(security, None), "u-1");
            let rendered = format!("{settings:?}");
            assert!(
                !rendered.contains("psk"),
                "{security:?} wrote a psk field: {rendered}"
            );
        }
    }
}
