//! Default paths for lunchboxd components
//!
//! Provides centralized path defaults that all crates can use.
//! Paths are user-writable by default (no root required):
//! - Socket: `$XDG_RUNTIME_DIR/lunchboxd/lunchboxd.sock` or `/tmp/lunchboxd-$USER/lunchboxd.sock`
//! - Data: `$XDG_DATA_HOME/lunchboxd` or `~/.local/share/lunchboxd`
//! - Logs: `$XDG_STATE_HOME/lunchboxd` or `~/.local/state/lunchboxd`

use std::path::PathBuf;

/// Environment variable for overriding the socket path
pub const LUNCHBOX_SOCKET_ENV: &str = "LUNCHBOX_SOCKET";

/// Environment variable for overriding the data directory
pub const LUNCHBOX_DATA_DIR_ENV: &str = "LUNCHBOX_DATA_DIR";

/// Socket filename within the socket directory
const SOCKET_FILENAME: &str = "lunchboxd.sock";

/// Application subdirectory name
const APP_DIR: &str = "lunchboxd";

/// Get the default socket path.
///
/// Order of precedence:
/// 1. `$LUNCHBOX_SOCKET` environment variable (if set)
/// 2. `$XDG_RUNTIME_DIR/lunchboxd/lunchboxd.sock` (if XDG_RUNTIME_DIR is set)
/// 3. `/tmp/lunchboxd-$USER/lunchboxd.sock` (fallback)
pub fn default_socket_path() -> PathBuf {
    // Check environment override first
    if let Ok(path) = std::env::var(LUNCHBOX_SOCKET_ENV) {
        return PathBuf::from(path);
    }

    socket_path_without_env()
}

/// Get the socket path without checking LUNCHBOX_SOCKET env var.
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
/// 1. `$LUNCHBOX_DATA_DIR` environment variable (if set)
/// 2. `$XDG_DATA_HOME/lunchboxd` (if XDG_DATA_HOME is set)
/// 3. `~/.local/share/lunchboxd` (fallback)
pub fn default_data_dir() -> PathBuf {
    // Check environment override first
    if let Ok(path) = std::env::var(LUNCHBOX_DATA_DIR_ENV) {
        return PathBuf::from(path);
    }

    data_dir_without_env()
}

/// Get the data directory without checking LUNCHBOX_DATA_DIR env var.
/// Used for default values in configs where the env var is checked separately.
pub fn data_dir_without_env() -> PathBuf {
    // Try XDG_DATA_HOME first
    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(data_home).join(APP_DIR);
    }

    // Fallback to ~/.local/share/lunchboxd
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join(APP_DIR);
    }

    // Last resort
    PathBuf::from("/tmp").join(APP_DIR).join("data")
}

/// The home directory of the user this process runs as.
///
/// `$HOME`, and nothing clever if it is missing: every path lunchboxd cares
/// about is derived from it, and a daemon whose environment has no `HOME` is
/// one whose session never started properly. Callers that can carry on without
/// one (the file manager, issue #195) say so by handling the `None`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Get the default log directory.
///
/// Order of precedence:
/// 1. `$XDG_STATE_HOME/lunchboxd` (if XDG_STATE_HOME is set)
/// 2. `~/.local/state/lunchboxd` (fallback)
pub fn default_log_dir() -> PathBuf {
    // Try XDG_STATE_HOME first
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state_home).join(APP_DIR);
    }

    // Fallback to ~/.local/state/lunchboxd
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

/// Configuration subdirectory name (uses "lunchbox" not "lunchboxd")
const CONFIG_APP_DIR: &str = "lunchbox";

/// Configuration filename
const CONFIG_FILENAME: &str = "config.toml";

/// Get the default configuration file path.
///
/// Returns `$XDG_CONFIG_HOME/lunchbox/config.toml` or `~/.config/lunchbox/config.toml`
pub fn default_config_path() -> PathBuf {
    // Try XDG_CONFIG_HOME first
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(config_home)
            .join(CONFIG_APP_DIR)
            .join(CONFIG_FILENAME);
    }

    // Fallback to ~/.config/lunchbox/config.toml
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
mod scope_tests {
    use super::*;

    /// Which scope each file has is the whole of this design, and it is one
    /// line of `match` away from being wrong in a way nothing else notices: a
    /// device file scoped per-user goes back to being a claim that disagrees
    /// with the bond it names.
    #[test]
    fn the_device_owns_the_admin_record_and_the_user_owns_the_policy() {
        assert_eq!(ProtectedFile::Config.scope(), FileScope::PerUser);
        for file in [
            ProtectedFile::AdminRecord,
            ProtectedFile::ResetSentinel,
            ProtectedFile::UnbondQueue,
        ] {
            assert_eq!(
                file.scope(),
                FileScope::System,
                "{file:?} describes the machine's one Bluetooth adapter, not a child"
            );
        }
    }

    /// A scoped store puts each file under the root its scope names; an
    /// unscoped one puts everything in the single directory it was given,
    /// which is what a device without the custodian has.
    #[test]
    fn the_roots_follow_the_scope() {
        let scoped = LocalProtectedFiles::scoped("/user".into(), "/device".into());
        assert_eq!(
            scoped.path(ProtectedFile::Config),
            std::path::Path::new("/user/config.toml")
        );
        assert_eq!(
            scoped.path(ProtectedFile::AdminRecord),
            std::path::Path::new("/device/admin.toml")
        );

        let flat = LocalProtectedFiles::new("/home".into());
        assert_eq!(
            flat.path(ProtectedFile::Config),
            std::path::Path::new("/home/config.toml")
        );
        assert_eq!(
            flat.path(ProtectedFile::AdminRecord),
            std::path::Path::new("/home/admin.toml"),
            "with one root there is nowhere else for a device file to go"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_contains_lunchboxd() {
        // The socket path should always contain "lunchboxd" regardless of environment
        let path = socket_path_without_env();
        assert!(path.to_string_lossy().contains("lunchboxd"));
        assert!(path.to_string_lossy().contains(".sock"));
    }

    #[test]
    fn data_dir_contains_lunchboxd() {
        let path = data_dir_without_env();
        assert!(path.to_string_lossy().contains("lunchboxd"));
    }

    #[test]
    fn log_dir_contains_lunchboxd() {
        let path = default_log_dir();
        assert!(path.to_string_lossy().contains("lunchboxd"));
    }

    #[test]
    fn socket_dir_is_parent_of_socket_path() {
        let socket = socket_path_without_env();
        let dir = socket_dir();
        assert_eq!(socket.parent().unwrap(), dir);
    }

    #[test]
    fn config_path_contains_lunchbox() {
        let path = default_config_path();
        assert!(path.to_string_lossy().contains("lunchbox"));
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }
}

/// One of lunchbox's protected files (issue #157).
///
/// The files that decide what a child may do: the policy, the BLE admin
/// identity, and the two small records that go with it. They live at a uid
/// activities do not have and are reached through `lunchbox-stated`.
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
    /// spec. Read by the daemon at boot and on every reload; written by an
    /// operator (`sudoedit`, `lunchbox install policy`) or, since issue #185,
    /// by the daemon itself on behalf of the web config editor.
    Config,
    /// `admin.toml` — the bonded admin's identity and the minted HTTP token.
    AdminRecord,
    /// `.factory-reset-ble` — a one-shot instruction, consumed when acted on.
    ResetSentinel,
    /// The queue of BlueZ bonds still to be removed after a factory reset.
    UnbondQueue,
    /// `web-auth.toml` — the management web UI's password hash, its
    /// first-run enrolment code, and the live browser sessions (issue #156).
    WebAuth,
    /// `tls.pem` — the self-signed certificate and key the management API
    /// generated for itself, kept so the fingerprint a parent accepted
    /// survives a restart. Here rather than in the data directory because the
    /// half of it that is a private key must not be readable from an activity.
    TlsCert,
}

/// Who a protected file belongs to.
///
/// Not a detail of where bytes are put: it is the difference between a fact
/// about a *child* and a fact about the *device*, and getting it wrong is
/// visible to a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileScope {
    /// One per kiosk user. What this child may do, and what they have used.
    PerUser,
    /// One for the machine, shared by every kiosk user on it.
    System,
}

impl ProtectedFile {
    /// Whose file this is.
    ///
    /// The admin record, the unbond queue and the reset sentinel are the
    /// device's, not a user's, because the thing they describe is: a BlueZ bond
    /// lives in `/var/lib/bluetooth` at one adapter, owned by root, and
    /// `Adapter::remove_device` forgets it for the whole machine. Keeping the
    /// *claim* per-user while the *bond* was system-wide made a device with two
    /// kiosk users behave in ways nobody chose — a phone claimed for one
    /// arriving at the next already bonded, and a factory reset for one
    /// unpairing the phone from the others.
    ///
    /// The policy is genuinely a per-child fact and stays one.
    pub fn scope(self) -> FileScope {
        match self {
            Self::Config => FileScope::PerUser,
            Self::AdminRecord
            | Self::ResetSentinel
            | Self::UnbondQueue
            | Self::WebAuth
            | Self::TlsCert => FileScope::System,
        }
    }
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
            Self::WebAuth => "web-auth.toml",
            Self::TlsCert => "tls.pem",
        }
    }
}

/// A small store of lunchbox's protected files, wherever they actually live.
///
/// Implemented against the local filesystem in development, and against the
/// custodian's socket on a device. It exists so `lunchbox-ble` can keep the
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
/// uses it for the directory it owns, and `lunchboxd` uses it for the data
/// directory when the custodian is opted out or unreachable. Sharing one
/// implementation is what keeps "protected" and "not protected" from drifting
/// into two different behaviours.
pub struct LocalProtectedFiles {
    dir: PathBuf,
    /// Where the machine's files live, when they live apart from this user's.
    system_dir: Option<PathBuf>,
}

impl LocalProtectedFiles {
    /// Everything in one directory.
    ///
    /// What a device *without* the custodian gets: there is no protected root
    /// to share, so the distinction between a user's files and the machine's
    /// has nowhere to live and the home directory holds both. Also what the
    /// tests use.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            system_dir: None,
        }
    }

    /// A directory per scope: this user's files in `dir`, the machine's in
    /// `system_dir`.
    ///
    /// What the custodian serves. One implementation either way, so
    /// "protected" and "not protected" cannot drift into two behaviours — only
    /// the roots differ.
    pub fn scoped(dir: PathBuf, system_dir: PathBuf) -> Self {
        Self {
            dir,
            system_dir: Some(system_dir),
        }
    }

    fn path(&self, file: ProtectedFile) -> PathBuf {
        let root = match (file.scope(), &self.system_dir) {
            (FileScope::System, Some(system)) => system,
            _ => &self.dir,
        };
        root.join(file.file_name())
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
