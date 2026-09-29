//! Removing a saved profile without letting netplan rewrite `/etc/netplan`
//! (issue #194).
//!
//! On Ubuntu, NetworkManager's `Delete` makes netplan parse every file in
//! `/etc/netplan` and write each one back out from what it parsed. Every
//! comment goes, and a file a later one overrides is unlinked: a forget once
//! removed the installer's `00-installer-config.yaml`. So a netplan-backed
//! profile is removed by `lunchbox-wifi-forget@<uuid>.service` instead, a root
//! oneshot that cuts out the one definition and leaves every other byte alone.
//! See `crates/lunchbox-wifi-forget`.
//!
//! A profile that is not in netplan — a keyfile on another distribution, or one
//! that only ever lived in memory — is deleted the ordinary way, because
//! there the ordinary way touches nothing else.
//!
//! There is no startup check for the unit grant, unlike the two NetworkManager
//! actions. The rule tests which unit and which verb, and polkit refuses to let
//! anyone but root pass those details to `CheckAuthorization` ("Only trusted
//! callers … can use CheckAuthorization() and pass details", measured on
//! polkit 127). Asked without them, the rule cannot match, so the answer would
//! be "refused" on every device. The grant sits in the same rules file as the
//! two actions that are checked, so a device has all three or none.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use tokio::sync::Mutex;
use tracing::info;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};

use super::{SettingsConnectionProxy, uuid_of, with_timeout};

/// Where netplan generates the files NetworkManager loads its profiles from. A
/// profile whose file is here is stored in netplan's YAML.
const NETPLAN_GENERATED: &str = "/run/NetworkManager/system-connections/netplan-";

/// Longer than the unit's own `TimeoutStartSec=60`, so systemd's verdict
/// arrives first when there is one.
const UNIT_TIMEOUT: Duration = Duration::from_secs(70);

/// systemd's error for a second `Subscribe` from the same connection.
const ALREADY_SUBSCRIBED: &str = "org.freedesktop.systemd1.AlreadySubscribed";

/// One forget at a time from here. systemd's subscription belongs to the
/// connection, not to the call, so a second forget's `Unsubscribe` would stop
/// the signal the first is waiting for.
static FORGETTING: Mutex<()> = Mutex::const_new(());

/// The unit that forgets the profile `uuid`.
pub(super) fn unit_name(uuid: &str) -> String {
    format!("lunchbox-wifi-forget@{uuid}.service")
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn subscribe(&self) -> zbus::Result<()>;
    fn unsubscribe(&self) -> zbus::Result<()>;
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    #[zbus(signal)]
    fn job_removed(
        &self,
        id: u32,
        job: ObjectPath<'_>,
        unit: String,
        result: String,
    ) -> zbus::Result<()>;
}

/// Remove the saved profile at `path`, however it is stored.
pub(super) async fn remove_profile(conn: &zbus::Connection, path: &OwnedObjectPath) -> Result<()> {
    let profile = SettingsConnectionProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await?;
    let filename = with_timeout(profile.filename())
        .await
        .context("reading where the profile is stored")?;
    if !filename.starts_with(NETPLAN_GENERATED) {
        return with_timeout(profile.delete())
            .await
            .context("deleting the profile");
    }

    let settings = with_timeout(profile.get_settings())
        .await
        .context("reading the profile")?;
    let uuid = uuid_of(&settings).context("the profile has no UUID")?;
    // The unit's instance name, and from there a file name and the key the
    // helper deletes as root. It checks too; this keeps a request it would
    // refuse from reaching systemd at all.
    let canonical = uuid::Uuid::parse_str(&uuid)
        .map(|u| u.hyphenated().to_string())
        .ok()
        .filter(|canonical| *canonical == uuid)
        .with_context(|| format!("{uuid:?} is not a UUID in the form NetworkManager writes"))?;
    start_and_wait(conn, &unit_name(&canonical)).await
}

/// Start `unit` and wait for systemd to say how its job ended.
async fn start_and_wait(conn: &zbus::Connection, unit: &str) -> Result<()> {
    let _one_at_a_time = FORGETTING.lock().await;
    let manager = SystemdManagerProxy::new(conn).await?;

    // Before starting: the job can end before `StartUnit` has returned.
    let mut removed = manager.receive_job_removed().await?;
    match manager.subscribe().await {
        Ok(()) => {}
        Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == ALREADY_SUBSCRIBED => {}
        Err(e) => return Err(e).context("asking systemd for job signals"),
    }

    let outcome = async {
        let job = with_timeout(manager.start_unit(unit, "replace"))
            .await
            .with_context(|| format!("starting {unit}"))?;
        info!(%unit, "forgetting a Wi-Fi network through netplan");
        let result = tokio::time::timeout(UNIT_TIMEOUT, async {
            while let Some(signal) = removed.next().await {
                let args = signal.args()?;
                if args.job.as_str() == job.as_str() {
                    return Ok(args.result);
                }
            }
            bail!("systemd's signals stopped before {unit} finished")
        })
        .await
        .map_err(|_| anyhow::anyhow!("{unit} did not finish within {UNIT_TIMEOUT:?}"))??;
        match result.as_str() {
            "done" => Ok(()),
            other => bail!("{unit} ended with `{other}`; `journalctl -u {unit}` says why"),
        }
    }
    .await;

    let _ = manager.unsubscribe().await;
    outcome
}
