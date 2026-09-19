//! Reader launch materialization for `type = "ebook"` entries (issue #160).
//!
//! A document reader launched bare is not usable as a supervised reading
//! activity. Okular's defaults hand a child a file dialog over the whole
//! filesystem (Ctrl+O), a print dialog with a write path, a settings dialog,
//! and a menubar, toolbar and sidebar competing with the page. It also writes
//! its reading position **only when the window closes cleanly** — it installs
//! no `SIGTERM` handler at all — so under a signal-only stop every session
//! would start again at page one.
//!
//! This module renders the configuration that fixes the first problem and
//! builds the argv around it. The second is fixed in the stop path: see
//! [`EntryKind::wants_polite_close`](lunchbox_api::EntryKind::wants_polite_close).
//!
//! **The admin's own KDE configuration is never touched.** Everything below
//! goes into a per-entry `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_CACHE_HOME`,
//! which is also what keeps a child's reading out of the admin's recent-files
//! list and their settings out of the child's reach.
//!
//! Three separate mechanisms are needed, because Okular has no single "kiosk"
//! switch:
//!
//! 1. **`kdeglobals`** carries KDE's Kiosk *action restrictions*. Restricting
//!    an action makes `KActionCollection::addAction` disable it, hide it and
//!    block its signals, so the menu item, the toolbar button *and* the
//!    keyboard shortcut die together. The `[$i]` marker makes the group
//!    immutable, and no environment variable lifts it.
//! 2. **`okularrc` / `okularpartrc`** carry the view: no menubar, no sidebar,
//!    no scrollbars, one page (or one spread) at a time, fitted to the screen.
//!
//! The toolbar is the awkward one: nothing on disk hides it directly, so it goes
//! by asking Okular to start in its own full-screen mode, which hides the
//! menubar and toolbar together — see [`render_okularrc`] for why that survives
//! the compositor refusing the fullscreen state.
//!
//! Every file is re-rendered before each launch. Okular rewrites its own config
//! on exit, so a one-time seed would decay; re-rendering makes the restrictions
//! hold across sessions as well as within one. The reading positions are *not*
//! touched: they live in `<data>/okular/docdata/`, which nothing here writes.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lunchbox_api::{EbookLayout, EbookViewer, EntryKind};
use tracing::debug;

/// Overrides the root under which per-entry reader state is created. Tests
/// point it at a scratch dir; otherwise the daemon's data dir is used.
pub const EBOOK_ROOT_ENV: &str = "LUNCHBOX_EBOOK_ROOT";

/// Floor on the graceful-stop grace period for a reading session.
///
/// The close request has to reach the reader, its window has to run its close
/// handler, and the position has to reach disk. Measured at 0.33 s (PDF) to
/// ~2 s (EPUB) on a slow debug build; the polite-close budget is 3 s, and this
/// leaves room for the `SIGTERM` fallback behind it.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// The reader-specific fields of an `EntryKind::Ebook`, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct Spec<'a> {
    pub book: &'a Path,
    pub viewer: EbookViewer,
    pub open_at: Option<u32>,
    pub layout: EbookLayout,
    pub font_size: u32,
    pub font_family: &'a str,
    pub command: Option<&'a str>,
    pub args: &'a [String],
    pub kiosk: bool,
}

/// Where a single entry's reader state lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Per-entry root; everything below is inside it.
    pub root: PathBuf,
    /// `XDG_CONFIG_HOME` for the reader.
    pub config: PathBuf,
    /// `XDG_DATA_HOME`. Reading positions live here, under `okular/docdata/`.
    pub data: PathBuf,
    /// `XDG_CACHE_HOME`.
    pub cache: PathBuf,
}

/// Everything the adapter needs to spawn the session.
#[derive(Debug, Clone)]
pub struct Launch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub paths: Paths,
}

/// Root for all per-entry reader state. Absolute, always: these paths are
/// handed to the reader as environment variables, which it resolves against
/// its own working directory rather than the daemon's.
fn root_dir() -> PathBuf {
    // Gated like the other environment redirects (issue #144): on a device the
    // kiosk user owns the environment, and this decides where a child's
    // reading positions are written.
    let root = match crate::helpers::env_override(EBOOK_ROOT_ENV) {
        Some(root) => root,
        None => lunchbox_util::default_data_dir().join("ebook"),
    };
    std::path::absolute(&root).unwrap_or(root)
}

/// Reduce an entry id (or a book's file stem) to one safe path segment.
fn sanitize_segment(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['-', '.']).to_string();
    if trimmed.is_empty() {
        "entry".to_string()
    } else {
        trimmed
    }
}

/// Per-entry directory layout, keyed by entry id.
///
/// Keyed by entry rather than shared, so an operator can back up or reset one
/// book by name, and so a book that appears in two entries (a shared reader and
/// a bedtime one, say) keeps a position per entry. Callers without an entry id
/// fall back to the book's file stem.
///
/// `root` is made absolute here rather than by the caller: these paths become
/// the reader's `XDG_*` variables, which it resolves against *its own* working
/// directory.
pub fn paths_for_in(root: &Path, entry_id: Option<&str>, book: &Path) -> Paths {
    let key = entry_id
        .map(sanitize_segment)
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| {
            sanitize_segment(&book.file_stem().unwrap_or_default().to_string_lossy())
        });

    let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let root = absolute.join(key);
    Paths {
        config: root.join("config"),
        data: root.join("data"),
        cache: root.join("cache"),
        root,
    }
}

/// Okular actions that open a door out of the book.
///
/// Ordered as they appear to a child rather than alphabetically: the ways out
/// first, then the ways to change what the activity is. Names are the
/// `KActionCollection` names from Okular's `shell/shell.cpp` and `part/part.cpp`
/// — a name that no longer exists is inert, not an error, which is what makes
/// this list safe to carry across reader versions.
const RESTRICTED_ACTIONS: &[&str] = &[
    // Ways to reach another file.
    "file_open",
    "file_open_recent",
    "file_save_as",
    "file_export_as",
    "file_print",
    "file_print_preview",
    "file_share",
    "open_containing_folder",
    "embedded_files",
    "import_ps",
    // Ways to change the reader itself.
    "options_configure",
    "options_configure_keybinding",
    "options_configure_toolbars",
    "options_show_menubar",
    "options_show_toolbar",
    "toolbars_submenu_action",
    "options_configure_annotations",
    // The toolbar itself cannot be removed (see `render_okularrc`), so the two
    // doors it opens are shut instead: the hamburger menu, which is the whole
    // menubar in one button, and the sidebar toggle.
    "hamburger_menu",
    "show_leftpanel",
    // Ways out to the web.
    "help_report_bug",
    "help_contents",
    "help_about_app",
];

/// Render the Kiosk restrictions.
///
/// `[$i]` on the group is what stops the application writing over them; the
/// generic `shell_access` restriction is KDE's own "no launching things from
/// inside an app".
pub fn render_kdeglobals(kiosk: bool) -> String {
    if !kiosk {
        return "# lunchbox: kiosk = false, no restrictions applied\n".to_string();
    }

    let mut out = String::from(
        "# Generated by lunchbox (issue #160). Re-rendered on every launch.\n\
         [KDE Action Restrictions][$i]\n",
    );
    for action in RESTRICTED_ACTIONS {
        out.push_str(&format!("action/{action}=false\n"));
    }
    out.push_str("shell_access=false\n");
    out.push_str("movable_toolbars=false\n");
    out
}

/// Render the reader's window settings: what furniture is on screen.
///
/// The toolbar is the interesting one. Its visibility is not a config key —
/// `KToolBar` reads it from the `hidden` attribute of the XMLGUI definition —
/// and supplying that definition does not work either: measured on a device,
/// the toolbar survives an empty config, `[MainWindow][Toolbar mainToolBar]
/// Hidden=true`, and a local GUI document declaring `hidden="true"`, minimal or
/// a full copy of Okular's own with a version stamp beating it.
///
/// What does work is Okular's own **full-screen mode**, which hides the menubar
/// and the toolbar together (`Shell::slotUpdateFullScreen`). lunchbox asks for
/// it in config, and the two `shouldShow…ComingFromFullScreen` keys are the
/// other half: sway refuses the fullscreen surface state to keep the HUD
/// visible, so Okular leaves the mode again immediately — and on the way out it
/// restores exactly what those keys say, which is nothing.
///
/// The window keeps its ordinary geometry throughout, inside the HUD's
/// exclusive zone, because the compositor never granted the fullscreen state in
/// the first place.
///
/// Note the `fullscreen` action must stay *unrestricted* for this: hiding the
/// chrome hangs off that action's checked state, so a Kiosk restriction on it
/// would leave the toolbar on screen.
pub fn render_okularrc(kiosk: bool) -> String {
    let menubar = if kiosk { "Disabled" } else { "Enabled" };
    let sidebar = !kiosk;
    format!(
        "# Generated by lunchbox (issue #160). Re-rendered on every launch.\n\
         [MainWindow][$i]\n\
         MenuBar={menubar}\n\
         \n\
         [General][$i]\n\
         ShowSidebar={sidebar}\n\
         LockSidebar=true\n\
         \n\
         [Desktop Entry][$i]\n\
         FullScreen={kiosk}\n\
         shouldShowMenuBarComingFromFullScreen=false\n\
         shouldShowToolBarComingFromFullScreen=false\n"
    )
}

/// Render the page view: how the book is laid out and what is drawn around it.
///
/// A paged layout gets `ViewContinuous=false` and `ZoomMode=2` (Fit Page),
/// which is what makes "one page at a time" mean a whole page rather than the
/// top of one. `EbookLayout::Scroll` gets the opposite — one continuous column
/// fitted to the width — because that is the only shape a touchscreen can
/// navigate without a keyboard.
///
/// The background is the colour around the page, not the paper: white makes
/// the letterboxing beside a portrait page disappear on a landscape screen.
/// Scrollbars stay hidden even when scrolling: dragging is the gesture, and a
/// scrollbar is a thing to catch by accident, not a control.
pub fn render_okularpartrc(layout: EbookLayout, kiosk: bool) -> String {
    format!(
        "# Generated by lunchbox (issue #160). Re-rendered on every launch.\n\
         [Main View][$i]\n\
         ShowLeftPanel={left_panel}\n\
         \n\
         [PageView][$i]\n\
         ShowScrollBars={scrollbars}\n\
         UseCustomBackgroundColor=true\n\
         BackgroundColor=#ffffff\n\
         ViewContinuous={continuous}\n\
         ViewMode={view_mode}\n\
         TrimMode=None\n\
         \n\
         [Zoom][$i]\n\
         ZoomMode={zoom_mode}\n\
         \n\
         [General][$i]\n\
         ShowOSD=false\n\
         ShowEmbeddedContentMessages=false\n",
        left_panel = !kiosk,
        scrollbars = !kiosk,
        continuous = layout.is_continuous(),
        view_mode = layout.okular_view_mode(),
        zoom_mode = layout.okular_zoom_mode(),
    )
}

/// Render the EPUB backend's font.
///
/// Okular reflows an EPUB at this size and paginates from it, so it is the
/// reading-size knob: with the page fitted to the screen, a larger font means
/// fewer words per page and larger text. It also means *different* pages, which
/// is why changing it moves a remembered position.
///
/// The key lives in the unnamed group because that is where a `KConfigSkeleton`
/// with no `setCurrentGroup` puts it — `[General]` is silently ignored. The
/// value is Qt's font serialization; only family and point size are set, the
/// rest are Qt's defaults for weight, style and hinting.
pub fn render_font_settings(family: &str, size: u32) -> String {
    format!("[No Group]\nFont={family},{size},-1,5,400,0,0,0,0,0,0,0,0,0,0,1\n")
}

/// Write every generated file into the entry's config and data directories.
fn materialize(paths: &Paths, spec: &Spec<'_>) -> io::Result<()> {
    std::fs::create_dir_all(&paths.config)?;
    std::fs::create_dir_all(&paths.data)?;
    std::fs::create_dir_all(&paths.cache)?;

    std::fs::write(
        paths.config.join("kdeglobals"),
        render_kdeglobals(spec.kiosk),
    )?;
    std::fs::write(paths.config.join("okularrc"), render_okularrc(spec.kiosk))?;
    std::fs::write(
        paths.config.join("okularpartrc"),
        render_okularpartrc(spec.layout, spec.kiosk),
    )?;
    std::fs::write(
        paths.config.join("okular_epub_generator_settings"),
        render_font_settings(spec.font_family, spec.font_size),
    )?;

    // Earlier versions wrote XMLGUI documents here to hide the reader's
    // toolbar. They never took effect on a real session (see `render_okularrc`),
    // so they are cleaned up rather than left behind looking as though they do
    // something.
    match std::fs::remove_dir_all(paths.data.join("kxmlgui5")) {
        Ok(()) => debug!("Removed an obsolete XMLGUI override"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    debug!(config = %paths.config.display(), "Rendered reader configuration");
    Ok(())
}

/// Build the reader's argv.
fn build_argv(spec: &Spec<'_>, command: &str, book: &str) -> Vec<String> {
    let mut argv = vec![command.to_string()];

    // Only meaningful on the first launch: once docdata has a viewport for this
    // book the reader restores that instead, which is the whole point.
    if let Some(page) = spec.open_at {
        argv.push("-p".to_string());
        argv.push(page.to_string());
    }

    argv.extend(spec.args.iter().cloned());
    argv.push(book.to_string());
    argv
}

/// The environment that points the reader at the entry's own state.
fn build_env(paths: &Paths) -> Vec<(String, String)> {
    vec![
        (
            "XDG_CONFIG_HOME".to_string(),
            paths.config.to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            paths.data.to_string_lossy().into_owned(),
        ),
        (
            "XDG_CACHE_HOME".to_string(),
            paths.cache.to_string_lossy().into_owned(),
        ),
    ]
}

/// Report a book that is not where the entry says it is.
///
/// Returns the expanded path, so the diagnostic names what was actually looked
/// for rather than the `~/`-shaped config value.
pub fn missing_book(kind: &EntryKind) -> Option<String> {
    let EntryKind::Ebook { book, .. } = kind else {
        return None;
    };
    let expanded = crate::adapter::expand_tilde(&book.to_string_lossy());
    (!Path::new(&expanded).exists()).then_some(expanded)
}

/// What an `ebook` entry needs on the device but has not got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingSupport {
    /// The reader binary is not installed.
    Reader { command: String },
    /// The reader is installed but cannot open this format — on Ubuntu, EPUB
    /// and DjVu live in a separate `okular-extra-backends` package, and
    /// without it the child gets an error dialog instead of a book.
    Backend {
        format: String,
        generator: &'static str,
    },
}

/// Directories Okular loads its format backends from.
///
/// The multiarch triplet is not known at compile time and the Qt plugin path
/// can be redirected, so this globs the usual roots rather than naming one.
fn generator_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("QT_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&path).map(|p| p.join("okular_generators")));
    }
    for lib in ["/usr/lib", "/usr/local/lib", "/usr/lib64"] {
        let root = Path::new(lib);
        dirs.push(root.join("qt6/plugins/okular_generators"));
        // Multiarch: /usr/lib/x86_64-linux-gnu/qt6/plugins/okular_generators
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                dirs.push(entry.path().join("qt6/plugins/okular_generators"));
            }
        }
    }
    dirs
}

/// Whether a backend plugin with this name is installed.
fn generator_installed(generator: &str) -> bool {
    generator_dirs()
        .iter()
        .any(|dir| dir.join(generator).exists())
}

/// The Okular backend a book's extension needs, when it is one that ships
/// separately from the reader itself.
fn separate_backend(book: &Path) -> Option<(&'static str, &'static str)> {
    match book
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("epub") => Some(("EPUB", "okularGenerator_epub.so")),
        Some("djvu") | Some("djv") => Some(("DjVu", "okularGenerator_djvu.so")),
        Some("md") | Some("markdown") => Some(("Markdown", "okularGenerator_md.so")),
        _ => None,
    }
}

/// Report a reader, or a format backend, that this entry needs and the device
/// has not got.
///
/// Checked at the same place as a missing book, and for the same reason: the
/// failure it prevents is a child tapping a tile and getting a black screen or
/// an error dialog, which no log line reaches.
pub fn missing_support(kind: &EntryKind) -> Option<MissingSupport> {
    let EntryKind::Ebook {
        book,
        viewer,
        command,
        ..
    } = kind
    else {
        return None;
    };

    let command = command.as_deref().unwrap_or(viewer.default_command());
    let expanded = crate::adapter::expand_tilde(command);
    // `resolve` searches the trusted directories (and `$PATH` in a dev
    // session) and falls back to a path that does not exist, so this answers
    // "would the spawn find it" rather than "is there a file of that name".
    let installed = crate::helpers::resolve(&expanded).exists();
    if !installed {
        return Some(MissingSupport::Reader { command: expanded });
    }

    if let Some((format, generator)) = separate_backend(book)
        && !generator_installed(generator)
    {
        return Some(MissingSupport::Backend {
            format: format.to_string(),
            generator,
        });
    }

    None
}

/// Render the configuration and build the launch.
pub fn prepare<F>(spec: &Spec<'_>, entry_id: Option<&str>, expand: F) -> io::Result<Launch>
where
    F: Fn(&str) -> String,
{
    prepare_in(&root_dir(), spec, entry_id, expand)
}

/// [`prepare`], against a root the caller supplies. See [`paths_for_in`].
pub fn prepare_in<F>(
    root: &Path,
    spec: &Spec<'_>,
    entry_id: Option<&str>,
    expand: F,
) -> io::Result<Launch>
where
    F: Fn(&str) -> String,
{
    let book = expand(&spec.book.to_string_lossy());
    let paths = paths_for_in(root, entry_id, Path::new(&book));

    materialize(&paths, spec)?;

    let command = expand(spec.command.unwrap_or(spec.viewer.default_command()));

    Ok(Launch {
        argv: build_argv(spec, &command, &book),
        env: build_env(&paths),
        paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_for<'a>(book: &'a Path, args: &'a [String]) -> Spec<'a> {
        Spec {
            book,
            viewer: EbookViewer::Okular,
            open_at: None,
            layout: EbookLayout::default(),
            font_size: 16,
            font_family: "Noto Serif",
            command: None,
            args,
            kiosk: true,
        }
    }

    #[test]
    fn restrictions_are_immutable_and_cover_the_file_dialog() {
        let rendered = render_kdeglobals(true);
        assert!(rendered.contains("[KDE Action Restrictions][$i]"));
        // The one that matters most: Ctrl+O is a filesystem browser.
        assert!(rendered.contains("action/file_open=false"));
        assert!(rendered.contains("action/file_print=false"));
        assert!(rendered.contains("action/options_configure=false"));
        assert!(rendered.contains("shell_access=false"));
        // The toolbar cannot be hidden, so what it opens must be shut: the
        // hamburger menu is the whole menubar in one button, and the sidebar
        // toggle brings back the panel the config just turned off.
        assert!(rendered.contains("action/hamburger_menu=false"));
        assert!(rendered.contains("action/show_leftpanel=false"));
    }

    /// The toolbar goes only because the reader starts in its own full-screen
    /// mode and is told to restore nothing on the way out of it — see
    /// `render_okularrc`. Both halves have to be there: with `FullScreen=true`
    /// alone, leaving the mode brings the menubar *and* the toolbar back, which
    /// is worse than never asking.
    #[test]
    fn the_chrome_goes_through_full_screen_mode_and_stays_gone() {
        let rendered = render_okularrc(true);
        assert!(rendered.contains("[Desktop Entry][$i]"));
        assert!(rendered.contains("FullScreen=true"));
        assert!(rendered.contains("shouldShowMenuBarComingFromFullScreen=false"));
        assert!(rendered.contains("shouldShowToolBarComingFromFullScreen=false"));

        // An unrestricted run is the reader as it comes.
        assert!(render_okularrc(false).contains("FullScreen=false"));
    }

    /// Hiding the chrome hangs off the full-screen *action*, so restricting it
    /// would leave the toolbar on screen. Easy to add by reflex; expensive to
    /// notice.
    #[test]
    fn the_fullscreen_action_is_not_restricted() {
        assert!(!RESTRICTED_ACTIONS.contains(&"fullscreen"));
        assert!(!render_kdeglobals(true).contains("action/fullscreen=false"));
    }

    #[test]
    fn kiosk_false_restricts_nothing() {
        let rendered = render_kdeglobals(false);
        assert!(!rendered.contains("KDE Action Restrictions"));
        assert!(render_okularrc(false).contains("MenuBar=Enabled"));
        assert!(render_okularpartrc(EbookLayout::Single, false).contains("ShowScrollBars=true"));
    }

    /// The touch layout is the paged one turned inside out, and both halves
    /// have to move together: a continuous view fitted to the *page* would
    /// show one page and refuse to scroll past it.
    #[test]
    fn the_scroll_layout_is_continuous_and_fitted_to_the_width() {
        let rendered = render_okularpartrc(EbookLayout::Scroll, true);
        assert!(rendered.contains("ViewContinuous=true"));
        assert!(rendered.contains("ZoomMode=1"));
        assert!(rendered.contains("ViewMode=Single"));
        assert!(!EbookLayout::Scroll.needs_keys_to_turn_pages());
        assert!(EbookLayout::Facing.needs_keys_to_turn_pages());
    }

    /// The shipped default puts the cover on its own, the way a paper book
    /// opens — and it must still be a *paged* layout, since that is what the
    /// page-turn buttons and the no-page-turn diagnostic assume.
    #[test]
    fn the_default_layout_is_facing_with_the_cover_alone() {
        assert_eq!(EbookLayout::default(), EbookLayout::FacingFirstCentered);
        assert_eq!(
            EbookLayout::default().okular_view_mode(),
            "FacingFirstCentered"
        );
        assert!(EbookLayout::default().needs_keys_to_turn_pages());
    }

    #[test]
    fn layout_maps_to_okular_view_modes() {
        assert!(render_okularpartrc(EbookLayout::Facing, true).contains("ViewMode=Facing"));
        assert!(render_okularpartrc(EbookLayout::Single, true).contains("ViewMode=Single"));
        assert!(
            render_okularpartrc(EbookLayout::FacingFirstCentered, true)
                .contains("ViewMode=FacingFirstCentered")
        );
        // Page at a time, fitted to the screen: the two settings that make a
        // page a page rather than a scroll position.
        let rendered = render_okularpartrc(EbookLayout::Facing, true);
        assert!(rendered.contains("ViewContinuous=false"));
        assert!(rendered.contains("ZoomMode=2"));
    }

    #[test]
    fn font_goes_in_the_unnamed_group() {
        // `[General]` is silently ignored here; the group name is the finding.
        let rendered = render_font_settings("Noto Serif", 18);
        assert!(rendered.starts_with("[No Group]\n"));
        assert!(rendered.contains("Font=Noto Serif,18,"));
    }

    #[test]
    fn paths_are_keyed_by_entry_id_then_book() {
        let root = Path::new("/state");
        let by_id = paths_for_in(root, Some("the-hobbit"), Path::new("/books/hobbit.epub"));
        assert_eq!(by_id.root, Path::new("/state/the-hobbit"));
        assert_eq!(by_id.config, Path::new("/state/the-hobbit/config"));

        let by_book = paths_for_in(root, None, Path::new("/books/hobbit.epub"));
        assert_eq!(by_book.root, Path::new("/state/hobbit"));

        // An id with separators in it must not escape the root.
        let hostile = paths_for_in(root, Some("../../etc"), Path::new("/books/x.epub"));
        assert_eq!(hostile.root, Path::new("/state/etc"));
    }

    #[test]
    fn prepare_writes_config_and_builds_argv() {
        let scratch = tempfile::tempdir().unwrap();
        let book = scratch.path().join("hobbit.epub");
        std::fs::write(&book, b"not really an epub").unwrap();
        let root = scratch.path().join("state");

        let args = vec!["--extra".to_string()];
        let mut spec = spec_for(&book, &args);
        spec.open_at = Some(12);

        let launch = prepare_in(&root, &spec, Some("the-hobbit"), |s| s.to_string()).unwrap();

        assert_eq!(
            launch.argv,
            vec![
                "okular".to_string(),
                "-p".to_string(),
                "12".to_string(),
                "--extra".to_string(),
                book.to_string_lossy().into_owned(),
            ]
        );

        // The reader must not be able to see, or dirty, the admin's own KDE
        // configuration.
        let config = launch.paths.config.clone();
        assert!(launch.env.contains(&(
            "XDG_CONFIG_HOME".to_string(),
            config.to_string_lossy().into_owned()
        )));
        assert!(config.join("kdeglobals").exists());
        assert!(config.join("okularrc").exists());
        assert!(config.join("okularpartrc").exists());
        assert!(config.join("okular_epub_generator_settings").exists());
        // Reading positions live under the data root, and nothing else does.
        assert!(launch.paths.data.is_dir());
        assert!(!launch.paths.data.join("kxmlgui5").exists());
    }

    #[test]
    fn re_rendering_restores_restrictions_the_reader_wrote_over() {
        let scratch = tempfile::tempdir().unwrap();
        let book = scratch.path().join("hobbit.epub");
        std::fs::write(&book, b"x").unwrap();
        let root = scratch.path().join("state");
        let spec = spec_for(&book, &[]);

        let launch = prepare_in(&root, &spec, Some("book"), |s| s.to_string()).unwrap();
        let kdeglobals = launch.paths.config.join("kdeglobals");

        // Okular rewrites its config on exit; pretend it dropped the lot.
        std::fs::write(&kdeglobals, "[General]\nnothing=true\n").unwrap();
        prepare_in(&root, &spec, Some("book"), |s| s.to_string()).unwrap();

        let restored = std::fs::read_to_string(&kdeglobals).unwrap();
        assert!(restored.contains("action/file_open=false"));
    }

    /// A state directory written by an older build carries XMLGUI documents
    /// that never worked; a launch clears them out rather than leaving a file
    /// that looks like it is hiding the toolbar.
    #[test]
    fn an_obsolete_xmlgui_override_is_cleaned_up() {
        let scratch = tempfile::tempdir().unwrap();
        let book = scratch.path().join("hobbit.epub");
        std::fs::write(&book, b"x").unwrap();
        let root = scratch.path().join("state");
        let spec = spec_for(&book, &[]);

        let launch = prepare_in(&root, &spec, Some("book"), |s| s.to_string()).unwrap();
        let stale = launch.paths.data.join("kxmlgui5/okular_part");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("part.rc"), b"<gui/>").unwrap();

        prepare_in(&root, &spec, Some("book"), |s| s.to_string()).unwrap();
        assert!(!launch.paths.data.join("kxmlgui5").exists());
    }

    #[test]
    fn missing_book_reports_the_expanded_path() {
        let scratch = tempfile::tempdir().unwrap();
        let there = scratch.path().join("here.epub");
        std::fs::write(&there, b"x").unwrap();

        let present = EntryKind::Ebook {
            book: there.clone(),
            viewer: EbookViewer::Okular,
            open_at: None,
            layout: EbookLayout::Facing,
            font_size: 16,
            font_family: "Noto Serif".into(),
            command: None,
            args: vec![],
            env: Default::default(),
            kiosk: true,
        };
        assert_eq!(missing_book(&present), None);

        let absent = EntryKind::Ebook {
            book: there.with_file_name("gone.epub"),
            viewer: EbookViewer::Okular,
            open_at: None,
            layout: EbookLayout::Facing,
            font_size: 16,
            font_family: "Noto Serif".into(),
            command: None,
            args: vec![],
            env: Default::default(),
            kiosk: true,
        };
        assert!(missing_book(&absent).unwrap().ends_with("gone.epub"));
    }
}
