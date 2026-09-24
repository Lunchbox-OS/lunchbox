//! Joining the wireless reader to the wireless writer (issue #194).
//!
//! The two halves of the feature live in different processes, for a reason
//! that is the whole point of the design: reading NetworkManager's properties
//! needs no privilege, while writing a profile needs
//! `settings.modify.system`, and this daemon's uid must not hold that — every
//! activity runs as the same user, and the grant was measured to be enough on
//! its own to read back every saved network's password.
//!
//! So [`CustodialWifi`] reads locally through
//! [`lunchbox_host_linux::LinuxWifiReader`] and forwards the three writes to
//! the state custodian. Joining a saved network goes there too when the
//! custodian may do it, because the custodian is the one that watches a join
//! and reports how it went; when it may not, `lunchboxd` activates the profile
//! itself. Nothing above it knows: `lunchbox-management` sees one
//! [`WifiController`], which is what keeps the split out of every call site.
//!
//! On a device with no custodian — a dev session started with
//! `--no-state-custodian`, or one whose custodian could not be reached — the
//! reads still work and the writes report
//! [`WifiError::NotAuthorized`]. Never a pretence that it worked.

use std::sync::Arc;

use async_trait::async_trait;
use lunchbox_api::{SavedWifiNetwork, WifiJoinRequest, WifiJoinState};
use lunchbox_host_api::{WifiController, WifiError, WifiResult, WifiSnapshot};
use lunchbox_state_proto::{StateRequest, Transport, WifiAuthorityReply};
use tracing::{debug, warn};

/// Reads from NetworkManager directly; writes through the custodian.
pub struct CustodialWifi {
    reader: Arc<dyn WifiController>,
    /// The custodian's request socket, or `None` on a device without one.
    ///
    /// Used for `save`, `forget` and, when the custodian holds its grant,
    /// `connect` -- see [`WifiController::connect`] below.
    ///
    /// Its own connection, not the one the store uses: a wireless call and a
    /// usage write would otherwise serialise behind each other.
    transport: Option<Transport>,
    /// What the custodian said at startup about whether it may write at all.
    authority: WifiAuthorityReply,
}

impl CustodialWifi {
    /// Connect to the custodian serving `user`, and ask once whether writes
    /// will be permitted.
    ///
    /// A device with no custodian is not an error: it is a dev session, or a
    /// fresh install, and it still answers every read.
    pub fn new(reader: Arc<dyn WifiController>, user: Option<&str>) -> Self {
        let Some(user) = user else {
            debug!("No state custodian, so Wi-Fi networks cannot be saved from here");
            return Self {
                reader,
                transport: None,
                authority: WifiAuthorityReply {
                    granted: false,
                    reason: Some(
                        "this daemon is running without a state custodian, which is the only \
                         thing allowed to write a Wi-Fi profile"
                            .into(),
                    ),
                },
            };
        };

        match Transport::connect_for_user(user) {
            Ok(transport) => {
                // Ask now rather than on the first save. The answer feeds a
                // Health diagnostic and the `can_configure` flag both UIs use
                // to decide whether to offer the form at all, and both want it
                // before a parent has typed anything.
                let authority = match transport
                    .call::<WifiAuthorityReply>(&StateRequest::WifiAuthority)
                {
                    Ok(reply) => reply,
                    Err(e) => {
                        // An older custodian does not know the request. Report
                        // it as unavailable rather than guessing: a half-
                        // finished upgrade is exactly the case the protocol
                        // handshake exists to make loud.
                        let e = lunchbox_store::StoreError::from(e);
                        warn!(error = %e, "The custodian did not answer whether Wi-Fi may be configured");
                        WifiAuthorityReply {
                            granted: false,
                            reason: Some(format!(
                                "the state custodian did not answer whether Wi-Fi networks may \
                                 be saved ({e}); it may be an older version than this daemon"
                            )),
                        }
                    }
                };
                Self {
                    reader,
                    transport: Some(transport),
                    authority,
                }
            }
            Err(e) => {
                warn!(error = %e, "Could not reach the state custodian for Wi-Fi configuration");
                Self {
                    reader,
                    transport: None,
                    authority: WifiAuthorityReply {
                        granted: false,
                        reason: Some(format!(
                            "the state custodian could not be reached ({e}), so Wi-Fi networks \
                             cannot be saved"
                        )),
                    },
                }
            }
        }
    }

    /// Why writes are unavailable, when they are. `None` when they work.
    ///
    /// Feeds the `wifi_config_unavailable` diagnostic. Separate from
    /// [`WifiController::can_configure`] because a diagnostic needs the
    /// sentence and a UI needs the boolean.
    pub fn unavailable_reason(&self) -> Option<&str> {
        if self.authority.granted {
            return None;
        }
        self.authority.reason.as_deref()
    }

    /// Whether this device even has a radio, so a diagnostic about a missing
    /// grant is not raised on a machine that has nothing to configure.
    pub async fn has_adapter(&self) -> bool {
        self.reader.networks().await.supported
    }

    fn custodian(&self) -> WifiResult<&Transport> {
        self.transport.as_ref().ok_or(WifiError::NotAuthorized)
    }
}

#[async_trait]
impl WifiController for CustodialWifi {
    // --- reads: straight to NetworkManager, no privilege needed ---

    async fn scan(&self) -> WifiResult<()> {
        self.reader.scan().await
    }

    async fn networks(&self) -> WifiSnapshot {
        self.reader.networks().await
    }

    async fn saved(&self) -> WifiResult<Vec<SavedWifiNetwork>> {
        self.reader.saved().await
    }

    // --- writes: through the custodian ---

    async fn save(&self, request: &WifiJoinRequest) -> WifiResult<SavedWifiNetwork> {
        let transport = self.custodian()?;
        // A blocking socket call on the runtime. It is bounded by the
        // custodian's own 10-second NetworkManager timeout and returns as soon
        // as the activation is *accepted*, not when the network comes up --
        // which is the design that makes this safe to do inline.
        transport
            .call::<SavedWifiNetwork>(&StateRequest::WifiSave {
                request: request.clone(),
            })
            .map_err(as_wifi_error)
    }

    async fn connect(&self, id: &str) -> WifiResult<()> {
        // Through the custodian when it may, because the custodian is what
        // watches a join and answers `join_state`. Activated here instead, a
        // join reported nothing: the phone's Connect button never showed
        // "connecting", and whatever the last custodian join had said -- a
        // failure, minutes old -- stayed on screen over the network it had
        // just joined.
        if self.authority.granted
            && let Some(transport) = &self.transport
        {
            // A stale id is told apart here, where the saved list is
            // readable, rather than by the custodian's error sentence.
            if !self.reader.saved().await?.iter().any(|n| n.id == id) {
                return Err(WifiError::UnknownNetwork);
            }
            match transport.call::<()>(&StateRequest::WifiConnect { id: id.to_string() }) {
                Ok(()) => return Ok(()),
                // The custodian was refused after all -- its rule changed
                // since startup. Fall through to the path that needs no rule.
                Err(e) => match as_wifi_error(e) {
                    WifiError::NotAuthorized => {}
                    other => return Err(other),
                },
            }
        }

        // Locally, when the custodian cannot. Activating a profile that
        // already exists is gated on `network-control`, which polkit grants to
        // an active local session outright -- so this must keep working on a
        // device whose rules file is missing, which is what
        // `wifi_config_unavailable` tells the parent it can still do. It
        // reports no progress, which is the cost of the missing rule.
        self.reader.connect(id).await
    }

    async fn forget(&self, id: &str) -> WifiResult<bool> {
        let transport = self.custodian()?;
        transport
            .call::<bool>(&StateRequest::WifiForget { id: id.to_string() })
            .map_err(as_wifi_error)
    }

    async fn join_state(&self) -> WifiJoinState {
        let Ok(transport) = self.custodian() else {
            return WifiJoinState::Idle;
        };
        match transport.call::<WifiJoinState>(&StateRequest::WifiJoinProgress) {
            Ok(state) => state,
            Err(e) => {
                // A poll that cannot reach the custodian must not invent a
                // failure: the join may well be succeeding. Idle is the honest
                // "nothing to report from here".
                let e = lunchbox_store::StoreError::from(e);
                debug!(error = %e, "Could not read the Wi-Fi join state");
                WifiJoinState::Idle
            }
        }
    }

    async fn can_configure(&self) -> bool {
        self.authority.granted
    }
}

/// A custodian call failure, as something a transport can report.
///
/// A polkit refusal becomes [`WifiError::NotAuthorized`], which the management
/// API reports as 403: it is the one failure on this path a person can act on,
/// and the custodian sends it as its own kind so it can be told apart without
/// reading a sentence. Everything else is `Backend`: by the time a call has
/// reached the custodian the other distinctions that matter — no adapter, a
/// stale id — have already been made on one side or the other.
fn as_wifi_error(error: lunchbox_state_proto::CallError) -> WifiError {
    // Through StoreError, which is the one of the two that implements Display
    // -- and which already words a transport failure as "the state custodian:
    // ...", naming the thing that was unreachable.
    let error = lunchbox_store::StoreError::from(error);
    if let lunchbox_store::StoreError::Io(e) = &error
        && e.kind() == std::io::ErrorKind::PermissionDenied
    {
        warn!(error = %e, "polkit refused a Wi-Fi change the custodian made on our behalf");
        return WifiError::NotAuthorized;
    }
    WifiError::Backend(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_host_api::NullWifi;

    #[tokio::test]
    async fn a_device_with_no_custodian_reads_but_does_not_write() {
        let wifi = CustodialWifi::new(Arc::new(NullWifi), None);

        assert!(!wifi.can_configure().await);
        assert!(
            wifi.unavailable_reason()
                .expect("a reason to show in Health")
                .contains("custodian"),
            "the remedy has to name what is missing"
        );

        let request = WifiJoinRequest {
            ssid: "home".into(),
            security: lunchbox_api::WifiSecurity::WpaPsk,
            password: Some("12345678".into()),
            hidden: false,
            connect: true,
        };
        assert_eq!(
            wifi.save(&request).await.unwrap_err(),
            WifiError::NotAuthorized,
            "never a pretence that it worked"
        );
        assert_eq!(
            wifi.forget("any").await.unwrap_err(),
            WifiError::NotAuthorized
        );
    }

    #[tokio::test]
    async fn joining_a_known_network_does_not_need_the_custodian() {
        // The deliberate exception to "everything that changes the device goes
        // through the custodian". Activating an existing profile is gated on
        // network-control, which the kiosk user already holds, so a device
        // whose polkit rules file is missing must still be able to get back
        // onto a network it knows -- which is exactly what the
        // wifi_config_unavailable diagnostic promises it can do.
        let wifi = CustodialWifi::new(Arc::new(NullWifi), None);

        let error = wifi.connect("some-id").await.unwrap_err();
        assert_ne!(
            error,
            WifiError::NotAuthorized,
            "connect must not be refused for want of a custodian"
        );
        // NullWifi has no adapter, so that is what it says -- and the point is
        // that it got as far as asking the radio.
        assert_eq!(error, WifiError::NoAdapter);
    }

    #[test]
    fn a_refusal_from_the_custodian_is_not_an_internal_error() {
        // The custodian sends polkit's refusal as WireErrorKind::Refused; the
        // client rebuilds that as a PermissionDenied Io error, and it has to
        // come out the far side as the error the API reports as 403.
        let refused = lunchbox_state_proto::WireErrorKind::Refused
            .into_error("Not authorized to control networking".into());
        assert_eq!(
            as_wifi_error(lunchbox_state_proto::CallError::Remote(refused)),
            WifiError::NotAuthorized
        );

        let other = lunchbox_state_proto::WireErrorKind::Io
            .into_error("NetworkManager did not answer".into());
        assert!(matches!(
            as_wifi_error(lunchbox_state_proto::CallError::Remote(other)),
            WifiError::Backend(_)
        ));
    }

    #[tokio::test]
    async fn a_join_poll_with_no_custodian_is_idle_not_failed() {
        // A poll that cannot reach the custodian must not invent a failure:
        // the join may be succeeding, and a UI told "failed" would have a
        // parent retyping a password that was right.
        let wifi = CustodialWifi::new(Arc::new(NullWifi), None);
        assert_eq!(wifi.join_state().await, WifiJoinState::Idle);
    }

    #[tokio::test]
    async fn reads_are_not_gated_on_the_write_grant() {
        // The read half is the whole of slice one and needs no privilege. A
        // device with no custodian still shows what is in range.
        let wifi = CustodialWifi::new(Arc::new(NullWifi), None);
        let snapshot = wifi.networks().await;
        // NullWifi reports no adapter, which is the honest answer here; the
        // point is that it answered rather than refusing on authority.
        assert!(!snapshot.supported);
        assert!(!wifi.has_adapter().await);
    }
}
