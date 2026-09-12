//! `AdminRecord`: the per-bond admin identity that survives restarts.
//!
//! Stored as TOML alongside other shepherd persistent state. Also owns
//! the factory-reset sentinel check that runs at daemon startup before
//! the GATT server comes up.
//!
//! There is a *list* of these, not one (issue #149). A household with two
//! caregivers, or a device that travels between two homes, needs more than one
//! phone able to supervise it. The file grew an `[[admins]]` array for that;
//! the v1 single `[admin]` table is still read and migrated on the next write,
//! so a device claimed before this change keeps its admin and its token.

use chrono::{DateTime, Local};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use shepherd_management::AdminSummary;
use shepherd_util::{ProtectedFile, ProtectedFiles};
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AdminRole {
    Admin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AdminRecord {
    /// Stable public handle for this admin, minted once and never reused.
    ///
    /// Not a credential: it is what `revoke_admin` names and what
    /// [`AdminSummary`] hands to a phone listing the others, so it has to be
    /// safe to show. The identity address would have served, but an admin is
    /// revoked and re-enrolled at the same address often enough — a phone
    /// reset, a re-pair — that naming rows by address makes a stale tap in a
    /// list act on a record the parent was not looking at.
    ///
    /// Defaulted rather than required so a v1 record, written before this
    /// field existed, still parses and gets one on load.
    #[serde(default = "new_admin_id")]
    pub id: String,
    /// The BlueZ-resolved identity address for the bonded peer. Once
    /// pairing completes BlueZ presents this address regardless of the
    /// peer's random MAC rotation, so it doubles as the stable identity.
    pub identity_address: String,
    /// `"public"` or `"random"` — matches `bluer`'s `AddressType` enum
    /// so the server can compare on reconnect without a parse step.
    pub address_type: String,
    pub device_name: String,
    pub bonded_at: DateTime<Local>,
    /// Bearer token also accepted by the HTTP API. See the unified-identity
    /// section of the BLE management design.
    pub http_token: String,
    pub role: AdminRole,
}

impl AdminRecord {
    /// Create a fresh admin record with a securely-random HTTP token.
    pub fn new(identity_address: String, address_type: String, device_name: String) -> Self {
        Self {
            id: new_admin_id(),
            identity_address,
            address_type,
            device_name,
            bonded_at: shepherd_util::now(),
            http_token: random_token(),
            role: AdminRole::Admin,
        }
    }
}

impl AdminRecord {
    /// The credential-free view of this record.
    ///
    /// [`AdminSummary`] lives in `shepherd-management` because both transports
    /// return it and that crate sits below this one. It deliberately has no
    /// "is this me?" flag: over HTTP there is no phone to be, and a client
    /// that wants to recognise its own row already knows its own identity
    /// address and can match on it. A flag would have meant either a `viewer`
    /// argument threaded through `ManagementService` for one transport's
    /// benefit, or the device guessing.
    pub fn summary(&self) -> AdminSummary {
        AdminSummary {
            id: self.id.clone(),
            device_name: self.device_name.clone(),
            identity_address: self.identity_address.clone(),
            bonded_at: self.bonded_at,
            role: match self.role {
                AdminRole::Admin => "admin".to_string(),
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum AdminStoreError {
    #[error("admin record I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("admin record TOML error: {0}")]
    Toml(String),
}

impl From<toml::de::Error> for AdminStoreError {
    fn from(e: toml::de::Error) -> Self {
        Self::Toml(e.to_string())
    }
}

impl From<toml::ser::Error> for AdminStoreError {
    fn from(e: toml::ser::Error) -> Self {
        Self::Toml(e.to_string())
    }
}

#[derive(Clone)]
pub struct AdminStore {
    files: Arc<dyn ProtectedFiles>,
}

impl std::fmt::Debug for AdminStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdminStore")
    }
}

impl AdminStore {
    /// Keep the record wherever `files` keeps it.
    ///
    /// One constructor, because there is one place: on a device the state
    /// custodian, at a uid no activity has (issue #157); in a dev stack and the
    /// tests, `LocalProtectedFiles` over a directory. Both are the same trait,
    /// so this store has nothing to decide.
    ///
    /// It used to take a `PathBuf` as well, from a configurable
    /// `admin_record_path`. That knob is gone: the record holds the bonded
    /// admin's identity *and* the minted HTTP token, so a path pointing
    /// anywhere but the custodian's directory is a credential an activity can
    /// read and present to the management API as any remote caller would.
    pub fn new(files: Arc<dyn ProtectedFiles>) -> Self {
        Self { files }
    }

    /// Every admin on this device, oldest first, plus whether the file still
    /// has to be rewritten in the current shape.
    ///
    /// A missing file is no admins, not an error: that is what an unclaimed
    /// device looks like.
    pub fn load(&self) -> Result<StoredAdmins, AdminStoreError> {
        let Some(text) = self.files.read(ProtectedFile::AdminRecord)? else {
            return Ok(StoredAdmins::default());
        };
        let wrapped: AdminFile = toml::from_str(&text)?;
        let AdminFile { mut admins, admin } = wrapped;
        // A v1 file has `[admin]` and no `[[admins]]`. Take it as the first
        // element rather than discarding it: the token in it is the one the
        // paired phone and the HTTP API are both still using, so dropping it
        // would silently unclaim a working device on upgrade.
        let mut needs_migration = false;
        if let Some(v1) = admin {
            needs_migration = true;
            if !admins.iter().any(|a| {
                a.identity_address
                    .eq_ignore_ascii_case(&v1.identity_address)
            }) {
                admins.insert(0, v1);
            }
        }
        Ok(StoredAdmins {
            admins,
            needs_migration,
        })
    }

    /// Replace the whole list. Writing all of it at once is what keeps the
    /// file consistent: there is no operation on one admin that does not also
    /// have to be visible to the check that counts them.
    pub fn save_all(&self, admins: &[AdminRecord]) -> Result<(), AdminStoreError> {
        let wrapped = AdminFile {
            admins: admins.to_vec(),
            admin: None,
        };
        self.files.write(
            ProtectedFile::AdminRecord,
            &toml::to_string_pretty(&wrapped)?,
        )?;
        Ok(())
    }

    pub fn clear(&self) -> Result<(), AdminStoreError> {
        self.files.delete(ProtectedFile::AdminRecord)?;
        Ok(())
    }
}

/// What [`AdminStore::load`] found on disk.
#[derive(Debug, Default)]
pub struct StoredAdmins {
    pub admins: Vec<AdminRecord>,
    /// The file was in the v1 single-`[admin]` shape and wants writing back.
    /// The caller does the write, because only it knows whether the process
    /// got far enough to be trusted with one.
    pub needs_migration: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct AdminFile {
    #[serde(default)]
    admins: Vec<AdminRecord>,
    /// v1's single `[admin]` table, read-only. Never written back — a save
    /// emits `[[admins]]` only, so the shape converges on first write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admin: Option<AdminRecord>,
}

#[derive(Default, Serialize, Deserialize)]
struct PendingUnbondFile {
    #[serde(default)]
    addresses: Vec<String>,
}

/// Peers whose BlueZ bond still needs forgetting, held on disk until the
/// removal actually succeeds.
///
/// Un-claiming a device is two steps — clear the admin record, then tell
/// BlueZ to forget the bond — and only the first is atomic. If the second
/// fails (adapter not ready yet, BlueZ hiccup, daemon killed in between),
/// the peer stays bonded to a device that has no admin: every reconnect
/// is accepted at the link layer and then rejected with `not_claimed`,
/// and re-pairing can't clear it because the bond already exists. That's
/// the "asymmetric bond" lockout.
///
/// The removal used to be best-effort with a comment claiming the next
/// restart would retry. It wouldn't: the admin record was already gone
/// and the sentinel already consumed, so nothing on the next boot knew
/// which address to forget. This file is that missing memory — an entry
/// is written *before* the removal is attempted and deleted only once it
/// succeeds, so a failure at any point just means the next startup tries
/// again.
#[derive(Clone)]
pub struct PendingUnbondStore {
    files: Arc<dyn ProtectedFiles>,
}

impl std::fmt::Debug for PendingUnbondStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingUnbondStore")
    }
}

impl PendingUnbondStore {
    /// Keep the queue with the rest of shepherd's protected files (issue #157).
    ///
    /// It used to derive its own path beside the admin record, under a
    /// different name (`ble-pending-unbond.toml`) from the one the custodian
    /// serves — so the two halves of the same queue disagreed about what the
    /// file was called. One name now, from `ProtectedFile::UnbondQueue`.
    pub fn new(files: Arc<dyn ProtectedFiles>) -> Self {
        Self { files }
    }

    pub fn list(&self) -> Result<Vec<String>, AdminStoreError> {
        let Some(text) = self.files.read(ProtectedFile::UnbondQueue)? else {
            return Ok(Vec::new());
        };
        let parsed: PendingUnbondFile = toml::from_str(&text)?;
        Ok(parsed.addresses)
    }

    /// Record `address` as needing a bond removal. Idempotent.
    pub fn add(&self, address: &str) -> Result<(), AdminStoreError> {
        let mut addresses = self.list()?;
        if addresses.iter().any(|a| a == address) {
            return Ok(());
        }
        addresses.push(address.to_string());
        self.write(&addresses)
    }

    /// Drop `address` from the list — the bond is provably gone. Removes
    /// the file entirely once nothing is left, so the common case leaves
    /// no stray state behind.
    pub fn remove(&self, address: &str) -> Result<(), AdminStoreError> {
        let mut addresses = self.list()?;
        let before = addresses.len();
        addresses.retain(|a| a != address);
        if addresses.len() == before {
            return Ok(());
        }
        if addresses.is_empty() {
            self.files.delete(ProtectedFile::UnbondQueue)?;
            return Ok(());
        }
        self.write(&addresses)
    }

    fn write(&self, addresses: &[String]) -> Result<(), AdminStoreError> {
        // `ProtectedFiles` does a temp-then-rename on both sides: a torn write
        // here would strand a bond with no record of it.
        self.files.write(
            ProtectedFile::UnbondQueue,
            &toml::to_string_pretty(&PendingUnbondFile {
                addresses: addresses.to_vec(),
            })?,
        )?;
        Ok(())
    }
}

/// Check for the factory-reset sentinel at daemon startup. If it is there,
/// consume it and return `true` — the caller is then responsible for clearing
/// the admin record and removing the BlueZ bond before the GATT server starts
/// accepting connections.
///
/// One form, over `ProtectedFiles`. The path-based twin went with the
/// `reset_sentinel_path` knob that fed it: a sentinel an activity could write
/// is a way for a game to unpair the phone supervising it, which is why the
/// file belongs where the custodian keeps it and nowhere else (issue #157).
pub fn check_reset_sentinel(files: &dyn ProtectedFiles) -> bool {
    match files.take(ProtectedFile::ResetSentinel) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            warn!(
                error = %e,
                "Could not read the factory-reset sentinel from the state custodian; \
                 skipping reset",
            );
            false
        }
    }
}

fn random_token() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    hex_encode(&buf)
}

/// A short public handle for an admin row. Long enough not to collide across
/// the handful of phones a household has; not a credential.
pub(crate) fn new_admin_id() -> String {
    let mut buf = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    hex_encode(&buf)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_record() -> AdminRecord {
        AdminRecord::new(
            "AA:BB:CC:DD:EE:FF".to_string(),
            "public".to_string(),
            "Test phone".to_string(),
        )
    }

    /// What a dev stack passes: the files rooted at a directory, which is the
    /// same implementation the custodian uses on its own side of the socket.
    fn files_in(dir: &TempDir) -> Arc<dyn ProtectedFiles> {
        Arc::new(shepherd_util::LocalProtectedFiles::new(
            dir.path().to_path_buf(),
        ))
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(files_in(&dir));
        assert!(store.load().unwrap().admins.is_empty());

        let r = sample_record();
        store.save_all(std::slice::from_ref(&r)).unwrap();
        let loaded = store.load().unwrap();
        assert!(!loaded.needs_migration);
        assert_eq!(loaded.admins.len(), 1);
        assert_eq!(loaded.admins[0].id, r.id);
        assert_eq!(loaded.admins[0].identity_address, r.identity_address);
        assert_eq!(loaded.admins[0].http_token, r.http_token);
        assert_eq!(loaded.admins[0].role, AdminRole::Admin);
    }

    #[test]
    fn save_then_load_round_trips_several() {
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(files_in(&dir));
        let a = sample_record();
        let b = AdminRecord::new("11:22:33:44:55:66".into(), "public".into(), "Second".into());
        store.save_all(&[a.clone(), b.clone()]).unwrap();

        let loaded = store.load().unwrap().admins;
        assert_eq!(loaded.len(), 2);
        // Order is preserved: the list is oldest-first and a UI shows it that
        // way, so a save must not reshuffle it.
        assert_eq!(loaded[0].id, a.id);
        assert_eq!(loaded[1].id, b.id);
        // Ids are distinct, which is what makes revoke-by-id unambiguous.
        assert_ne!(loaded[0].id, loaded[1].id);
        assert_ne!(loaded[0].http_token, loaded[1].http_token);
    }

    /// A device claimed before #149 has a single `[admin]` table and no `id`.
    /// It must come back as one admin, with its token intact — anything else
    /// silently unclaims a working device on upgrade — and be flagged for
    /// rewriting in the current shape.
    #[test]
    fn v1_single_admin_file_migrates() {
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(files_in(&dir));
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(ProtectedFile::AdminRecord.file_name()),
            r#"
[admin]
identity_address = "AA:BB:CC:DD:EE:FF"
address_type = "public"
device_name = "Pixel 10a"
bonded_at = "2026-09-07T15:11:40.684313362-04:00"
http_token = "5ee6bfbb85bb23212c32763c0067a212b160b1dff6d895f6711d944d7f788989"
role = "admin"
"#,
        )
        .unwrap();

        let loaded = store.load().unwrap();
        assert!(loaded.needs_migration);
        assert_eq!(loaded.admins.len(), 1);
        assert_eq!(loaded.admins[0].identity_address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(
            loaded.admins[0].http_token,
            "5ee6bfbb85bb23212c32763c0067a212b160b1dff6d895f6711d944d7f788989"
        );
        assert!(!loaded.admins[0].id.is_empty(), "migration mints an id");

        // Writing it back converges the shape: the reread is clean.
        store.save_all(&loaded.admins).unwrap();
        let again = store.load().unwrap();
        assert!(!again.needs_migration);
        assert_eq!(again.admins[0].id, loaded.admins[0].id);
    }

    #[test]
    fn save_creates_missing_parent() {
        // `LocalProtectedFiles` is rooted at a directory that need not exist
        // yet -- a fresh device's data directory does not.
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("nested/sub");
        let store = AdminStore::new(Arc::new(shepherd_util::LocalProtectedFiles::new(
            root.clone(),
        )));
        store.save_all(&[sample_record()]).unwrap();
        assert!(root.join("admin.toml").exists());
    }

    #[test]
    fn clear_removes_file_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(files_in(&dir));
        store.save_all(&[sample_record()]).unwrap();
        store.clear().unwrap();
        assert!(store.load().unwrap().admins.is_empty());
        // Calling clear again on a missing file is a no-op.
        store.clear().unwrap();
    }

    #[test]
    fn pending_unbond_survives_until_removal_succeeds() {
        let dir = TempDir::new().unwrap();
        let store = PendingUnbondStore::new(files_in(&dir));
        assert!(store.list().unwrap().is_empty());

        store.add("AA:BB:CC:DD:EE:FF").unwrap();
        store.add("AA:BB:CC:DD:EE:FF").unwrap(); // idempotent
        store.add("11:22:33:44:55:66").unwrap();
        assert_eq!(store.list().unwrap().len(), 2);

        // A fresh handle on the same directory sees the list — this is the
        // whole point: the retry has to survive a daemon restart.
        let reopened = PendingUnbondStore::new(files_in(&dir));
        assert_eq!(reopened.list().unwrap().len(), 2);

        reopened.remove("AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(store.list().unwrap(), vec!["11:22:33:44:55:66".to_string()]);

        // Draining the last entry cleans the file up rather than leaving
        // an empty list behind.
        reopened.remove("11:22:33:44:55:66").unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(!dir.path().join("unbond-queue.toml").exists());
        // Removing something absent is a no-op, not an error.
        reopened.remove("11:22:33:44:55:66").unwrap();
    }

    #[test]
    fn pending_unbond_creates_missing_parent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("nested/sub");
        let store = PendingUnbondStore::new(Arc::new(shepherd_util::LocalProtectedFiles::new(
            root.clone(),
        )));
        store.add("AA:BB:CC:DD:EE:FF").unwrap();
        assert!(root.join("unbond-queue.toml").exists());
    }

    #[test]
    fn random_tokens_differ() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn reset_sentinel_consumed_on_check() {
        let dir = TempDir::new().unwrap();
        let files = files_in(&dir);
        let path = dir.path().join(ProtectedFile::ResetSentinel.file_name());
        std::fs::write(&path, "").unwrap();
        assert!(check_reset_sentinel(files.as_ref()));
        assert!(!path.exists());
        // Second call returns false (no file to act on).
        assert!(!check_reset_sentinel(files.as_ref()));
    }
}
