//! Default paths for shepherdd components
//!
//! Provides centralized path defaults that all crates can use.
//! Paths are user-writable by default (no root required):
//! - Socket: `$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock` or `/tmp/shepherdd-$USER/shepherdd.sock`
//! - Data: `$XDG_DATA_HOME/shepherdd` or `~/.local/share/shepherdd`
//! - Logs: `$XDG_STATE_HOME/shepherdd` or `~/.local/state/shepherdd`

use std::path::PathBuf;

/// Environment variable for overriding the socket path
pub const SHEPHERD_SOCKET_ENV: &str = "SHEPHERD_SOCKET";

/// Environment variable for overriding the data directory
pub const SHEPHERD_DATA_DIR_ENV: &str = "SHEPHERD_DATA_DIR";

/// Socket filename within the socket directory
const SOCKET_FILENAME: &str = "shepherdd.sock";

/// Application subdirectory name
const APP_DIR: &str = "shepherdd";

/// Get the default socket path.
///
/// Order of precedence:
/// 1. `$SHEPHERD_SOCKET` environment variable (if set)
/// 2. `$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock` (if XDG_RUNTIME_DIR is set)
/// 3. `/tmp/shepherdd-$USER/shepherdd.sock` (fallback)
pub fn default_socket_path() -> PathBuf {
    // Check environment override first
    if let Ok(path) = std::env::var(SHEPHERD_SOCKET_ENV) {
        return PathBuf::from(path);
    }

    socket_path_without_env()
}

/// Get the socket path without checking SHEPHERD_SOCKET env var.
/// Used for default values in configs where the env var is checked separately.
pub fn socket_path_without_env() -> PathBuf {
    // Try XDG_RUNTIME_DIR first (typically /run/user/<uid>)
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir)
            .join(APP_DIR)
            .join(SOCKET_FILENAME);
    }

    // Fallback to /tmp with username
    let username = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    PathBuf::from(format!("/tmp/{}-{}", APP_DIR, username)).join(SOCKET_FILENAME)
}

/// Get the default data directory.
///
/// Order of precedence:
/// 1. `$SHEPHERD_DATA_DIR` environment variable (if set)
/// 2. `$XDG_DATA_HOME/shepherdd` (if XDG_DATA_HOME is set)
/// 3. `~/.local/share/shepherdd` (fallback)
pub fn default_data_dir() -> PathBuf {
    // Check environment override first
    if let Ok(path) = std::env::var(SHEPHERD_DATA_DIR_ENV) {
        return PathBuf::from(path);
    }

    data_dir_without_env()
}

/// Get the data directory without checking SHEPHERD_DATA_DIR env var.
/// Used for default values in configs where the env var is checked separately.
pub fn data_dir_without_env() -> PathBuf {
    // Try XDG_DATA_HOME first
    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(data_home).join(APP_DIR);
    }

    // Fallback to ~/.local/share/shepherdd
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join(APP_DIR);
    }

    // Last resort
    PathBuf::from("/tmp").join(APP_DIR).join("data")
}

/// Get the default log directory.
///
/// Order of precedence:
/// 1. `$XDG_STATE_HOME/shepherdd` (if XDG_STATE_HOME is set)
/// 2. `~/.local/state/shepherdd` (fallback)
pub fn default_log_dir() -> PathBuf {
    // Try XDG_STATE_HOME first
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state_home).join(APP_DIR);
    }

    // Fallback to ~/.local/state/shepherdd
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join(APP_DIR);
    }

    // Last resort
    PathBuf::from("/tmp").join(APP_DIR).join("logs")
}

/// Get the parent directory of the socket (for creating it)
pub fn socket_dir() -> PathBuf {
    let socket_path = socket_path_without_env();
    socket_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            // Should never happen with our paths, but just in case
            PathBuf::from("/tmp").join(APP_DIR)
        })
}

/// Configuration subdirectory name (uses "shepherd" not "shepherdd")
const CONFIG_APP_DIR: &str = "shepherd";

/// Configuration filename
const CONFIG_FILENAME: &str = "config.toml";

/// Get the default configuration file path.
///
/// Returns `$XDG_CONFIG_HOME/shepherd/config.toml` or `~/.config/shepherd/config.toml`
pub fn default_config_path() -> PathBuf {
    // Try XDG_CONFIG_HOME first
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(config_home)
            .join(CONFIG_APP_DIR)
            .join(CONFIG_FILENAME);
    }

    // Fallback to ~/.config/shepherd/config.toml
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join(CONFIG_APP_DIR)
            .join(CONFIG_FILENAME);
    }

    // Last resort (unlikely to be valid, but provides a fallback)
    PathBuf::from("/etc")
        .join(CONFIG_APP_DIR)
        .join(CONFIG_FILENAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_contains_shepherdd() {
        // The socket path should always contain "shepherdd" regardless of environment
        let path = socket_path_without_env();
        assert!(path.to_string_lossy().contains("shepherdd"));
        assert!(path.to_string_lossy().contains(".sock"));
    }

    #[test]
    fn data_dir_contains_shepherdd() {
        let path = data_dir_without_env();
        assert!(path.to_string_lossy().contains("shepherdd"));
    }

    #[test]
    fn log_dir_contains_shepherdd() {
        let path = default_log_dir();
        assert!(path.to_string_lossy().contains("shepherdd"));
    }

    #[test]
    fn socket_dir_is_parent_of_socket_path() {
        let socket = socket_path_without_env();
        let dir = socket_dir();
        assert_eq!(socket.parent().unwrap(), dir);
    }

    #[test]
    fn config_path_contains_shepherd() {
        let path = default_config_path();
        assert!(path.to_string_lossy().contains("shepherd"));
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }
}

/// One of shepherd's protected files (issue #157).
///
/// The files that decide what a child may do: the policy, the BLE admin
/// identity, and the two small records that go with it. They live at a uid
/// activities do not have and are reached through `shepherd-stated`.
///
/// **An enum rather than a path**, deliberately. The custodian serves these
/// over a socket, and a request that carried a *name* would need validating
/// against an allow-list on every call — a check that can be got wrong once and
/// then serves arbitrary files out of a directory whose whole point is that
/// nothing else can read it. A closed set cannot express a path at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedFile {
    /// `config.toml` — every entry, limit, availability window and firewall
    /// spec. Read by the daemon; written out of band by an operator.
    Config,
    /// `admin.toml` — the bonded admin's identity and the minted HTTP token.
    AdminRecord,
    /// `.factory-reset-ble` — a one-shot instruction, consumed when acted on.
    ResetSentinel,
    /// The queue of BlueZ bonds still to be removed after a factory reset.
    UnbondQueue,
}

impl ProtectedFile {
    /// The file's name inside the protected directory.
    ///
    /// Only the custodian calls this: it is the one process that turns the
    /// closed set back into a path, in a directory it owns.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Config => "config.toml",
            Self::AdminRecord => "admin.toml",
            Self::ResetSentinel => ".factory-reset-ble",
            Self::UnbondQueue => "unbond-queue.toml",
        }
    }
}

/// A small store of shepherd's protected files, wherever they actually live.
///
/// Implemented against the local filesystem in development, and against the
/// custodian's socket on a device. It exists so `shepherd-ble` can keep the
/// admin record without knowing whether it is a file it can open or one it has
/// to ask for — and without depending on the wire crate to find out.
pub trait ProtectedFiles: Send + Sync {
    /// The file's contents, or `None` if it does not exist.
    fn read(&self, file: ProtectedFile) -> std::io::Result<Option<String>>;
    /// Replace the file's contents.
    fn write(&self, file: ProtectedFile, contents: &str) -> std::io::Result<()>;
    /// Remove it. Returns whether it was there.
    fn delete(&self, file: ProtectedFile) -> std::io::Result<bool>;
    /// Read it and remove it in one step.
    ///
    /// Separate from `read` + `delete` because the sentinel is an instruction
    /// rather than state: acting on it twice factory-resets a device that has
    /// already been reset, and doing it in two calls leaves a window where a
    /// restart does exactly that.
    fn take(&self, file: ProtectedFile) -> std::io::Result<Option<String>>;
}

/// [`ProtectedFiles`] against a directory this process can open.
///
/// Two callers, and they are opposite ends of the same design: the custodian
/// uses it for the directory it owns, and `shepherdd` uses it for the data
/// directory when the custodian is opted out or unreachable. Sharing one
/// implementation is what keeps "protected" and "not protected" from drifting
/// into two different behaviours.
pub struct LocalProtectedFiles {
    dir: PathBuf,
}

impl LocalProtectedFiles {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, file: ProtectedFile) -> PathBuf {
        self.dir.join(file.file_name())
    }
}

impl ProtectedFiles for LocalProtectedFiles {
    fn read(&self, file: ProtectedFile) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(self.path(file)) {
            Ok(contents) => Ok(Some(contents)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&self, file: ProtectedFile, contents: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        // Through a temp file and a rename, so a partial write cannot leave a
        // corrupt policy or admin record behind. `AdminStore` already did this
        // for its own file; doing it here means every protected file gets it.
        let target = self.path(file);
        let tmp = target.with_extension("tmp");
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, &target)
    }

    fn delete(&self, file: ProtectedFile) -> std::io::Result<bool> {
        match std::fs::remove_file(self.path(file)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn take(&self, file: ProtectedFile) -> std::io::Result<Option<String>> {
        let contents = self.read(file)?;
        if contents.is_some() {
            self.delete(file)?;
        }
        Ok(contents)
    }
}
