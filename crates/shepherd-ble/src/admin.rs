//! `AdminRecord`: the per-bond admin identity that survives restarts.
//!
//! Stored as TOML alongside other shepherd persistent state. Also owns
//! the factory-reset sentinel check that runs at daemon startup before
//! the GATT server comes up.

use chrono::{DateTime, Local};
use rand::RngCore;
use serde::{Deserialize, Serialize};
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
            identity_address,
            address_type,
            device_name,
            bonded_at: shepherd_util::now(),
            http_token: random_token(),
            role: AdminRole::Admin,
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

    pub fn load(&self) -> Result<Option<AdminRecord>, AdminStoreError> {
        let Some(text) = self.files.read(ProtectedFile::AdminRecord)? else {
            return Ok(None);
        };
        let wrapped: AdminFile = toml::from_str(&text)?;
        Ok(Some(wrapped.admin))
    }

    pub fn save(&self, record: &AdminRecord) -> Result<(), AdminStoreError> {
        let wrapped = AdminFile {
            admin: record.clone(),
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

#[derive(Serialize, Deserialize)]
struct AdminFile {
    admin: AdminRecord,
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
        assert!(store.load().unwrap().is_none());

        let r = sample_record();
        store.save(&r).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.identity_address, r.identity_address);
        assert_eq!(loaded.http_token, r.http_token);
        assert_eq!(loaded.role, AdminRole::Admin);
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
        store.save(&sample_record()).unwrap();
        assert!(root.join("admin.toml").exists());
    }

    #[test]
    fn clear_removes_file_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(files_in(&dir));
        store.save(&sample_record()).unwrap();
        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
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
