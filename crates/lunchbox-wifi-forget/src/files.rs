//! Which netplan files to change, and changing them all or none.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::{Edit, Refusal, netdef_id, own_file_name, remove_definition};

/// The directories netplan reads, in the order it reads them. A later file of
/// the same name shadows an earlier one.
const HIERARCHY: [&str; 3] = ["lib/netplan", "etc/netplan", "run/netplan"];

/// Packaged configuration. A definition there is not something a parent's
/// "forget" should reach, and the package would put it back.
const VENDOR: &str = "lib/netplan";

/// Every change a forget will make, worked out before any is made.
#[derive(Debug)]
pub struct Plan {
    changes: Vec<(PathBuf, Original, Edit)>,
}

/// A file as it was, to put back.
#[derive(Debug)]
struct Original {
    text: String,
    mode: u32,
    uid: u32,
    gid: u32,
}

impl Plan {
    /// Read every netplan file under `root` and decide what removing the
    /// profile `uuid` does to each.
    ///
    /// Refuses, naming the file and the reason, rather than plan a change it
    /// cannot make safely. Nothing has been written when this returns.
    pub fn new(root: &Path, uuid: &str) -> Result<Self, String> {
        let id = netdef_id(uuid);
        let own = own_file_name(uuid);
        let mut changes = Vec::new();
        for dir in HIERARCHY {
            for path in yaml_files(&root.join(dir))? {
                let text = fs::read_to_string(&path)
                    .map_err(|e| format!("reading {}: {e}", path.display()))?;
                // Cheap, and keeps the parsers away from files that cannot be
                // involved. A definition cannot name the id without its UUID.
                if !text.contains(uuid) {
                    continue;
                }
                let refuse = |why: &dyn std::fmt::Display| {
                    format!("{} mentions {id}, and {why}", path.display())
                };
                if dir == VENDOR {
                    return Err(refuse(&"it is packaged configuration"));
                }
                let meta = fs::symlink_metadata(&path)
                    .map_err(|e| format!("reading {}: {e}", path.display()))?;
                if meta.file_type().is_symlink() {
                    return Err(refuse(&"it is a symlink, which a rewrite would replace"));
                }
                let own_file = path.file_name().is_some_and(|name| *name == *own);
                match remove_definition(&text, &id, own_file) {
                    Ok(Edit::Absent) => {}
                    Ok(edit) => changes.push((
                        path,
                        Original {
                            text,
                            mode: meta.permissions().mode() & 0o7777,
                            uid: meta.uid(),
                            gid: meta.gid(),
                        },
                        edit,
                    )),
                    Err(refusal) => return Err(refuse(&Why(refusal))),
                }
            }
        }
        Ok(Self { changes })
    }

    /// The files this plan changes, and how, for the log.
    pub fn describe(&self) -> Vec<String> {
        self.changes
            .iter()
            .map(|(path, _, edit)| match edit {
                Edit::Unlink => format!("remove {}", path.display()),
                _ => format!("edit {}", path.display()),
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Make every change. On the first failure, put back what was already
    /// changed and return the failure.
    pub fn apply(&self) -> io::Result<()> {
        for (done, (path, original, edit)) in self.changes.iter().enumerate() {
            let result = match edit {
                Edit::Rewrite(text) => replace(path, text, original),
                Edit::Unlink => fs::remove_file(path),
                Edit::Absent => Ok(()),
            };
            if let Err(e) = result {
                self.restore_first(done);
                return Err(io::Error::new(
                    e.kind(),
                    format!("changing {}: {e}", path.display()),
                ));
            }
        }
        Ok(())
    }

    /// Put every file back as it was before [`Self::apply`].
    pub fn restore(&self) -> io::Result<()> {
        let mut first_error = None;
        for (path, original, _) in &self.changes {
            if let Err(e) = replace(path, &original.text, original) {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn restore_first(&self, n: usize) {
        for (path, original, _) in &self.changes[..n] {
            let _ = replace(path, &original.text, original);
        }
    }
}

struct Why(Refusal);

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — nothing was changed", self.0)
    }
}

/// The `*.yaml` files in `dir`, sorted as netplan sorts them. A missing
/// directory has none.
fn yaml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("listing {}: {e}", dir.display())),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    files.sort();
    Ok(files)
}

/// Write `text` to `path` so that a reader sees the old file or the new one,
/// never half of either, with the old file's owner and mode.
///
/// netplan warns about, and NetworkManager's own files rely on, `0600 root`: a
/// passphrase is stored in plain text in these files.
fn replace(path: &Path, text: &str, like: &Original) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = dir.join(format!(".{name}.lunchbox-wifi-forget"));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        std::os::unix::fs::fchown(&file, Some(like.uid), Some(like.gid))?;
        file.set_permissions(fs::Permissions::from_mode(like.mode))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "11111111-2222-3333-4444-555555555555";

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        root
    }

    fn read(root: &tempfile::TempDir, path: &str) -> Option<String> {
        fs::read_to_string(root.path().join(path)).ok()
    }

    const INSTALLER: &str = "# This is the network config written by 'subiquity'\n\
                             network:\n  ethernets:\n    enp1s0:\n      dhcp4: true\n  version: 2\n";
    const RENDERER: &str = "# Let NetworkManager manage all devices on this system\n\
                            network:\n  version: 2\n  renderer: NetworkManager\n";
    const OWN: &str = "network:\n  version: 2\n  wifis:\n    \
                       NM-11111111-2222-3333-4444-555555555555:\n      dhcp4: true\n";

    #[test]
    fn a_forget_touches_the_profiles_own_file_and_nothing_else() {
        let root = tree(&[
            ("etc/netplan/00-installer-config.yaml", INSTALLER),
            ("etc/netplan/01-network-manager-all.yaml", RENDERER),
            (
                "etc/netplan/90-NM-11111111-2222-3333-4444-555555555555.yaml",
                OWN,
            ),
        ]);
        let plan = Plan::new(root.path(), UUID).unwrap();
        plan.apply().unwrap();

        assert_eq!(
            read(&root, "etc/netplan/00-installer-config.yaml").as_deref(),
            Some(INSTALLER)
        );
        assert_eq!(
            read(&root, "etc/netplan/01-network-manager-all.yaml").as_deref(),
            Some(RENDERER)
        );
        assert_eq!(
            read(
                &root,
                "etc/netplan/90-NM-11111111-2222-3333-4444-555555555555.yaml"
            ),
            None
        );
    }

    #[test]
    fn a_definition_in_a_hand_written_file_is_cut_out_of_it() {
        let set = "# overrides\nnetwork:\n  wifis:\n    \
                   NM-11111111-2222-3333-4444-555555555555:\n      dhcp6: false\n    \
                   other:\n      dhcp4: true\n";
        let root = tree(&[
            ("etc/netplan/70-netplan-set.yaml", set),
            (
                "etc/netplan/90-NM-11111111-2222-3333-4444-555555555555.yaml",
                OWN,
            ),
        ]);
        let plan = Plan::new(root.path(), UUID).unwrap();
        plan.apply().unwrap();
        assert_eq!(
            read(&root, "etc/netplan/70-netplan-set.yaml").as_deref(),
            Some("# overrides\nnetwork:\n  wifis:\n    other:\n      dhcp4: true\n")
        );
        let mode = fs::metadata(root.path().join("etc/netplan/70-netplan-set.yaml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a file holding passphrases stays private"
        );
    }

    #[test]
    fn one_refusal_changes_nothing_anywhere() {
        let root = tree(&[
            (
                "etc/netplan/90-NM-11111111-2222-3333-4444-555555555555.yaml",
                OWN,
            ),
            (
                "run/netplan/50-broken.yaml",
                "network:\n  wifis:\n    NM-11111111-2222-3333-4444-555555555555: [\n",
            ),
        ]);
        let err = Plan::new(root.path(), UUID).unwrap_err();
        assert!(err.contains("50-broken.yaml"), "{err}");
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(
            read(
                &root,
                "etc/netplan/90-NM-11111111-2222-3333-4444-555555555555.yaml"
            )
            .as_deref(),
            Some(OWN)
        );
    }

    #[test]
    fn packaged_configuration_is_refused() {
        let root = tree(&[("lib/netplan/10-vendor.yaml", OWN)]);
        let err = Plan::new(root.path(), UUID).unwrap_err();
        assert!(err.contains("packaged"), "{err}");
    }

    #[test]
    fn a_symlinked_file_is_refused() {
        let root = tree(&[("elsewhere/real.yaml", OWN)]);
        fs::create_dir_all(root.path().join("etc/netplan")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("elsewhere/real.yaml"),
            root.path().join("etc/netplan/90-linked.yaml"),
        )
        .unwrap();
        let err = Plan::new(root.path(), UUID).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
    }

    #[test]
    fn restore_puts_every_file_back() {
        let set = "# overrides\nnetwork:\n  wifis:\n    \
                   NM-11111111-2222-3333-4444-555555555555: {}\n    other: {}\n";
        let own = "etc/netplan/90-NM-11111111-2222-3333-4444-555555555555.yaml";
        let root = tree(&[("etc/netplan/70-netplan-set.yaml", set), (own, OWN)]);
        let plan = Plan::new(root.path(), UUID).unwrap();
        plan.apply().unwrap();
        plan.restore().unwrap();
        assert_eq!(
            read(&root, "etc/netplan/70-netplan-set.yaml").as_deref(),
            Some(set)
        );
        assert_eq!(read(&root, own).as_deref(), Some(OWN));
    }

    #[test]
    fn a_profile_netplan_never_heard_of_plans_nothing() {
        let root = tree(&[("etc/netplan/00-installer-config.yaml", INSTALLER)]);
        assert!(Plan::new(root.path(), UUID).unwrap().is_empty());
    }
}
