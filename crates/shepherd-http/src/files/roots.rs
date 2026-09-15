//! The places the file manager may browse (issue #195).
//!
//! Three kinds, enumerated fresh on every `GET /api/v1/files/roots`: the kiosk
//! user's home, whatever removable media is mounted right now, and whatever
//! `[[service.file_manager.extra_roots]]` names.
//!
//! Re-enumerated per request rather than cached, because the interesting one
//! is dynamic: a drive plugged in while the page is open should appear on the
//! next refresh, and the whole cost is one `/proc/mounts` read plus a
//! `statvfs` per root. A mount watcher would buy nothing a refresh does not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Where a root came from, so the UI can group and icon them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// The kiosk user's home directory. Always present.
    Home,
    /// A removable drive mounted under `/media` or `/run/media`.
    External,
    /// A directory named in `config.toml`.
    Configured,
}

/// One browsable place, as the API reports it.
#[derive(Debug, Clone, Serialize)]
pub struct RootInfo {
    /// Opaque handle. Every other route takes this and never an absolute path.
    pub id: String,
    /// What to call it on screen.
    pub label: String,
    pub kind: RootKind,
    /// The absolute path, for display only. Nothing accepts it back.
    pub path: PathBuf,
    /// Whether this device's kiosk user may write here. A drive mounted
    /// read-only, or owned by root, reads `false` and the UI hides its upload
    /// and delete controls rather than offering buttons that will 403.
    pub writable: bool,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// The canonical path, kept out of the wire format: it is what
    /// [`super::resolve`] measures every request against, and canonicalising
    /// it once here is why that function does not pay for it per call.
    #[serde(skip)]
    pub canonical: PathBuf,
    /// How precisely this filesystem records modification times, which decides
    /// whether its etags can be trusted byte-for-byte. Asked once per root
    /// rather than once per file.
    #[serde(skip)]
    pub granularity: super::Granularity,
}

/// Build the list. `home` is shepherdd's own home directory.
pub fn enumerate(home: &Path, settings: &shepherd_config::FileManagerConfig) -> Vec<RootInfo> {
    let mut roots = Vec::new();

    if let Some(root) = describe("home".to_string(), "Home".to_string(), RootKind::Home, home) {
        roots.push(root);
    }

    if settings.external_media {
        roots.extend(removable());
    }

    for (i, extra) in settings.extra_roots.iter().enumerate() {
        // Indexed rather than derived from the label, so renaming a root in
        // `config.toml` does not silently become a different root, and two
        // roots cannot collide on an id if validation is ever relaxed.
        if let Some(root) = describe(
            format!("extra-{i}"),
            extra.label.clone(),
            RootKind::Configured,
            &extra.path,
        ) {
            roots.push(root);
        }
    }

    roots
}

/// Fill in everything that has to be asked of the filesystem.
///
/// `None` when the path is not a directory this device can even look at — a
/// configured root on a disk that is not mounted, most likely. Dropping it
/// from the list is the honest answer: it is not somewhere a file can be put
/// right now.
fn describe(id: String, label: String, kind: RootKind, path: &Path) -> Option<RootInfo> {
    let canonical = std::fs::canonicalize(path).ok()?;
    if !canonical.is_dir() {
        return None;
    }
    let (total_bytes, free_bytes) = match space(&canonical) {
        Some((total, free)) => (Some(total), Some(free)),
        None => (None, None),
    };
    Some(RootInfo {
        id,
        label,
        kind,
        path: canonical.clone(),
        writable: writable(&canonical),
        total_bytes,
        free_bytes,
        granularity: super::Granularity::of(&canonical),
        canonical,
    })
}

/// Effective write access for the uid shepherdd runs as.
///
/// `access(2)` rather than reading the mode bits: a drive mounted read-only,
/// or one owned by root, answers the question a mode comparison would get
/// wrong.
pub fn writable(path: &Path) -> bool {
    nix::unistd::access(path, nix::unistd::AccessFlags::W_OK).is_ok()
}

/// Total and available bytes on the filesystem holding `path`.
///
/// `f_bavail`, not `f_bfree`: the difference is the reserve only root may use,
/// and shepherdd is not root. Deliberately not shared with
/// `shepherdd::media::free_space`, which walks up to an existing ancestor
/// because it is asked about a cache directory that may not have been created
/// yet; every path reaching this one has already been canonicalised and exists.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    let stat = nix::sys::statvfs::statvfs(path).ok()?;
    let unit = stat.fragment_size();
    Some((
        stat.blocks().saturating_mul(unit),
        stat.blocks_available().saturating_mul(unit),
    ))
}

/// Removable drives, from `/proc/mounts`.
///
/// Not from `udisks2` over D-Bus: a sway kiosk commonly has no automounter
/// running at all, so a drive is as likely to have been mounted by `fstab` or
/// by an administrator — and `/proc/mounts` sees all three identically.
fn removable() -> Vec<RootInfo> {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    let uuids = uuid_map();
    let mut out = Vec::new();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(device), Some(mount_point)) = (fields.next(), fields.next()) else {
            continue;
        };
        // A real block device, which is also what makes a UUID meaningful.
        // Skips the tmpfs and gvfs mounts that live alongside real media.
        if !device.starts_with("/dev/") {
            continue;
        }
        let device = unescape(device);
        let mount_point = PathBuf::from(unescape(mount_point));
        if !is_removable_location(&mount_point) {
            continue;
        }
        let label = mount_point
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| mount_point.display().to_string());
        let id = match uuids.get(Path::new(&device)) {
            // The filesystem's own UUID: it survives the drive being
            // unplugged and remounted somewhere else, which neither the mount
            // point nor the label does, and it does not collide when two
            // drives are both labelled `UNTITLED`.
            Some(uuid) => format!("ext-{uuid}"),
            // Rare — a filesystem without a UUID at all. Offered anyway,
            // keyed on the device node, because "your drive is not listed" is
            // a worse answer than an id that does not survive a replug.
            None => format!("ext-dev-{}", device.replace('/', "-")),
        };
        if let Some(root) = describe(id, label, RootKind::External, &mount_point) {
            out.push(root);
        }
    }
    out.sort_by_key(|r| r.label.to_lowercase());
    out
}

/// Whether a mount point is where removable media turns up.
///
/// `udisks2` mounts at `/media/<user>/<label>`; an `fstab` entry or a hand
/// mount is as likely to be `/media/<label>`. Both, and `/run/media` for the
/// distributions that put it there, at any depth below.
fn is_removable_location(mount_point: &Path) -> bool {
    [Path::new("/media"), Path::new("/run/media")]
        .iter()
        .any(|prefix| mount_point.starts_with(prefix) && mount_point != *prefix)
}

/// Device node → filesystem UUID, from `/dev/disk/by-uuid`.
fn uuid_map() -> HashMap<PathBuf, String> {
    let mut map = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/dev/disk/by-uuid") else {
        return map;
    };
    for entry in entries.flatten() {
        let uuid = entry.file_name().to_string_lossy().into_owned();
        // The entries are symlinks into `/dev`; canonicalising them is what
        // matches the `/dev/sdb1` spelling `/proc/mounts` uses.
        if let Ok(target) = std::fs::canonicalize(entry.path()) {
            map.insert(target, uuid);
        }
    }
    map
}

/// Undo the octal escaping `/proc/mounts` applies.
///
/// A USB drive labelled "My Photos" mounts at `/media/kiosk/My\040Photos`, and
/// reading that path literally is a drive that silently never appears.
fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let bytes = field.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(code) = u8::from_str_radix(&field[i + 1..i + 4], 8)
        {
            out.push(code as char);
            i += 4;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_escapes_are_undone() {
        assert_eq!(
            unescape("/media/kiosk/My\\040Photos"),
            "/media/kiosk/My Photos"
        );
        assert_eq!(unescape("/media/kiosk/plain"), "/media/kiosk/plain");
        // A trailing backslash is not an escape and must not be eaten.
        assert_eq!(unescape("/media/odd\\"), "/media/odd\\");
    }

    #[test]
    fn only_media_mount_points_count() {
        assert!(is_removable_location(Path::new("/media/kiosk/KINGSTON")));
        assert!(is_removable_location(Path::new("/media/usb0")));
        assert!(is_removable_location(Path::new(
            "/run/media/kiosk/KINGSTON"
        )));
        assert!(!is_removable_location(Path::new("/media")));
        assert!(!is_removable_location(Path::new("/")));
        assert!(!is_removable_location(Path::new("/home/kiosk")));
        assert!(!is_removable_location(Path::new("/mnt/nas")));
    }

    #[test]
    fn the_home_root_is_described() {
        let dir = tempfile::tempdir().unwrap();
        let settings = shepherd_config::FileManagerConfig::default();
        let roots = enumerate(dir.path(), &settings);
        let home = roots.iter().find(|r| r.id == "home").unwrap();
        assert_eq!(home.kind, RootKind::Home);
        assert!(home.writable);
        assert!(home.total_bytes.unwrap() > 0);
    }

    #[test]
    fn a_configured_root_that_is_not_there_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let settings = shepherd_config::FileManagerConfig {
            external_media: false,
            extra_roots: vec![
                shepherd_config::FileManagerRoot {
                    label: "Gone".into(),
                    path: dir.path().join("not-mounted"),
                },
                shepherd_config::FileManagerRoot {
                    label: "There".into(),
                    path: dir.path().to_path_buf(),
                },
            ],
            ..Default::default()
        };
        let roots = enumerate(dir.path(), &settings);
        let ids: Vec<&str> = roots.iter().map(|r| r.id.as_str()).collect();
        assert!(!ids.contains(&"extra-0"), "an absent root was offered");
        assert!(ids.contains(&"extra-1"));
    }
}
