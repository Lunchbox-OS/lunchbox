//! Icon autodetection for entry kinds

use shepherd_api::{EntryKind, MediaMode};
use std::path::{Path, PathBuf};
use tracing::debug;

/// Try to autodetect an icon for an entry based on its kind.
/// Returns an icon reference (theme name or absolute path), or None if not detected.
pub(crate) fn autodetect_icon(kind: &EntryKind) -> Option<String> {
    match kind {
        EntryKind::Flatpak { app_id, .. } => {
            // Flatpak exports icons using the app_id as the icon theme name
            Some(app_id.clone())
        }
        EntryKind::Snap { snap_name, .. } => {
            if let Some(icon) = find_snap_icon(snap_name) {
                debug!(snap_name, icon, "snap icon autodetected");
                Some(icon)
            } else {
                // Snaps often install their icon under the snap name
                Some(snap_name.clone())
            }
        }
        EntryKind::Steam { app_id, .. } => {
            let icon = find_steam_icon(*app_id).unwrap_or_else(|| format!("steam_icon_{}", app_id));
            debug!(app_id, icon, "steam icon autodetected");
            Some(icon)
        }
        EntryKind::Process { command, .. } => {
            let icon = find_process_icon(command);
            if let Some(ref i) = icon {
                debug!(command, icon = i, "process icon autodetected");
            }
            icon
        }
        // A media activity has no on-disk app to look up, but its mode says
        // which of the two stock theme icons fits: a collection to browse, or
        // one video to play.
        EntryKind::Media { mode, .. } => Some(
            match mode {
                MediaMode::Browse => "folder-videos",
                MediaMode::Play => "video-x-generic",
            }
            .to_string(),
        ),
        EntryKind::Retroarch { command, .. } => {
            // RetroArch ships a desktop file, so the same lookup the process
            // kind uses finds its icon. Per-game artwork stays an explicit
            // `icon = ` on the entry — there is nothing to autodetect from a
            // ROM path.
            let icon = find_process_icon(command);
            if let Some(ref i) = icon {
                debug!(command, icon = i, "retroarch icon autodetected");
            }
            icon
        }
        EntryKind::Vm { .. } | EntryKind::Custom { .. } => None,
    }
}

/// Read the `Icon=` value from the `[Desktop Entry]` section of a .desktop file.
fn read_desktop_icon(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[Desktop Entry]" {
            in_entry = true;
            continue;
        }
        if in_entry {
            if trimmed.starts_with('[') {
                break; // entered a new section
            }
            if let Some(value) = trimmed.strip_prefix("Icon=") {
                let icon = value.trim().to_string();
                if !icon.is_empty() {
                    return Some(icon);
                }
            }
        }
    }
    None
}

/// Search `/var/lib/snapd/desktop/applications/` for a .desktop file matching the snap name.
fn find_snap_icon(snap_name: &str) -> Option<String> {
    let snap_apps_dir = Path::new("/var/lib/snapd/desktop/applications");
    if !snap_apps_dir.is_dir() {
        return None;
    }
    let prefix = format!("{}_", snap_name);
    for entry in std::fs::read_dir(snap_apps_dir).ok()?.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.starts_with(&prefix)
            && name.ends_with(".desktop")
            && let Some(icon) = read_desktop_icon(&entry.path())
        {
            return Some(icon);
        }
    }
    None
}

/// Find the icon for a Steam app from its generated .desktop file.
fn find_steam_icon(app_id: u32) -> Option<String> {
    let apps_dir = home_dir()?.join(".local/share/applications");
    read_desktop_icon(&apps_dir.join(format!("steam_{}.desktop", app_id)))
}

/// Search XDG application dirs for a .desktop file whose `Exec=` basename matches the command.
fn find_process_icon(command: &str) -> Option<String> {
    let cmd_basename = Path::new(command)
        .file_name()?
        .to_string_lossy()
        .to_string();
    for dir in xdg_application_dirs() {
        if let Some(icon) = search_dir_for_command(&dir, &cmd_basename) {
            return Some(icon);
        }
    }
    None
}

/// XDG application directories in priority order.
fn xdg_application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // $XDG_DATA_HOME/applications (default: ~/.local/share/applications)
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".local/share")));
    if let Some(d) = data_home {
        dirs.push(d.join("applications"));
    }

    // $XDG_DATA_DIRS/applications (default: /usr/local/share:/usr/share)
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for part in data_dirs.split(':') {
        dirs.push(PathBuf::from(part).join("applications"));
    }

    dirs
}

/// Search a single directory for a .desktop file with a matching `Exec=` command basename.
fn search_dir_for_command(dir: &Path, cmd_basename: &str) -> Option<String> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        if let Some(icon) = match_desktop_file_command(&path, cmd_basename) {
            return Some(icon);
        }
    }
    None
}

/// Return the `Icon=` value from a .desktop file if its `Exec=` basename matches `cmd_basename`.
fn match_desktop_file_command(path: &Path, cmd_basename: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut exec_matches = false;
    let mut icon: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[Desktop Entry]" {
            in_entry = true;
            continue;
        }
        if in_entry {
            if trimmed.starts_with('[') {
                break;
            }
            if let Some(exec_value) = trimmed.strip_prefix("Exec=") {
                let exec_cmd = exec_value.split_ascii_whitespace().next().unwrap_or("");
                let exec_base = Path::new(exec_cmd)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(exec_cmd);
                if exec_base == cmd_basename {
                    exec_matches = true;
                }
            }
            if let Some(value) = trimmed.strip_prefix("Icon=") {
                let v = value.trim().to_string();
                if !v.is_empty() {
                    icon = Some(v);
                }
            }
        }
    }

    if exec_matches { icon } else { None }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_desktop_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", content).unwrap();
        file
    }

    fn write_desktop_file_in(dir: &std::path::Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn read_desktop_icon_finds_icon() {
        let file = write_desktop_file(
            "[Desktop Entry]\nName=Test\nExec=test %U\nIcon=test-app\nType=Application\n",
        );
        assert_eq!(read_desktop_icon(file.path()), Some("test-app".to_string()));
    }

    #[test]
    fn read_desktop_icon_stops_at_next_section() {
        let file =
            write_desktop_file("[Desktop Entry]\nName=Test\n[Other]\nIcon=should-not-find\n");
        assert_eq!(read_desktop_icon(file.path()), None);
    }

    #[test]
    fn read_desktop_icon_requires_desktop_entry_section() {
        let file = write_desktop_file("[Other]\nIcon=should-not-find\n");
        assert_eq!(read_desktop_icon(file.path()), None);
    }

    #[test]
    fn match_desktop_file_command_matches_basename() {
        let file = write_desktop_file(
            "[Desktop Entry]\nName=MPV\nExec=mpv %U\nIcon=mpv\nType=Application\n",
        );
        assert_eq!(
            match_desktop_file_command(file.path(), "mpv"),
            Some("mpv".to_string())
        );
    }

    #[test]
    fn match_desktop_file_command_matches_full_path_exec() {
        let file = write_desktop_file(
            "[Desktop Entry]\nName=MPV\nExec=/usr/bin/mpv %U\nIcon=mpv\nType=Application\n",
        );
        assert_eq!(
            match_desktop_file_command(file.path(), "mpv"),
            Some("mpv".to_string())
        );
    }

    #[test]
    fn match_desktop_file_command_no_match() {
        let file = write_desktop_file(
            "[Desktop Entry]\nName=Other\nExec=other %U\nIcon=other\nType=Application\n",
        );
        assert_eq!(match_desktop_file_command(file.path(), "mpv"), None);
    }

    #[test]
    fn flatpak_autodetect_returns_app_id() {
        let kind = EntryKind::Flatpak {
            app_id: "org.kde.krita".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert_eq!(autodetect_icon(&kind), Some("org.kde.krita".to_string()));
    }

    #[test]
    fn vm_autodetect_returns_none() {
        let kind = EntryKind::Vm {
            driver: "qemu".to_string(),
            args: Default::default(),
        };
        assert_eq!(autodetect_icon(&kind), None);
    }

    fn media_kind(mode: MediaMode) -> EntryKind {
        EntryKind::Media {
            library: "/etc/shepherd/movies.toml".to_string(),
            mode,
            item: None,
            quality: Default::default(),
            sort_by: Default::default(),
            reverse: false,
            resume: false,
            prefetch: None,
            sponsorblock: None,
        }
    }

    #[test]
    fn media_autodetect_follows_mode() {
        assert_eq!(
            autodetect_icon(&media_kind(MediaMode::Browse)).as_deref(),
            Some("folder-videos")
        );
        assert_eq!(
            autodetect_icon(&media_kind(MediaMode::Play)).as_deref(),
            Some("video-x-generic")
        );
    }

    #[test]
    fn search_dir_finds_icon_by_command() {
        let dir = tempfile::tempdir().unwrap();
        write_desktop_file_in(
            dir.path(),
            "myapp.desktop",
            "[Desktop Entry]\nName=MyApp\nExec=myapp %U\nIcon=myapp-icon\nType=Application\n",
        );
        assert_eq!(
            search_dir_for_command(dir.path(), "myapp"),
            Some("myapp-icon".to_string())
        );
    }

    #[test]
    fn search_dir_finds_icon_by_full_path_exec() {
        let dir = tempfile::tempdir().unwrap();
        write_desktop_file_in(
            dir.path(),
            "myapp.desktop",
            "[Desktop Entry]\nName=MyApp\nExec=/usr/bin/myapp %U\nIcon=myapp-icon\nType=Application\n",
        );
        assert_eq!(
            search_dir_for_command(dir.path(), "myapp"),
            Some("myapp-icon".to_string())
        );
    }

    #[test]
    fn search_dir_returns_none_for_wrong_command() {
        let dir = tempfile::tempdir().unwrap();
        write_desktop_file_in(
            dir.path(),
            "otherapp.desktop",
            "[Desktop Entry]\nName=Other\nExec=otherapp %U\nIcon=other-icon\nType=Application\n",
        );
        assert_eq!(search_dir_for_command(dir.path(), "myapp"), None);
    }

    #[test]
    fn search_dir_skips_non_desktop_files() {
        let dir = tempfile::tempdir().unwrap();
        write_desktop_file_in(
            dir.path(),
            "myapp.txt",
            "[Desktop Entry]\nName=MyApp\nExec=myapp %U\nIcon=myapp-icon\n",
        );
        assert_eq!(search_dir_for_command(dir.path(), "myapp"), None);
    }

    #[test]
    fn search_dir_empty_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(search_dir_for_command(dir.path(), "myapp"), None);
    }

    #[test]
    fn find_steam_icon_reads_desktop_file() {
        let home = tempfile::tempdir().unwrap();
        let apps_dir = home.path().join(".local/share/applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        std::fs::write(
            apps_dir.join("steam_12345.desktop"),
            "[Desktop Entry]\nName=My Game\nExec=steam steam://rungameid/12345\nIcon=steam_icon_12345\nType=Application\n",
        )
        .unwrap();
        // Override HOME so home_dir() points to our temp dir
        unsafe { std::env::set_var("HOME", home.path()) };
        let result = find_steam_icon(12345);
        unsafe { std::env::remove_var("HOME") };
        assert_eq!(result, Some("steam_icon_12345".to_string()));
    }
}
