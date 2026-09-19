//! Remote file management for the web interface (issue #195).
//!
//! A hardened kiosk account denies SSH and keeps its home at mode 0700, so
//! `scp`, `rsync` and every SFTP file manager are shut out of exactly the
//! directory a parent has to put a book, a ROM or a video into. The workaround
//! — copy in as the admin user and `sudo mv` — leaves root-owned files in a
//! child's home. shepherdd already runs *as* that user, so it is the one
//! process on the device for which none of that is a problem.
//!
//! ## Shape
//!
//! A caller names a **root** and a path inside it, and never an absolute path.
//! Roots are enumerated by the server ([`roots`]); a location it did not offer
//! has no spelling. Everything else goes through [`resolve::resolve`].
//!
//! ## Not on `ManagementService`
//!
//! These are HTTP routes, like `/api/v1/config` and for the same reason:
//! `#[management_rpc]` carries every async trait method to BLE, whose frames
//! cap at 16 KiB, and a file transfer there would be a method that exists and
//! cannot work. Nothing here reaches the companion, which is also the decision
//! recorded in `docs/ai/history/2026-09-11 004 remote-file-manager-api (#195).md`.

pub mod resolve;
pub mod roots;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::json;
use shepherd_config::FileManagerConfig;
use tokio::sync::watch;

pub use roots::{RootInfo, RootKind};

/// What went wrong, in the vocabulary the rest of this API already speaks.
///
/// Deliberately **not** `ManagementError`. That enum maps `Conflict` onto
/// `412`, because a policy write has exactly one kind of conflict — a stale
/// `If-Match`. A file API needs `409` ("something is already there") and `412`
/// ("what is there is not what you expected") to mean different things, and
/// borrowing the enum is how those two quietly become one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    BadRequest(String),
    NotFound(String),
    Forbidden(String),
    Conflict(String),
    PreconditionRequired(String),
    PreconditionFailed(String),
    TooLarge(String),
    RangeNotSatisfiable(String),
    InsufficientStorage(String),
    Internal(String),
}

impl FileError {
    fn parts(&self) -> (StatusCode, &'static str, &str) {
        match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            Self::PreconditionRequired(m) => (
                StatusCode::PRECONDITION_REQUIRED,
                "precondition_required",
                m,
            ),
            Self::PreconditionFailed(m) => {
                (StatusCode::PRECONDITION_FAILED, "precondition_failed", m)
            }
            Self::TooLarge(m) => (StatusCode::PAYLOAD_TOO_LARGE, "too_large", m),
            Self::RangeNotSatisfiable(m) => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
                m,
            ),
            Self::InsufficientStorage(m) => {
                (StatusCode::INSUFFICIENT_STORAGE, "insufficient_storage", m)
            }
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", m),
        }
    }

    /// Turn an `io::Error` from an operation on `path` into the closest thing
    /// a caller can act on.
    pub fn from_io(e: &std::io::Error, what: &str) -> Self {
        // The errno arms come first, because a removable drive's refusals are
        // answers about the *request* — a name FAT cannot spell, a file past
        // what the format can hold — and `ErrorKind` either flattens them into
        // something misleading or leaves them uncategorised. Every one of
        // these reached a caller as `500 internal` until a FAT stick was
        // tested against.
        match e.raw_os_error() {
            // ENOSPC arriving mid-write, after the free-space check passed —
            // somebody else filled the disk while the upload was in flight.
            Some(28) => {
                return Self::InsufficientStorage("the disk filled up during the write".into());
            }
            // EINVAL. On FAT and exFAT this is how the kernel says the name
            // contains a character the format has no room for.
            Some(22) => {
                return Self::BadRequest(format!(
                    "{what}: that name is not one this drive can store — \
                     removable drives cannot hold : * ? \" < > | or \\ in a name"
                ));
            }
            // EFBIG: past what the format can address at all, which on FAT32
            // is 4 GiB however much space is free.
            Some(27) => {
                return Self::TooLarge(format!(
                    "{what} is larger than this drive can store in one file"
                ));
            }
            // EROFS: mounted read-only, usually because the drive was pulled
            // out mid-write once and has a dirty bit set.
            Some(30) => {
                return Self::Forbidden(format!("{what}: this drive is mounted read-only"));
            }
            // ENAMETOOLONG.
            Some(36) => {
                return Self::BadRequest(format!("{what}: that name is too long for this drive"));
            }
            _ => {}
        }
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound(format!("{what} is not on this device")),
            std::io::ErrorKind::PermissionDenied => {
                Self::Forbidden(format!("this device's kiosk user may not touch {what}"))
            }
            std::io::ErrorKind::AlreadyExists => Self::Conflict(format!("{what} already exists")),
            _ => Self::Internal(format!("{what}: {e}")),
        }
    }
}

impl IntoResponse for FileError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}

/// What kind of thing a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    /// A socket, fifo or device node. Listed so it is not invisible, and not
    /// something this API will open.
    Other,
}

/// One row of a directory listing.
#[derive(Debug, Clone, Serialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub modified: Option<DateTime<Local>>,
    /// `"<size>-<mtime_nanos>"`, opaque and compared only for equality.
    ///
    /// Deliberately not the content hash `PolicyDocument::version_of` uses: a
    /// policy is tens of kilobytes and worth hashing so that a restore from
    /// backup reads as unchanged, while a directory of ROMs is gigabytes and
    /// re-hashing it to draw a list is not the same trade.
    pub etag: Option<String>,
    pub hidden: bool,
    pub symlink: bool,
    /// Why this API will not act on the entry, or absent when it will.
    ///
    /// A reason rather than a bare `usable: false`, because the answers differ
    /// in what they still allow: an escaping symlink can be *deleted* — which
    /// is the whole reason it is listed rather than hidden — while a name that
    /// is not valid UTF-8 cannot be addressed at all, so nothing can be done
    /// to it from here. A client given one flag for both would have to guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unusable: Option<UnusableReason>,
    /// For a **directory**: whether this device's kiosk user may create,
    /// rename and delete inside it. `None` for anything else.
    ///
    /// Only directories carry it because only directories answer the question
    /// a client actually asks. Deleting or renaming a *file* needs write
    /// permission on its parent, not on the file — so that answer is the
    /// containing [`Listing::writable`], and a `writable` on a file row would
    /// be a field that looks like the one you want and is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
}

/// Why an entry is listed but cannot be operated on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnusableReason {
    /// A symlink whose target leaves the root. Listed so a person can see it
    /// and **delete** it; never followed, never read, never written through.
    SymlinkEscapes,
    /// The name is not valid UTF-8. It can only be shown lossily, and a lossy
    /// name cannot be turned back into the bytes that address the right file,
    /// so nothing — including a delete — can be done to it here. Nothing
    /// uploaded through this API can ever be in this state; it describes what
    /// was already on the disk.
    NameNotUtf8,
    /// A socket, fifo, device node or other special file. Listed so it is not
    /// invisible, and not something this API will open.
    SpecialFile,
    /// One of shepherd's own directories, or `~/.ssh`. Refused for reading and
    /// writing alike — see [`denied_dirs`].
    NotBrowsable,
    /// The directory named it, and then nothing could be learned about it —
    /// no size, no kind, no timestamp.
    ///
    /// Not hypothetical, and not always a failing disk. A FAT drive mounted
    /// with a charset that cannot represent a stored name renders it with
    /// literal `?` characters, and that rendering is not a name the filesystem
    /// can look up again; the same is true of an entry another writer unlinks
    /// between the directory being read and the row being built. Listed
    /// anyway, because an entry that is *there* and merely unexplainable is
    /// still something a person needs to know about — and, unlike
    /// [`Self::NameNotUtf8`], its name may well still address it well enough
    /// to delete.
    Unreadable,
}

/// A page of one directory.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub root: String,
    pub path: String,
    /// Whether this device's kiosk user may create, rename and delete in the
    /// directory being listed.
    ///
    /// This is what a client needs to decide whether the delete and rename
    /// controls on these rows do anything: both are operations on the
    /// *parent*, not on the entry. Without it a root-owned folder inside a
    /// writable root refuses after the click instead of greying out before it.
    pub writable: bool,
    pub entries: Vec<DirEntryInfo>,
    pub truncated: bool,
    pub cursor: Option<String>,
    /// How many entries the directory reported that could not be read *at
    /// all* — not even their names.
    ///
    /// Those cannot become rows, because a row with no name is not something
    /// anybody can act on or even recognise. They are counted instead, so that
    /// the one answer a file manager must never give — a list that is quietly
    /// short — is not what a failing USB stick produces.
    #[serde(skip_serializing_if = "is_zero")]
    pub unreadable: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Default and maximum page sizes.
///
/// A directory holding a ROM set really does have tens of thousands of files
/// in it, and a listing that tried to be one response would be a browser tab
/// that stops responding.
pub const DEFAULT_LIMIT: usize = 1000;
pub const MAX_LIMIT: usize = 5000;

/// A root plus a path inside it that has passed every check.
///
/// The final component is deliberately *not* resolved — see
/// [`resolve::resolve`]. An operation that will follow it (a listing, a
/// download) asks [`Located::follow`] first; one that must not (an upload, a
/// delete) uses [`Located::path`] as it stands.
pub struct Located {
    pub root: RootInfo,
    pub path: PathBuf,
}

impl Located {
    /// The target with every link resolved, having proved the result is still
    /// inside the root.
    ///
    /// This is what stops `path=escape` — a symlink out of the home, which any
    /// activity at the kiosk uid can create — from listing or handing over
    /// whatever it points at. `resolve` cannot do it: not following the last
    /// component is what keeps a link swapped in after the check from being
    /// the thing the check looked at.
    pub fn follow(&self, denied: &[PathBuf]) -> Result<PathBuf, FileError> {
        let canonical =
            std::fs::canonicalize(&self.path).map_err(|e| FileError::from_io(&e, "that path"))?;
        if !canonical.starts_with(&self.root.canonical) {
            return Err(FileError::Forbidden(
                "that leads outside the folder it was asked for".into(),
            ));
        }
        resolve::check_denied(&canonical, denied)?;
        Ok(canonical)
    }
}

/// The file manager, as the HTTP handlers see it.
pub struct FileService {
    /// shepherdd's own home directory, canonicalised once.
    home: PathBuf,
    /// The device id of the filesystem that home is on.
    ///
    /// What the free-space floor is *for*: a kiosk whose own disk fills up is
    /// a session that will not start. A removable drive filling up costs
    /// nobody an evening, so the floor does not apply there — and applied
    /// everywhere it made any drive smaller than the floor unwritable, which
    /// is most USB sticks.
    home_device: Option<u64>,
    settings: watch::Receiver<Arc<FileManagerConfig>>,
    /// shepherd's own directories inside the home, refused for read and write
    /// alike. See [`resolve::check_denied`].
    denied: Vec<PathBuf>,
}

impl FileService {
    /// Build the service around a live settings channel.
    ///
    /// A channel rather than a value because `reload_config` and the policy
    /// watcher both change `extra_roots` and the caps without restarting the
    /// daemon, and a parent who edits the config in the web editor expects the
    /// next page load to reflect it.
    pub fn new(home: PathBuf, settings: watch::Receiver<Arc<FileManagerConfig>>) -> Self {
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        let denied = denied_dirs(&home);
        let home_device = std::fs::metadata(&home)
            .ok()
            .map(|meta| std::os::unix::fs::MetadataExt::dev(&meta));
        Self {
            home,
            home_device,
            settings,
            denied,
        }
    }

    /// The same thing with settings that never change — tests, and an
    /// embedding with no reload path.
    pub fn fixed(home: PathBuf, settings: FileManagerConfig) -> Self {
        // The sender is dropped immediately and deliberately: a `watch`
        // receiver keeps serving the last value it was given after the sender
        // is gone, which is exactly "these settings, forever".
        let (tx, rx) = watch::channel(Arc::new(settings));
        drop(tx);
        Self::new(home, rx)
    }

    /// Also refuse `dir`, and everything under it.
    ///
    /// [`denied_dirs`] works the database's location out from the environment,
    /// which is a guess: `shepherdd -d /srv/library` puts it somewhere else
    /// entirely, and a `-d` pointing inside the home would leave the database
    /// downloadable while the refusal quietly guarded a path nothing was at.
    /// The process that opened the store is the one that knows where it is, so
    /// it says so here rather than being second-guessed.
    pub fn also_deny(mut self, dir: PathBuf) -> Self {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !self.denied.contains(&dir) {
            self.denied.push(dir);
        }
        self
    }

    pub fn settings(&self) -> Arc<FileManagerConfig> {
        self.settings.borrow().clone()
    }

    pub fn denied(&self) -> &[PathBuf] {
        &self.denied
    }

    /// Whether the free-space floor applies to writes landing on `meta`'s
    /// filesystem — that is, whether this is the device's own disk.
    pub fn floor_applies(&self, meta: &std::fs::Metadata) -> bool {
        match self.home_device {
            Some(home) => std::os::unix::fs::MetadataExt::dev(meta) == home,
            // Unknown: keep the floor, which is the cautious direction and the
            // behaviour before this existed.
            None => true,
        }
    }

    /// Everywhere this device will let a caller browse, right now.
    pub fn roots(&self) -> Vec<RootInfo> {
        roots::enumerate(&self.home, &self.settings())
    }

    /// One root by id.
    ///
    /// A drive unplugged mid-session lands here as `404`, which is the honest
    /// answer and what sends the UI back to the roots list.
    pub fn root(&self, id: &str) -> Result<RootInfo, FileError> {
        self.roots()
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| FileError::NotFound("that place is not on this device".into()))
    }

    /// Resolve `root` + `path` into something openable.
    pub fn locate(&self, root: &str, path: &str) -> Result<Located, FileError> {
        let root = self.root(root)?;
        let resolved = resolve::resolve(&root.canonical, path, &self.denied)?;
        Ok(Located {
            root,
            path: resolved,
        })
    }
}

/// Directories inside a home this API refuses to serve.
///
/// Two of the three are about damage nobody would connect back to the edit
/// rather than about privilege — a caller who reaches these routes can already
/// rewrite the policy, which is strictly more power. The video cache keeps an
/// index that hand-deletion desynchronises, and the data directory holds a
/// SQLite database that a half-finished upload corrupts.
///
/// `~/.ssh` is the exception, and it is here on its own merits: a private key
/// is a credential for somewhere *else*, so handing it out is the one thing on
/// this surface that is not already implied by "this caller administers this
/// device". A kiosk home does not normally have one; a device whose home does
/// should not publish it over HTTP.
///
/// `~/.local/state/shepherdd` is deliberately *not* here. It is the log
/// directory, and pulling `shepherdd.log` off a device that has no shell is
/// one of the more useful things this feature does.
fn denied_dirs(home: &Path) -> Vec<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("shepherdd");
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".cache"))
        .join("shepherd");
    [data, cache, home.join(".ssh")]
        .into_iter()
        // Canonical where it can be, because what it is compared against is:
        // a lexical path would not match a home reached through a symlink.
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect()
}

/// How precisely a filesystem records modification times.
///
/// The etag is `size-mtime`, so this is the window inside which a file can be
/// rewritten *without the tag changing* — which is exactly what a weak
/// validator means in HTTP, and exactly what a resumed download must not
/// trust. On ext4 and friends it is a nanosecond and nothing is ever weak; on
/// the FAT and exFAT drives this device is expected to have plugged into it,
/// it is two seconds, and two different files of the same size written in the
/// same tick really do produce the same tag. Measured, not assumed — see
/// `docs/ai/history/2026-09-14 003 …`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Granularity(pub Duration);

impl Default for Granularity {
    fn default() -> Self {
        Self::FINE
    }
}

impl Granularity {
    /// What ext4, xfs, btrfs and tmpfs do: enough resolution that a second
    /// write always moves the tag.
    pub const FINE: Self = Self(Duration::from_nanos(1));
    /// FAT and exFAT, whose directory entries hold two-second resolution.
    pub const COARSE: Self = Self(Duration::from_secs(2));

    /// Ask the filesystem holding `path`.
    ///
    /// Anything this does not recognise is treated as fine-grained, which is
    /// the same answer it would have given before this existed. Being wrong in
    /// that direction only costs the safety this adds; being wrong the other
    /// way would refuse every resume on an ordinary disk.
    pub fn of(path: &Path) -> Self {
        use nix::sys::statfs::{MSDOS_SUPER_MAGIC, statfs};
        // exFAT has no constant in `nix`; its superblock magic is this, and
        // the kernel has used it since exfat landed in 5.7.
        const EXFAT_SUPER_MAGIC: i64 = 0x2011_BAB0;
        match statfs(path) {
            Ok(stat) => {
                let fs = stat.filesystem_type();
                #[allow(clippy::unnecessary_cast)]
                let raw = fs.0 as i64;
                if fs == MSDOS_SUPER_MAGIC || raw == EXFAT_SUPER_MAGIC {
                    Self::COARSE
                } else {
                    Self::FINE
                }
            }
            Err(_) => Self::FINE,
        }
    }
}

/// A file's validator, and whether it can be trusted byte-for-byte.
///
/// Weak means "this tag may not change when the content does", which HTTP
/// already has a spelling for (`W/"…"`) and already has rules about: a weak
/// validator may not be used to assemble a range, and `If-Match` compares
/// strongly, so a weak tag never satisfies one. Both of those are the
/// behaviour this needs on a FAT drive, for free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validator {
    pub value: String,
    pub weak: bool,
}

impl Validator {
    /// How it goes on the wire.
    pub fn header(&self) -> String {
        if self.weak {
            format!("W/\"{}\"", self.value)
        } else {
            format!("\"{}\"", self.value)
        }
    }

    /// Whether a caller's tag matches this one *strongly*, which is what
    /// `If-Match` and range assembly require. A weak validator never does.
    pub fn strongly_matches(&self, candidate: &str) -> bool {
        let candidate = candidate.trim();
        if self.weak || candidate.starts_with("W/") {
            return false;
        }
        candidate.trim_matches('"') == self.value
    }
}

/// The tag for a file, and whether the filesystem can be trusted to change it.
///
/// A file written *within the last tick* of a coarse-grained filesystem is the
/// dangerous case: another write in the same tick would leave the tag alone.
/// Once the tick has passed, any later write must land on a different second,
/// so the tag is as good as it is anywhere else.
pub fn validator_of(meta: &std::fs::Metadata, granularity: Granularity) -> Validator {
    let value = etag_of(meta);
    let weak = granularity.0 > Duration::from_nanos(1)
        && meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_none_or(|age| age < granularity.0);
    Validator { value, weak }
}

/// The tag for a file, from what a `stat` already told us.
pub fn etag_of(meta: &std::fs::Metadata) -> String {
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", meta.len())
}

fn modified_of(meta: &std::fs::Metadata) -> Option<DateTime<Local>> {
    meta.modified().ok().map(DateTime::<Local>::from)
}

/// Sort key: directories first, then by name, case-insensitively.
///
/// The order is the server's because the cursor is a position in it. A client
/// that re-sorted a paginated listing would show a page boundary that makes no
/// sense — and a 5000-entry cap means most directories arrive in one page and
/// the question never comes up.
fn sort_key(name: &str, kind: EntryKind) -> (u8, String) {
    let rank = if kind == EntryKind::Dir { 0 } else { 1 };
    (rank, name.to_lowercase())
}

fn encode_cursor(key: &(u8, String)) -> String {
    format!("{}\u{1f}{}", key.0, key.1)
}

fn decode_cursor(cursor: &str) -> Result<(u8, String), FileError> {
    let (rank, name) = cursor
        .split_once('\u{1f}')
        .ok_or_else(|| FileError::BadRequest("that listing cursor is not one of ours".into()))?;
    let rank: u8 = rank
        .parse()
        .map_err(|_| FileError::BadRequest("that listing cursor is not one of ours".into()))?;
    Ok((rank, name.to_string()))
}

/// Read one directory, sorted and paginated.
pub fn list_dir(
    root: &RootInfo,
    rel: &str,
    dir: &Path,
    denied: &[PathBuf],
    limit: usize,
    cursor: Option<&str>,
) -> Result<Listing, FileError> {
    let meta = std::fs::metadata(dir).map_err(|e| FileError::from_io(&e, "that folder"))?;
    if !meta.is_dir() {
        return Err(FileError::BadRequest("that is a file, not a folder".into()));
    }

    let after = cursor.map(decode_cursor).transpose()?;
    let entries = std::fs::read_dir(dir).map_err(|e| FileError::from_io(&e, "that folder"))?;

    let mut rows: Vec<((u8, String), DirEntryInfo)> = Vec::new();
    let mut unreadable = 0usize;
    for entry in entries {
        // Counted rather than skipped. There is no name here to build a row
        // out of, so this is the one thing that cannot become a row — and the
        // count is what keeps it from silently shortening the list instead.
        let Ok(entry) = entry else {
            unreadable += 1;
            continue;
        };
        let raw = entry.file_name();
        let (name, utf8) = match raw.to_str() {
            Some(s) => (s.to_string(), true),
            None => (raw.to_string_lossy().into_owned(), false),
        };
        // `symlink_metadata`, so a link is reported as a link rather than as
        // whatever it points at.
        //
        // `None` when even that fails, which this used to treat as a reason to
        // drop the entry. It is not: the directory just said the entry is
        // there. A FAT drive whose charset cannot spell a stored name hands
        // back a rendering that cannot be looked up again, and another writer
        // can unlink something between the read and this line — in both cases
        // the honest answer is a row saying so, not a list that is quietly
        // one shorter than the folder.
        let link_meta = entry.metadata().ok();
        let symlink = link_meta
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink());
        let path = entry.path();
        // What it *is* — for a link, what it points at, which is what a person
        // browsing wants to see.
        let target_meta = if symlink {
            std::fs::metadata(&path).ok()
        } else {
            link_meta.clone()
        };
        let kind = match &target_meta {
            Some(m) if m.is_dir() => EntryKind::Dir,
            Some(m) if m.is_file() => EntryKind::File,
            Some(_) => EntryKind::Other,
            None => EntryKind::Other,
        };
        // Ordered by how much they take away: a name that cannot be addressed
        // leaves nothing possible, while an escaping link can still be
        // deleted. The first match is what the client is told.
        let unusable = if !utf8 {
            Some(UnusableReason::NameNotUtf8)
        } else if resolve::check_denied(&path, denied).is_err() {
            Some(UnusableReason::NotBrowsable)
        } else if link_meta.is_none() {
            // Before the `SpecialFile` arm below, which would otherwise claim
            // this is a socket or a device — `kind` is `Other` here only
            // because nothing could be learned, not because anything was.
            Some(UnusableReason::Unreadable)
        } else if symlink && !resolve::link_stays_inside(&path, &root.canonical, denied) {
            Some(UnusableReason::SymlinkEscapes)
        } else if kind == EntryKind::Other {
            Some(UnusableReason::SpecialFile)
        } else {
            None
        };
        let info = DirEntryInfo {
            hidden: name.starts_with('.'),
            kind,
            size: target_meta
                .as_ref()
                .filter(|m| m.is_file())
                .map(|m| m.len()),
            modified: target_meta.as_ref().and_then(modified_of),
            etag: target_meta.as_ref().filter(|m| m.is_file()).map(etag_of),
            symlink,
            // Asked only of a directory this API would actually descend into:
            // an `access` on every row of a ROM directory is cheap, and one on
            // a link pointing out of the root is a question about somewhere
            // this API will not go.
            writable: (kind == EntryKind::Dir && unusable.is_none())
                .then(|| roots::writable(&path)),
            unusable,
            name,
        };
        let key = sort_key(&info.name, info.kind);
        if let Some(after) = &after
            && key <= *after
        {
            continue;
        }
        rows.push((key, info));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let cursor = truncated.then(|| rows.last().map(|(key, _)| encode_cursor(key)));

    Ok(Listing {
        root: root.id.clone(),
        path: rel.to_string(),
        writable: roots::writable(dir),
        entries: rows.into_iter().map(|(_, info)| info).collect(),
        truncated,
        cursor: cursor.flatten(),
        unreadable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Error;

    /// Built by hand rather than by touching a filesystem: what matters is the
    /// arithmetic between the tick and the file's age, and there is no FAT
    /// drive in a unit test.
    fn meta_of(path: &Path) -> std::fs::Metadata {
        std::fs::metadata(path).unwrap()
    }

    #[test]
    fn a_fresh_file_on_a_coarse_filesystem_is_only_weakly_tagged() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("v.bin");
        std::fs::write(&file, b"AAAA").unwrap();
        let meta = meta_of(&file);

        // On an ordinary disk a second write always moves the tag, so it is
        // worth the caller's trust.
        assert!(!validator_of(&meta, Granularity::FINE).weak);

        // On FAT it is not: another write in the same two-second tick would
        // leave size and mtime alone, and the tag with them.
        let weak = validator_of(&meta, Granularity::COARSE);
        assert!(weak.weak);
        assert!(weak.header().starts_with("W/"), "{}", weak.header());
    }

    #[test]
    fn once_the_tick_has_passed_a_coarse_tag_is_as_good_as_any() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("v.bin");
        std::fs::write(&file, b"AAAA").unwrap();
        // Ten seconds old: no later write can land on the same second, so the
        // tag is once again a promise.
        let ten_seconds_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
        filetime::set_file_mtime(&file, filetime::FileTime::from_system_time(ten_seconds_ago))
            .unwrap();
        let validator = validator_of(&meta_of(&file), Granularity::COARSE);
        assert!(!validator.weak);
        assert!(validator.header().starts_with('"'));
    }

    #[test]
    fn a_weak_validator_satisfies_nothing() {
        let weak = Validator {
            value: "8-1789437996000000000".into(),
            weak: true,
        };
        // Neither spelling of the same value: `If-Match` and range assembly
        // both compare strongly, and that is the entire protection.
        assert!(!weak.strongly_matches("\"8-1789437996000000000\""));
        assert!(!weak.strongly_matches("W/\"8-1789437996000000000\""));

        let strong = Validator {
            value: "8-1789437996000000000".into(),
            weak: false,
        };
        assert!(strong.strongly_matches("\"8-1789437996000000000\""));
        // A caller's *weak* tag never matches either, whatever ours is.
        assert!(!strong.strongly_matches("W/\"8-1789437996000000000\""));
        assert!(!strong.strongly_matches("\"8-1\""));
    }

    /// The errnos a removable drive answers with. Each of these reached a
    /// caller as `500 internal` until a FAT stick was tested against.
    #[test]
    fn a_drives_refusals_are_answers_rather_than_faults() {
        let cases = [
            (22, "not one this drive can store"), // EINVAL: a name FAT rejects
            (27, "larger than this drive"),       // EFBIG: past FAT32's 4 GiB
            (30, "read-only"),                    // EROFS
            (36, "too long"),                     // ENAMETOOLONG
        ];
        for (errno, needle) in cases {
            let error = FileError::from_io(&Error::from_raw_os_error(errno), "that file");
            let (status, code, message) = error.parts();
            assert_ne!(
                status,
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "errno {errno} still reads as a fault in the device: {message}"
            );
            assert!(
                message.contains(needle),
                "errno {errno} ({code}) says {message:?}, which does not mention {needle:?}"
            );
        }
        // And the one that was already right.
        let full = FileError::from_io(&Error::from_raw_os_error(28), "that file");
        assert_eq!(full.parts().1, "insufficient_storage");
    }
}
