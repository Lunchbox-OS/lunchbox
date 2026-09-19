//! Turning a caller-supplied path into one this device will actually open
//! (issue #195).
//!
//! Every file route goes through [`resolve`] and nothing else touches the
//! string the caller sent. That is the whole design: `ProtectedFile` is an
//! enum rather than a path precisely because "a request that carried a *name*
//! would need validating against an allow-list on every call — a check that
//! can be got wrong once and then serves arbitrary files out of a directory
//! whose whole point is that nothing else can read it"
//! (`lunchbox_util::ProtectedFile`). A file manager cannot take that advice —
//! naming files is what it is for — so the next best thing is one function,
//! called from everywhere, tested against the escapes.
//!
//! ## What it defends, and what it does not
//!
//! It defends the **remote** caller's reach. It is not a boundary against the
//! kiosk user: every activity runs at lunchboxd's own uid and can read and
//! write these files directly, without asking an HTTP server for permission.
//! What that uid *can* do is plant a symlink, which is why an escaping link is
//! refused here rather than trusted to be somebody else's problem.

use std::path::{Path, PathBuf};

use super::FileError;

/// Longest single path component, as Linux defines it. Checked here so an
/// over-long name is a `400` naming the problem rather than an `ENAMETOOLONG`
/// surfacing as a `500`.
const NAME_MAX: usize = 255;

/// Split a caller's relative path into components, refusing anything that is
/// not a plain name.
///
/// `..` is **refused, not normalised**. A rewrite is a place to be wrong, and
/// a caller with a legitimate reason to send `..` does not exist: the client
/// builds paths by appending names it was given by a listing.
pub fn components(rel: &str) -> Result<Vec<&str>, FileError> {
    let mut out = Vec::new();
    for part in rel.split('/') {
        if part.is_empty() {
            // Tolerated only as leading/trailing slop ("Books/" or "/Books"),
            // which a UI joining strings produces constantly. An empty segment
            // in the middle is the same thing and equally harmless once it is
            // dropped rather than interpreted.
            continue;
        }
        if part == "." || part == ".." {
            return Err(FileError::BadRequest(format!(
                "'{part}' is not a name this API accepts in a path"
            )));
        }
        if part.contains('\0') {
            return Err(FileError::BadRequest(
                "a path component contains a NUL byte".into(),
            ));
        }
        if part.len() > NAME_MAX {
            return Err(FileError::BadRequest(format!(
                "a path component is longer than {NAME_MAX} bytes"
            )));
        }
        out.push(part);
    }
    Ok(out)
}

/// The absolute path `rel` names inside `root`, or an error.
///
/// `root` must already be canonical — [`super::FileService`] canonicalises
/// each root once, when it enumerates them, so this does not pay for it per
/// request.
///
/// The target itself is **not** required to exist: an upload names a file that
/// is about to. What must exist is its parent, and it is the parent that gets
/// canonicalised — which is what catches a symlinked directory component
/// pointing out of the root. The final component is left alone for the caller
/// to open with `O_NOFOLLOW` (writes) or to `symlink_metadata` (reads), since
/// resolving it here would be the check that a swapped link defeats.
pub fn resolve(root: &Path, rel: &str, denied: &[PathBuf]) -> Result<PathBuf, FileError> {
    let parts = components(rel)?;
    let Some((last, parents)) = parts.split_last() else {
        // The root itself.
        return Ok(root.to_path_buf());
    };

    let mut parent = root.to_path_buf();
    for part in parents {
        parent.push(part);
    }
    let parent = std::fs::canonicalize(&parent).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            FileError::NotFound("that folder is not on this device".into())
        }
        std::io::ErrorKind::PermissionDenied => {
            FileError::Forbidden("that folder cannot be read by this device's kiosk user".into())
        }
        _ => FileError::Internal(format!("could not resolve that folder: {e}")),
    })?;

    // After canonicalisation, so a symlinked component that leaves the root is
    // caught here whatever it was spelled as.
    if !parent.starts_with(root) {
        return Err(FileError::Forbidden(
            "that path leads outside the folder it was asked for".into(),
        ));
    }

    let path = parent.join(last);
    check_denied(&path, denied)?;
    Ok(path)
}

/// Refuse a path inside one of Lunchbox's own directories.
///
/// Not about the remote caller's privilege — they can already rewrite the
/// policy, which is strictly more power than reading the database. It is about
/// the two directories where a well-meaning edit does damage nobody will
/// connect to the edit: the video cache keeps an index that hand-deletion
/// desynchronises, and the data directory holds a SQLite database that a
/// partial upload corrupts.
pub fn check_denied(path: &Path, denied: &[PathBuf]) -> Result<(), FileError> {
    if denied.iter().any(|d| path.starts_with(d)) {
        return Err(FileError::Forbidden(
            "that folder belongs to Lunchbox itself and is not editable here".into(),
        ));
    }
    Ok(())
}

/// Whether `path` — a symlink — points somewhere still inside `root`.
///
/// A link that escapes is listed (so a person can see it and delete it) and
/// refused for everything else, which is what `usable: false` means on the
/// wire.
pub fn link_stays_inside(path: &Path, root: &Path, denied: &[PathBuf]) -> bool {
    match std::fs::canonicalize(path) {
        Ok(target) => target.starts_with(root) && check_denied(&target, denied).is_ok(),
        // A dangling link resolves to nothing, so it leads nowhere worth
        // following either.
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    /// A root with `Books/covers`, a symlink out of it, and a symlink back
    /// into it.
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home");
        fs::create_dir_all(root.join("Books/covers")).unwrap();
        fs::write(root.join("Books/hobbit.epub"), b"book").unwrap();
        fs::create_dir_all(dir.path().join("outside")).unwrap();
        fs::write(dir.path().join("outside/secret"), b"secret").unwrap();
        symlink(dir.path().join("outside"), root.join("escape")).unwrap();
        symlink(root.join("Books"), root.join("shortcut")).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        (dir, root)
    }

    #[test]
    fn plain_paths_resolve() {
        let (_d, root) = fixture();
        let p = resolve(&root, "Books/hobbit.epub", &[]).unwrap();
        assert_eq!(p, root.join("Books/hobbit.epub"));
    }

    #[test]
    fn the_empty_path_is_the_root() {
        let (_d, root) = fixture();
        assert_eq!(resolve(&root, "", &[]).unwrap(), root);
        assert_eq!(resolve(&root, "/", &[]).unwrap(), root);
    }

    #[test]
    fn a_target_that_does_not_exist_yet_resolves() {
        let (_d, root) = fixture();
        let p = resolve(&root, "Books/new.epub", &[]).unwrap();
        assert_eq!(p, root.join("Books/new.epub"));
    }

    /// The table this module exists for. Every one of these used to be a way
    /// out of a directory somewhere.
    #[test]
    fn escapes_are_refused() {
        let (_d, root) = fixture();
        for attempt in [
            "..",
            "../",
            "../outside/secret",
            "Books/../../outside/secret",
            "./../outside",
            "Books/./../../outside",
            "escape/secret",
        ] {
            let result = resolve(&root, attempt, &[]);
            assert!(
                result.is_err(),
                "'{attempt}' resolved to {:?}",
                result.unwrap()
            );
        }
    }

    /// An absolute path is not a way out — it is simply not the shape this
    /// API speaks, and the leading empty segment is dropped like any other.
    /// What matters is that it lands *inside* the root rather than at `/`.
    #[test]
    fn an_absolute_looking_path_stays_inside() {
        let (_d, root) = fixture();
        let p = resolve(&root, "/Books/hobbit.epub", &[]).unwrap();
        assert_eq!(p, root.join("Books/hobbit.epub"));
    }

    #[test]
    fn a_symlink_that_stays_inside_is_followed() {
        let (_d, root) = fixture();
        let p = resolve(&root, "shortcut/hobbit.epub", &[]).unwrap();
        assert_eq!(p, root.join("Books/hobbit.epub"));
    }

    /// A link as the *final* component is deliberately not `resolve`'s
    /// business — it does not follow the last one, so that a link swapped in
    /// after the check cannot be what the check looked at. The caller asks
    /// [`link_stays_inside`] instead, and that is what these assert.
    #[test]
    fn an_escaping_symlink_is_not_usable() {
        let (_d, root) = fixture();
        assert!(!link_stays_inside(&root.join("escape"), &root, &[]));
        assert!(link_stays_inside(&root.join("shortcut"), &root, &[]));
    }

    #[test]
    fn a_dangling_symlink_is_not_usable() {
        let (_d, root) = fixture();
        symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        assert!(!link_stays_inside(&root.join("dangling"), &root, &[]));
    }

    #[test]
    fn nul_and_over_long_components_are_refused() {
        let (_d, root) = fixture();
        assert!(resolve(&root, "Books/a\0b", &[]).is_err());
        assert!(resolve(&root, &format!("Books/{}", "a".repeat(256)), &[]).is_err());
    }

    #[test]
    fn denied_subtrees_are_refused_however_they_are_reached() {
        let (_d, root) = fixture();
        let denied = vec![root.join(".local/share/lunchboxd")];
        fs::create_dir_all(root.join(".local/share/lunchboxd")).unwrap();
        fs::write(root.join(".local/share/lunchboxd/lunchbox.db"), b"db").unwrap();
        assert!(resolve(&root, ".local/share/lunchboxd/lunchbox.db", &denied).is_err());

        // And by a symlink pointing at it from somewhere innocuous, which is
        // the version a check on the *unresolved* path would have missed.
        symlink(root.join(".local/share/lunchboxd"), root.join("state")).unwrap();
        assert!(resolve(&root, "state/lunchbox.db", &denied).is_err());
    }
}
