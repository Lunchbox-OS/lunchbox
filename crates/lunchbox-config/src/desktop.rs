//! Freedesktop Desktop Entry enumeration, for administrator mode's app picker
//! (issue #154).
//!
//! This is the "everything a normal desktop would show you" list: the same
//! `.desktop` files GNOME puts in its overview, read from the same directories,
//! filtered by the same visibility keys. It exists because a caregiver setting
//! a device up needs to reach Steam, a file manager or a package manager
//! without leaving the kiosk for another session.
//!
//! Deliberately hand-rolled rather than taking a dependency. What the picker
//! needs is a small, well-specified subset — one group, a dozen keys, one
//! quoting rule — and the crate already walks the XDG application directories
//! for [`crate::icon`], which is the fiddly part.
//!
//! Not implemented, because the picker does not use them: desktop *actions*
//! (`[Desktop Action …]` groups), D-Bus activation (`DBusActivatable`), MIME
//! handling, and startup notification.
//!
//! Spec: <https://specifications.freedesktop.org/desktop-entry-spec/latest/>

use lunchbox_api::DesktopApp;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A parsed entry plus the bits only launching needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub app: DesktopApp,
    /// `Exec`, tokenized and with field codes removed — ready to spawn.
    pub argv: Vec<String>,
    /// The file this came from, for diagnostics.
    pub path: PathBuf,
}

/// Enumerate every application a desktop would show, in name order.
///
/// Honours the spec's precedence: earlier directories in the search path
/// shadow later ones **by desktop file ID**, so a user's
/// `~/.local/share/applications/firefox.desktop` replaces the system one
/// rather than appearing twice.
pub fn list_desktop_apps() -> Vec<DesktopApp> {
    let mut entries = collect_entries(&crate::icon::xdg_application_dirs(), &current_desktops());
    entries.sort_by(|a, b| {
        a.app
            .name
            .to_lowercase()
            .cmp(&b.app.name.to_lowercase())
            .then_with(|| a.app.id.cmp(&b.app.id))
    });
    entries.into_iter().map(|e| e.app).collect()
}

/// Look one application up by its desktop file ID, with everything needed to
/// launch it. `None` when no visible entry has that ID.
pub fn find_desktop_app(id: &str) -> Option<DesktopEntry> {
    collect_entries(&crate::icon::xdg_application_dirs(), &current_desktops())
        .into_iter()
        .find(|e| e.app.id == id)
}

/// Wrap `argv` so it runs inside a terminal emulator, for entries with
/// `Terminal=true`.
///
/// Returns `None` when no terminal is installed, which is the common case on a
/// kiosk and has to be reported rather than papered over: launching a
/// terminal-only program without one produces a process with nowhere to draw,
/// which looks to the caregiver exactly like a launch that silently failed.
pub fn terminal_command(argv: &[String]) -> Option<Vec<String>> {
    // `-e` is the one flag all of these agree on. Ordered by what a shepherd
    // device is most likely to have: foot is what the dev session uses, and
    // the rest are the usual desktop defaults.
    const TERMINALS: [&str; 6] = [
        "foot",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "alacritty",
        "xterm",
    ];
    let term = TERMINALS.iter().find(|t| which(t).is_some())?;
    let mut out = vec![(*term).to_string(), "-e".to_string()];
    out.extend(argv.iter().cloned());
    Some(out)
}

/// The desktop environments this session claims to be, from
/// `XDG_CURRENT_DESKTOP`, upper-cased for the case-insensitive comparison
/// `OnlyShowIn` / `NotShowIn` need.
fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase())
        .collect()
}

/// Walk the search path, newest-winning-by-ID, and return every visible
/// application entry.
fn collect_entries(dirs: &[PathBuf], desktops: &[String]) -> Vec<DesktopEntry> {
    let locale = current_locale();
    let mut by_id: HashMap<String, DesktopEntry> = HashMap::new();

    for dir in dirs {
        for (id, path) in desktop_files_in(dir) {
            // First directory to define an ID wins, and it wins even if its
            // file is hidden or unparseable: the spec's shadowing is by ID, so
            // a user file with `Hidden=true` is how you remove a system entry
            // from your menu. Recording the miss keeps the system one hidden.
            if by_id.contains_key(&id) {
                continue;
            }
            match parse_entry(&path, &id, &locale, desktops) {
                Visibility::Shown(entry) => {
                    by_id.insert(id, *entry);
                }
                Visibility::Hidden => {
                    by_id.insert(
                        id,
                        DesktopEntry {
                            app: DesktopApp {
                                id: String::new(),
                                name: String::new(),
                                comment: None,
                                icon: None,
                                terminal: false,
                            },
                            argv: Vec::new(),
                            path,
                        },
                    );
                }
            }
        }
    }

    // The placeholders above carry an empty id; drop them.
    by_id
        .into_values()
        .filter(|e| !e.app.id.is_empty())
        .collect()
}

/// Every `*.desktop` under `dir`, with its spec-defined ID.
///
/// Subdirectories are part of the ID with `/` turned into `-`, so
/// `applications/kde4/konsole.desktop` is `kde4-konsole.desktop`.
fn desktop_files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("desktop")
                && let Ok(rel) = path.strip_prefix(root)
            {
                let id = rel.to_string_lossy().replace('/', "-");
                out.push((id, path.clone()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

enum Visibility {
    Shown(Box<DesktopEntry>),
    Hidden,
}

impl Visibility {
    fn shown(entry: DesktopEntry) -> Self {
        Visibility::Shown(Box::new(entry))
    }
}

/// Read one `.desktop` file and decide whether the picker should offer it.
fn parse_entry(path: &Path, id: &str, locale: &Locale, desktops: &[String]) -> Visibility {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Visibility::Hidden;
    };
    let keys = parse_group(&content, "Desktop Entry");

    // Only applications. A `.desktop` can also be a Link or a Directory,
    // neither of which is launchable.
    if keys.get("Type").map(String::as_str) != Some("Application") {
        return Visibility::Hidden;
    }
    // `Hidden` means "deleted by the user" — the spec is explicit that it must
    // be treated as though the file were absent.
    if is_true(keys.get("Hidden")) {
        return Visibility::Hidden;
    }
    // `NoDisplay` means "valid, but not for menus": handlers and helpers that
    // exist to be looked up, not launched by a person.
    if is_true(keys.get("NoDisplay")) {
        return Visibility::Hidden;
    }
    if !shows_in(&keys, desktops) {
        return Visibility::Hidden;
    }
    // `TryExec` is the spec's own "is this actually installed?" check.
    if let Some(try_exec) = keys.get("TryExec")
        && which(try_exec).is_none()
    {
        return Visibility::Hidden;
    }

    let Some(exec) = keys.get("Exec") else {
        return Visibility::Hidden;
    };
    let argv = strip_field_codes(&tokenize_exec(exec));
    if argv.is_empty() {
        return Visibility::Hidden;
    }

    // A nameless entry is malformed; fall back to the ID rather than showing a
    // blank tile.
    let name = localized(&keys, "Name", locale)
        .unwrap_or_else(|| id.trim_end_matches(".desktop").to_string());

    Visibility::shown(DesktopEntry {
        app: DesktopApp {
            id: id.to_string(),
            name,
            comment: localized(&keys, "Comment", locale),
            icon: keys.get("Icon").filter(|s| !s.is_empty()).cloned(),
            terminal: is_true(keys.get("Terminal")),
        },
        argv,
        path: path.to_path_buf(),
    })
}

/// `OnlyShowIn` / `NotShowIn` against `XDG_CURRENT_DESKTOP`.
fn shows_in(keys: &HashMap<String, String>, desktops: &[String]) -> bool {
    let listed = |v: &str| {
        v.split(';')
            .filter(|s| !s.is_empty())
            .any(|s| desktops.iter().any(|d| d == &s.to_uppercase()))
    };
    if let Some(only) = keys.get("OnlyShowIn") {
        return listed(only);
    }
    if let Some(not) = keys.get("NotShowIn") {
        return !listed(not);
    }
    true
}

/// Parse one group of a desktop file into its key/value pairs.
///
/// Keys keep their locale suffix (`Name[de]`), because [`localized`] needs it.
/// Values get the spec's escape sequences resolved.
fn parse_group(content: &str, group: &str) -> HashMap<String, String> {
    let mut keys = HashMap::new();
    let mut in_group = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if in_group {
                break; // the group ended; everything we want is behind us
            }
            in_group = header == group;
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            keys.insert(k.trim().to_string(), unescape(v.trim()));
        }
    }
    keys
}

/// The spec's string escapes. Note this is *not* the `Exec` quoting rule,
/// which [`tokenize_exec`] handles separately.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The locale to prefer for `Name` / `Comment`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Locale {
    /// e.g. `de_DE`
    full: Option<String>,
    /// e.g. `de`
    lang: Option<String>,
}

fn current_locale() -> Locale {
    // LC_ALL wins over LC_MESSAGES wins over LANG, per POSIX.
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .unwrap_or_default();
    // `de_DE.UTF-8@euro` → `de_DE` → `de`. The modifier and codeset take part
    // in the spec's full matching rules; the picker keeps to the two forms
    // that account for essentially every translated entry in practice.
    let base = raw.split(['.', '@']).next().unwrap_or("").to_string();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Locale::default();
    }
    let lang = base.split('_').next().map(str::to_string);
    Locale {
        lang: lang.filter(|l| l != &base),
        full: Some(base),
    }
}

/// `Key[locale]` in preference order, falling back to the unlocalized `Key`.
fn localized(keys: &HashMap<String, String>, key: &str, locale: &Locale) -> Option<String> {
    for candidate in [locale.full.as_deref(), locale.lang.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(v) = keys.get(&format!("{key}[{candidate}]")) {
            return Some(v.clone());
        }
    }
    keys.get(key).filter(|s| !s.is_empty()).cloned()
}

/// Split an `Exec` value into arguments, per the spec's quoting rules.
///
/// These are *not* shell rules: only double quotes group, and inside them only
/// `` \ ` $ " `` may be backslash-escaped. Everything is split on unquoted
/// whitespace.
fn tokenize_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut started = false;
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            '\\' if in_quotes => match chars.next() {
                Some(esc @ ('\\' | '`' | '$' | '"')) => cur.push(esc),
                Some(other) => {
                    cur.push('\\');
                    cur.push(other);
                }
                None => cur.push('\\'),
            },
            c if c.is_whitespace() && !in_quotes => {
                if started || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started || !cur.is_empty() {
        args.push(cur);
    }
    args
}

/// Drop the `%` field codes.
///
/// The picker launches an application with no document and no URL, so every
/// code that would carry one expands to nothing. The deprecated codes
/// (`%d %D %n %N %v %m`) must be removed too — the spec says so outright, and
/// leaving one in passes a literal `%v` to the program.
///
/// An argument that was *only* a field code disappears entirely rather than
/// becoming an empty argument, which some programs would parse as a filename.
fn strip_field_codes(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    for arg in argv {
        let mut s = String::with_capacity(arg.len());
        let mut chars = arg.chars().peekable();
        let mut had_code = false;
        while let Some(c) = chars.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match chars.next() {
                // `%%` is a literal percent sign.
                Some('%') => s.push('%'),
                // Everything else known expands to nothing here: file and URL
                // codes have no argument to carry, `%i`/`%c`/`%k` are metadata
                // the picker does not pass, and the rest are deprecated.
                Some(
                    'f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm',
                ) => had_code = true,
                // An unknown code is malformed; drop the `%` and keep going
                // rather than passing something the program will misread.
                Some(other) => {
                    had_code = true;
                    let _ = other;
                }
                None => had_code = true,
            }
        }
        if s.is_empty() && had_code {
            continue;
        }
        out.push(s);
    }
    out
}

fn is_true(value: Option<&String>) -> bool {
    value.map(String::as_str) == Some("true")
}

/// Resolve a command the way `TryExec` and the terminal search need: an
/// absolute or relative path is checked directly, a bare name is looked up on
/// `PATH`.
fn which(command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        let p = PathBuf::from(command);
        return is_executable(&p).then_some(p);
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let candidate = dir.join(command);
            is_executable(&candidate).then_some(candidate)
        })
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn app(dir: &Path, name: &str, extra: &str) {
        write(
            dir,
            name,
            &format!("[Desktop Entry]\nType=Application\nName={name}\nExec=/bin/true\n{extra}"),
        );
    }

    fn collect(dirs: &[PathBuf], desktops: &[&str]) -> Vec<DesktopEntry> {
        let d: Vec<String> = desktops.iter().map(|s| s.to_uppercase()).collect();
        let mut v = collect_entries(dirs, &d);
        v.sort_by(|a, b| a.app.id.cmp(&b.app.id));
        v
    }

    /// The visibility keys are the whole difference between "everything a
    /// desktop would show you" and a menu full of MIME handlers and helpers.
    #[test]
    fn only_visible_application_entries_are_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        app(dir, "good.desktop", "");
        app(dir, "nodisplay.desktop", "NoDisplay=true\n");
        app(dir, "hidden.desktop", "Hidden=true\n");
        write(
            dir,
            "link.desktop",
            "[Desktop Entry]\nType=Link\nName=L\nURL=http://example.com\n",
        );
        write(
            dir,
            "noexec.desktop",
            "[Desktop Entry]\nType=Application\nName=N\n",
        );
        app(
            dir,
            "missing-tryexec.desktop",
            "TryExec=/nonexistent/binary\n",
        );
        app(dir, "present-tryexec.desktop", "TryExec=/bin/sh\n");

        let ids: Vec<String> = collect(&[dir.to_path_buf()], &[])
            .into_iter()
            .map(|e| e.app.id)
            .collect();
        assert_eq!(ids, vec!["good.desktop", "present-tryexec.desktop"]);
    }

    #[test]
    fn only_show_in_and_not_show_in_are_matched_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        app(dir, "gnome-only.desktop", "OnlyShowIn=GNOME;\n");
        app(dir, "sway-only.desktop", "OnlyShowIn=sway;\n");
        app(dir, "not-sway.desktop", "NotShowIn=sway;\n");
        app(dir, "neither.desktop", "");

        let ids: Vec<String> = collect(&[dir.to_path_buf()], &["sway"])
            .into_iter()
            .map(|e| e.app.id)
            .collect();
        assert_eq!(ids, vec!["neither.desktop", "sway-only.desktop"]);
    }

    /// The spec shadows by ID across the search path, and a user file with
    /// `Hidden=true` is the documented way to delete a system entry from your
    /// own menu — so the shadowing has to happen before the visibility check,
    /// not after.
    #[test]
    fn an_earlier_directory_shadows_a_later_one_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let system = tmp.path().join("system");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&system).unwrap();

        write(
            &user,
            "editor.desktop",
            "[Desktop Entry]\nType=Application\nName=My Editor\nExec=/bin/true\n",
        );
        write(
            &system,
            "editor.desktop",
            "[Desktop Entry]\nType=Application\nName=System Editor\nExec=/bin/true\n",
        );
        app(&user, "deleted.desktop", "Hidden=true\n");
        app(&system, "deleted.desktop", "");

        let found = collect(&[user.clone(), system.clone()], &[]);
        assert_eq!(
            found.len(),
            1,
            "the user's Hidden file deletes the system one"
        );
        assert_eq!(
            found[0].app.name, "My Editor",
            "and overrides it when shown"
        );
    }

    #[test]
    fn a_subdirectory_becomes_part_of_the_id() {
        let tmp = tempfile::tempdir().unwrap();
        app(tmp.path(), "kde4/konsole.desktop", "");
        let found = collect(&[tmp.path().to_path_buf()], &[]);
        assert_eq!(found[0].app.id, "kde4-konsole.desktop");
    }

    #[test]
    fn exec_is_tokenized_by_the_specs_quoting_rules_not_the_shells() {
        assert_eq!(tokenize_exec("prog -a -b"), vec!["prog", "-a", "-b"]);
        assert_eq!(
            tokenize_exec(r#"prog "one arg" two"#),
            vec!["prog", "one arg", "two"]
        );
        // Only \ ` $ " are escapable inside quotes; anything else keeps its
        // backslash, which is how Windows-style paths survive.
        assert_eq!(tokenize_exec(r#"prog "a\"b""#), vec!["prog", r#"a"b"#]);
        assert_eq!(tokenize_exec(r#"prog "a\\b""#), vec!["prog", r"a\b"]);
        // An empty quoted argument is a real argument.
        assert_eq!(tokenize_exec(r#"prog "" x"#), vec!["prog", "", "x"]);
        assert_eq!(tokenize_exec("   "), Vec::<String>::new());
    }

    /// Leaving a field code in passes a literal `%U` to the program, and the
    /// deprecated ones must go even though nothing would expand them.
    #[test]
    fn field_codes_are_removed_including_the_deprecated_ones() {
        let strip = |s: &str| strip_field_codes(&tokenize_exec(s));

        assert_eq!(strip("prog %U"), vec!["prog"]);
        assert_eq!(strip("prog %f %i %c %k"), vec!["prog"]);
        assert_eq!(strip("prog %d %D %n %N %v %m"), vec!["prog"]);
        // A code embedded in a larger argument leaves the rest behind.
        assert_eq!(strip("prog --file=%f"), vec!["prog", "--file="]);
        // `%%` is a literal percent, and an argument that is only `%%` stays.
        assert_eq!(strip("prog 100%%"), vec!["prog", "100%"]);
        // Real-world shape: the flag survives, the code does not.
        assert_eq!(
            strip("/usr/bin/krita --nosplash %U"),
            vec!["/usr/bin/krita", "--nosplash"]
        );
    }

    #[test]
    fn names_and_comments_prefer_the_current_locale() {
        let keys = parse_group(
            "[Desktop Entry]\n\
             Name=Files\n\
             Name[de]=Dateien\n\
             Name[de_AT]=Dateien AT\n\
             Comment=Browse files\n",
            "Desktop Entry",
        );

        let de_at = Locale {
            full: Some("de_AT".into()),
            lang: Some("de".into()),
        };
        assert_eq!(localized(&keys, "Name", &de_at).unwrap(), "Dateien AT");

        let de = Locale {
            full: Some("de".into()),
            lang: None,
        };
        assert_eq!(localized(&keys, "Name", &de).unwrap(), "Dateien");

        // A locale with no translation falls back to the plain key.
        let fr = Locale {
            full: Some("fr_FR".into()),
            lang: Some("fr".into()),
        };
        assert_eq!(localized(&keys, "Name", &fr).unwrap(), "Files");
        assert_eq!(localized(&keys, "Comment", &fr).unwrap(), "Browse files");
        assert_eq!(localized(&keys, "GenericName", &fr), None);
    }

    #[test]
    fn only_the_desktop_entry_group_is_read() {
        let keys = parse_group(
            "[Desktop Entry]\n\
             Name=Real\n\
             # a comment\n\
             \n\
             [Desktop Action new]\n\
             Name=Action name\n\
             Exec=other\n",
            "Desktop Entry",
        );
        assert_eq!(keys.get("Name").unwrap(), "Real");
        assert!(
            !keys.contains_key("Exec"),
            "the action's keys must not leak in"
        );
    }

    #[test]
    fn string_escapes_are_resolved() {
        assert_eq!(unescape(r"a\sb"), "a b");
        assert_eq!(unescape(r"a\nb"), "a\nb");
        assert_eq!(unescape(r"a\\b"), r"a\b");
        // An unknown escape is left alone rather than swallowing the backslash.
        assert_eq!(unescape(r"a\qb"), r"a\qb");
    }

    #[test]
    fn a_parsed_entry_carries_what_launching_needs() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "term.desktop",
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Package Manager\n\
             Comment=Install things\n\
             Icon=system-software-install\n\
             Exec=/bin/sh -c \"apt update\" %U\n\
             Terminal=true\n",
        );
        let found = collect(&[tmp.path().to_path_buf()], &[]);
        assert_eq!(found.len(), 1);
        let e = &found[0];
        assert_eq!(e.app.name, "Package Manager");
        assert_eq!(e.app.comment.as_deref(), Some("Install things"));
        assert_eq!(e.app.icon.as_deref(), Some("system-software-install"));
        assert!(e.app.terminal);
        assert_eq!(e.argv, vec!["/bin/sh", "-c", "apt update"]);
    }
}
