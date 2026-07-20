//! `AdminRecord`: the per-bond admin identity that survives restarts.
//!
//! Stored as TOML alongside other shepherd persistent state. Also owns
//! the factory-reset sentinel check that runs at daemon startup before
//! the GATT server comes up.

use chrono::{DateTime, Local};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone)]
pub struct AdminStore {
    path: PathBuf,
}

impl AdminStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<AdminRecord>, AdminStoreError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let text = String::from_utf8(bytes)
            .map_err(|e| AdminStoreError::Toml(format!("non-UTF-8 admin record: {e}")))?;
        let wrapped: AdminFile = toml::from_str(&text)?;
        Ok(Some(wrapped.admin))
    }

    pub fn save(&self, record: &AdminRecord) -> Result<(), AdminStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let wrapped = AdminFile {
            admin: record.clone(),
        };
        let text = toml::to_string_pretty(&wrapped)?;
        // Write to a sibling temp file then rename so a partial write
        // can't leave us with a corrupt admin file.
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn clear(&self) -> Result<(), AdminStoreError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct AdminFile {
    admin: AdminRecord,
}

/// Check for the factory-reset sentinel at daemon startup. If the file
/// exists, delete it and return `true` — the caller is then responsible
/// for clearing the admin record and removing the BlueZ bond before the
/// GATT server starts accepting connections.
pub fn check_reset_sentinel(sentinel_path: &Path) -> bool {
    if !sentinel_path.exists() {
        return false;
    }
    match std::fs::remove_file(sentinel_path) {
        Ok(()) => true,
        Err(e) => {
            // Be conservative: if we can't remove the sentinel we'd loop
            // factory-resetting on every restart, which is worse than
            // failing closed. Log loudly and treat as not-present.
            warn!(
                path = %sentinel_path.display(),
                error = %e,
                "Factory-reset sentinel present but could not be removed; skipping reset",
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

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(dir.path().join("admin.toml"));
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
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(dir.path().join("nested/sub/admin.toml"));
        store.save(&sample_record()).unwrap();
        assert!(store.path().exists());
    }

    #[test]
    fn clear_removes_file_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let store = AdminStore::new(dir.path().join("admin.toml"));
        store.save(&sample_record()).unwrap();
        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
        // Calling clear again on a missing file is a no-op.
        store.clear().unwrap();
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
        let path = dir.path().join(".factory-reset");
        std::fs::write(&path, "").unwrap();
        assert!(check_reset_sentinel(&path));
        assert!(!path.exists());
        // Second call returns false (no file to act on).
        assert!(!check_reset_sentinel(&path));
    }
}
