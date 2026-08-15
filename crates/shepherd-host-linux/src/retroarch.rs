//! RetroArch launch materialization for `type = "retroarch"` entries.
//!
//! RetroArch launched bare (`retroarch -L core content`) is not usable as a
//! supervised activity: closing it throws away the session, the child can walk
//! out of the game into its file browser, and its in-game save only reaches
//! disk if the process happens to exit cleanly. This module renders the
//! settings that fix that into a config fragment passed with `--appendconfig`,
//! then builds the argv around it.
//!
//! **The user's `retroarch.cfg` is never edited.** Note that this takes an
//! explicit setting to guarantee: `config_save_on_exit` defaults to *true*, so
//! a clean exit would otherwise write RetroArch's entire live settings block —
//! including everything we appended — back into the user's own config, making
//! shepherd's per-activity choices permanent and global. The fragment turns it
//! off for the run.
//!
//! Two kinds of "save" are in play and they are not interchangeable:
//!
//! - The **in-game save** (SRAM / battery save, `.srm`) is the one the game
//!   itself writes — the file a child would call "my save". RetroArch flushes
//!   it when content unloads, and `autosave_interval` makes it flush
//!   periodically too, so a crash or a `SIGKILL` costs seconds rather than an
//!   afternoon.
//! - The **save state** (`.state.auto`) is a snapshot of the whole emulator.
//!   With [`RetroarchSaveState::Auto`] closing the activity writes one and
//!   opening restores it, so the child resumes mid-battle rather than at the
//!   title screen.
//!
//! Both depend on RetroArch exiting cleanly, which is why
//! [`ManagedProcess::terminate`](crate::process::ManagedProcess::terminate)
//! sends exactly one `SIGTERM`: RetroArch's signal handler hard-exits on the
//! second, skipping every save path.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use shepherd_api::RetroarchSaveState;
use tracing::{debug, warn};

/// Overrides the root under which per-entry save/state directories are
/// created. Tests point it at a scratch dir; otherwise the daemon's data dir
/// is used.
pub const RETROARCH_ROOT_ENV: &str = "SHEPHERD_RETROARCH_ROOT";

/// Floor on the graceful-stop grace period for a RetroArch session.
///
/// The generic 5s is a fine default for an app whose shutdown is just "exit",
/// but here the window has to cover unloading the core, flushing SRAM, and
/// writing a save state, on whatever storage the box has. Overshooting costs a
/// second of black screen at the end of a session; undershooting costs the
/// child's save file.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// How often RetroArch flushes the in-game save while playing, in seconds.
/// Caps what a `SIGKILL` or a power cut can destroy.
const AUTOSAVE_INTERVAL_SECS: u32 = 10;

/// The RetroArch-specific fields of an `EntryKind::Retroarch`, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct Spec<'a> {
    pub core: Option<&'a str>,
    pub core_path: Option<&'a Path>,
    pub content: &'a Path,
    pub save_state: RetroarchSaveState,
    pub command: &'a str,
    pub args: &'a [String],
    pub kiosk: bool,
}

/// Where a single entry's RetroArch state lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Per-entry root; everything below is inside it.
    pub root: PathBuf,
    /// `savestate_directory`
    pub states: PathBuf,
    /// `savefile_directory` — the in-game saves
    pub saves: PathBuf,
    /// The generated `--appendconfig` fragment.
    pub config: PathBuf,
}

/// Everything the adapter needs to spawn and later clean up the session.
#[derive(Debug, Clone)]
pub struct Launch {
    pub argv: Vec<String>,
    pub paths: Paths,
}

/// Serializes tests that point [`RETROARCH_ROOT_ENV`] at a scratch directory.
/// The variable is process-global, and the crate's tests share one binary, so
/// two of them running at once would read each other's root.
#[cfg(test)]
pub(crate) static ROOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Root for all per-entry RetroArch state.
///
/// Absolute, always. These paths end up as `savefile_directory` /
/// `savestate_directory` in the fragment and as the `--appendconfig` argument,
/// where RetroArch resolves them against *its own* working directory rather
/// than the daemon's — so a relative data dir (the dev harness uses
/// `./dev-runtime/data`) would silently scatter saves.
fn root_dir() -> PathBuf {
    let root = match std::env::var_os(RETROARCH_ROOT_ENV) {
        Some(root) => PathBuf::from(root),
        None => shepherd_util::default_data_dir().join("retroarch"),
    };
    std::path::absolute(&root).unwrap_or(root)
}

/// Reduce an entry id (or a content file stem) to one safe path segment.
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

/// Per-entry directory layout.
///
/// Keyed by entry id so two entries pointing at the same ROM keep separate
/// progress, and so an operator can back up or reset one activity by name.
/// Callers without an entry id (a direct `spawn` in a test) fall back to the
/// content's file stem, which is stable for the same content.
pub fn paths_for(entry_id: Option<&str>, content: &Path) -> Paths {
    let key = entry_id
        .map(sanitize_segment)
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| {
            sanitize_segment(&content.file_stem().unwrap_or_default().to_string_lossy())
        });

    let root = root_dir().join(key);
    Paths {
        states: root.join("states"),
        saves: root.join("saves"),
        config: root.join("append.cfg"),
        root,
    }
}

/// Overrides the libretro core search path (`:`-separated). For installs that
/// put cores somewhere unusual, and for tests.
pub const LIBRETRO_DIR_ENV: &str = "SHEPHERD_LIBRETRO_DIR";

/// Directories searched for a core by name, in order of preference: the user's
/// own downloaded cores first, then the distro's packaged ones (Debian and
/// Ubuntu install `libretro-*` under the multiarch libdir).
fn core_search_dirs() -> Vec<PathBuf> {
    if let Some(raw) = std::env::var_os(LIBRETRO_DIR_ENV) {
        return std::env::split_paths(&raw).collect();
    }

    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".config/retroarch/cores"));
    }
    // The multiarch triplet varies (x86_64-linux-gnu, aarch64-linux-gnu, …);
    // read the directory rather than guessing the host's.
    if let Ok(entries) = std::fs::read_dir("/usr/lib") {
        let mut multiarch: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("libretro"))
            .filter(|p| p.is_dir())
            .collect();
        multiarch.sort();
        dirs.extend(multiarch);
    }
    dirs.push(PathBuf::from("/usr/lib/libretro"));
    dirs.push(PathBuf::from("/usr/local/lib/libretro"));
    dirs
}

/// Cores whose apt package name and shared-object name disagree by more than
/// punctuation, keyed by the [normalized](normalize_core) package name.
///
/// The Beetle family are libretro's forks of Mednafen and keep the upstream
/// name on disk, so `apt install libretro-beetle-psx` lands
/// `mednafen_psx_hw_libretro.so`. Without these an operator would have to know
/// two names for one core — and would find out only when the activity failed
/// to launch.
///
/// Taken from the packages themselves (`dpkg -c` over every `libretro-*` in
/// the Ubuntu archive and the libretro PPA), not from guesswork: of 87 core
/// packages, 74 match their shared object by name and these 12 do not.
///
/// `beetle-psx` ships two cores — a software and a hardware renderer. The
/// hardware one is the better default; the software one is still reachable by
/// its own name, `mednafen_psx`.
const CORE_ALIASES: [(&str, &str); 12] = [
    ("beetlegba", "mednafen_gba"),
    ("beetlelynx", "mednafen_lynx"),
    ("beetlengp", "mednafen_ngp"),
    ("beetlepcefast", "mednafen_pce_fast"),
    ("beetlepcfx", "mednafen_pcfx"),
    ("beetlepsx", "mednafen_psx_hw"),
    ("beetlesaturn", "mednafen_saturn"),
    ("beetlesupergrafx", "mednafen_supergrafx"),
    ("beetlevb", "mednafen_vb"),
    ("beetlewswan", "mednafen_wswan"),
    ("lrps2", "pcsx2"),
    ("np2", "nekop2"),
];

/// Reduce a core name to what is worth comparing: lowercase, with separators
/// dropped. Package names and shared objects disagree about `-` versus `_`
/// (`libretro-mupen64plus-next` ships `mupen64plus_next_libretro.so`) and about
/// whether words are separated at all (`libretro-genesisplusgx` ships
/// `genesis_plus_gx_libretro.so`).
fn normalize_core(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| *c != '-' && *c != '_')
        .collect()
}

/// Strip the decorations an operator might include: `mgba`, `mgba_libretro`,
/// and `mgba_libretro.so` all name the same core.
fn core_stem(name: &str) -> &str {
    let stem = name.strip_suffix(".so").unwrap_or(name);
    stem.strip_suffix("_libretro")
        .or_else(|| stem.strip_suffix("-libretro"))
        .unwrap_or(stem)
}

/// Turn a configured core name into a shared-object filename.
pub fn core_filename(name: &str) -> String {
    format!("{}_libretro.so", core_stem(name))
}

/// Resolve a core name to a path.
///
/// Matches against the files actually present rather than a filename computed
/// from the name, because the two disagree often enough that computing it is
/// wrong: separators differ (`mupen64plus-next` → `mupen64plus_next`), words
/// run together (`genesisplusgx` → `genesis_plus_gx`), and the Beetle cores
/// carry an entirely different upstream name (see [`CORE_ALIASES`]).
///
/// Best-effort by design: if nothing matches, hand RetroArch the bare filename
/// so it can resolve it against its own configured `libretro_directory`, which
/// is what `retroarch -L mgba_libretro.so` does today. An unusual install keeps
/// working rather than failing on our guess.
pub fn resolve_core(name: &str) -> String {
    let stem = core_stem(name);
    let wanted = normalize_core(stem);
    let wanted = CORE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == wanted)
        .map(|(_, real)| normalize_core(real))
        .unwrap_or(wanted);

    for dir in core_search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Sorted so a directory holding several matches resolves the same way
        // every time rather than in readdir order.
        let mut candidates: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        candidates.sort();
        for candidate in candidates {
            let Some(file_name) = candidate.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(file_stem) = file_name.strip_suffix("_libretro.so") else {
                continue;
            };
            if normalize_core(file_stem) == wanted && candidate.is_file() {
                debug!(core = %name, path = %candidate.display(), "Resolved libretro core");
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    let filename = core_filename(name);
    warn!(
        core = %name,
        filename = %filename,
        "libretro core not found in the usual directories; \
         passing the bare name for RetroArch to resolve"
    );
    filename
}

/// Quote a value for a RetroArch config line. RetroArch writes its own config
/// with quoted values, so this matches what it round-trips.
fn cfg_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render the `--appendconfig` fragment.
pub fn render_append_config(paths: &Paths, save_state: RetroarchSaveState, kiosk: bool) -> String {
    let auto = save_state.is_auto();
    let mut out = String::new();

    out.push_str(
        "# Generated by shepherd-launcher. Rewritten on every launch, so edits\n\
         # here are lost. Appended to the user's retroarch.cfg for this run\n\
         # only; that file is never modified.\n\n",
    );

    out.push_str(
        "# Without this, a clean exit writes RetroArch's whole live settings\n\
         # block -- everything below included -- back into retroarch.cfg,\n\
         # making these per-activity choices permanent and global.\n",
    );
    out.push_str(&format!("config_save_on_exit = {}\n\n", cfg_quote("false")));

    out.push_str("# Per-activity save and state directories.\n");
    out.push_str(&format!(
        "savefile_directory = {}\n",
        cfg_quote(&paths.saves.to_string_lossy())
    ));
    out.push_str(&format!(
        "savestate_directory = {}\n\n",
        cfg_quote(&paths.states.to_string_lossy())
    ));

    out.push_str(
        "# Save state on close, restore it on open, so the activity resumes\n\
         # where the session ended rather than at the title screen.\n",
    );
    out.push_str(&format!(
        "savestate_auto_save = {}\n",
        cfg_quote(if auto { "true" } else { "false" })
    ));
    out.push_str(&format!(
        "savestate_auto_load = {}\n\n",
        cfg_quote(if auto { "true" } else { "false" })
    ));

    out.push_str(
        "# Flush the in-game save periodically. The save state above only\n\
         # survives a clean exit; this caps what a crash or a forced kill can\n\
         # destroy at a few seconds of play.\n",
    );
    out.push_str(&format!(
        "autosave_interval = {}\n\n",
        cfg_quote(&AUTOSAVE_INTERVAL_SECS.to_string())
    ));

    out.push_str(
        "# The HUD is a layer-shell surface that takes keyboard focus for its\n\
         # popovers; left at RetroArch's default the game would pause every\n\
         # time one opened.\n",
    );
    out.push_str(&format!("pause_nonactive = {}\n\n", cfg_quote("false")));

    out.push_str("# One activity, fullscreen, no window furniture.\n");
    out.push_str(&format!("video_fullscreen = {}\n\n", cfg_quote("true")));

    out.push_str(
        "# Kiosk mode locks RetroArch's own menu: no settings, no file\n\
         # browser, no loading other content from inside the activity.\n",
    );
    out.push_str(&format!(
        "kiosk_mode_enable = {}\n",
        cfg_quote(if kiosk { "true" } else { "false" })
    ));

    out
}

/// Suffixes of the files RetroArch's auto save state is made of: the state
/// itself and, when `savestate_thumbnail_enable` is on, its screenshot.
const AUTO_STATE_SUFFIXES: [&str; 2] = [".state.auto", ".state.auto.png"];

/// Delete the auto save state, so the next launch boots the content from its
/// power-on screen instead of resuming.
///
/// Only the save *state* — the in-game save (`.srm`) beside it is the child's
/// actual progress and is deliberately left alone. Resetting a console returns
/// it to the title screen; it does not wipe the cartridge.
///
/// RetroArch files states under a per-core subdirectory of the one we hand it,
/// so this walks one level down rather than assuming a flat layout. Returns
/// how many files it removed.
pub fn discard_auto_state(paths: &Paths) -> io::Result<usize> {
    let mut removed = 0;

    let mut dirs = vec![paths.states.clone()];
    if let Ok(entries) = std::fs::read_dir(&paths.states) {
        dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if AUTO_STATE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        debug!(path = %path.display(), "Discarded auto save state");
                        removed += 1;
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }

    Ok(removed)
}

/// Create the per-entry directories and write the config fragment.
pub fn materialize(paths: &Paths, save_state: RetroarchSaveState, kiosk: bool) -> io::Result<()> {
    std::fs::create_dir_all(&paths.states)?;
    std::fs::create_dir_all(&paths.saves)?;
    if let Some(parent) = paths.config.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &paths.config,
        render_append_config(paths, save_state, kiosk),
    )
}

/// Build the argv for a RetroArch launch.
///
/// Entry-supplied `args` go last so an operator can override anything we
/// derived — RetroArch takes the final occurrence of a repeated flag.
pub fn build_argv(spec: &Spec<'_>, config: &Path, core: &str, content: &str) -> Vec<String> {
    let mut argv = vec![
        spec.command.to_string(),
        "--appendconfig".to_string(),
        config.to_string_lossy().into_owned(),
        "-f".to_string(),
        "-L".to_string(),
        core.to_string(),
        content.to_string(),
    ];
    argv.extend(spec.args.iter().cloned());
    argv
}

/// Resolve paths, write the config fragment, and build the argv.
///
/// `expand` is the caller's tilde expansion, threaded in so this module and
/// the rest of the adapter agree on what `~/Games/…` means.
pub fn prepare<F>(spec: &Spec<'_>, entry_id: Option<&str>, expand: F) -> io::Result<Launch>
where
    F: Fn(&str) -> String,
{
    let content = expand(&spec.content.to_string_lossy());
    let paths = paths_for(entry_id, Path::new(&content));

    materialize(&paths, spec.save_state, spec.kiosk)?;

    let core = match (spec.core_path, spec.core) {
        (Some(path), _) => expand(&path.to_string_lossy()),
        (None, Some(name)) => resolve_core(name),
        // Validation rejects an entry with neither, so this only happens for a
        // hand-built EntryKind. Let RetroArch report it.
        (None, None) => String::new(),
    };

    let command = expand(spec.command);
    let spec = Spec {
        command: &command,
        ..*spec
    };

    Ok(Launch {
        argv: build_argv(&spec, &paths.config, &core, &content),
        paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_for<'a>(content: &'a Path, args: &'a [String]) -> Spec<'a> {
        Spec {
            core: Some("mgba"),
            core_path: None,
            content,
            save_state: RetroarchSaveState::Auto,
            command: "retroarch",
            args,
            kiosk: true,
        }
    }

    /// Every naming shape the packaged cores actually use, taken from the
    /// `.deb` contents rather than guessed: the package name and the shared
    /// object agree only about half the time, so resolution has to match
    /// against what is on disk.
    #[test]
    fn resolve_core_matches_the_real_package_naming() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let dir = scratch.path();
        for so in [
            "mgba_libretro.so",
            "snes9x_libretro.so",
            "genesis_plus_gx_libretro.so", // pkg: libretro-genesisplusgx
            "bsnes_mercury_balanced_libretro.so", // pkg: …-bsnes-mercury-balanced
            "mupen64plus_next_libretro.so", // pkg: …-mupen64plus-next
            "mednafen_psx_hw_libretro.so", // pkg: …-beetle-psx
            "mednafen_psx_libretro.so",    // …which ships both renderers
            "mednafen_pce_fast_libretro.so", // pkg: …-beetle-pce-fast
            "mednafen_saturn_libretro.so", // pkg: …-beetle-saturn
            "pcsx2_libretro.so",           // pkg: …-lrps2
            "nekop2_libretro.so",          // pkg: …-np2
        ] {
            std::fs::write(dir.join(so), b"").unwrap();
        }

        let _guard = ROOT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: no other thread reads the variable while the lock is held.
        unsafe { std::env::set_var(LIBRETRO_DIR_ENV, dir) };

        let resolved = |name: &str| {
            PathBuf::from(resolve_core(name))
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };

        // Names that match outright.
        assert_eq!(resolved("mgba"), "mgba_libretro.so");
        assert_eq!(resolved("snes9x"), "snes9x_libretro.so");
        // Words run together in the package name.
        assert_eq!(resolved("genesisplusgx"), "genesis_plus_gx_libretro.so");
        // Hyphens in the package name, underscores on disk.
        assert_eq!(
            resolved("bsnes-mercury-balanced"),
            "bsnes_mercury_balanced_libretro.so"
        );
        assert_eq!(resolved("mupen64plus-next"), "mupen64plus_next_libretro.so");
        // Beetle is Mednafen under a different name.
        assert_eq!(resolved("beetle-psx"), "mednafen_psx_hw_libretro.so");
        assert_eq!(resolved("beetle-pce-fast"), "mednafen_pce_fast_libretro.so");
        assert_eq!(resolved("beetle-saturn"), "mednafen_saturn_libretro.so");
        // …and two that rename outright.
        assert_eq!(resolved("lrps2"), "pcsx2_libretro.so");
        assert_eq!(resolved("np2"), "nekop2_libretro.so");
        // beetle-psx ships both renderers; the software one keeps its own name.
        assert_eq!(resolved("mednafen-psx"), "mednafen_psx_libretro.so");
        // The on-disk spelling keeps working too.
        assert_eq!(resolved("mednafen_psx_hw"), "mednafen_psx_hw_libretro.so");
        assert_eq!(
            resolved("mupen64plus_next_libretro.so"),
            "mupen64plus_next_libretro.so"
        );

        // Unknown cores fall back to the bare filename for RetroArch to try.
        assert_eq!(resolve_core("nosuchcore"), "nosuchcore_libretro.so");

        unsafe { std::env::remove_var(LIBRETRO_DIR_ENV) };
    }

    #[test]
    fn core_filename_accepts_every_spelling() {
        assert_eq!(core_filename("mgba"), "mgba_libretro.so");
        assert_eq!(core_filename("mgba_libretro"), "mgba_libretro.so");
        assert_eq!(core_filename("mgba_libretro.so"), "mgba_libretro.so");
        // Normalization is idempotent -- a name that already looks right
        // comes back unchanged rather than growing another suffix.
        assert_eq!(core_filename("bsnes_libretro.so"), "bsnes_libretro.so");
    }

    #[test]
    fn paths_are_keyed_by_entry_id() {
        let content = PathBuf::from("/roms/game.gba");
        let a = paths_for(Some("pokemon-firered"), &content);
        let b = paths_for(Some("pokemon-leafgreen"), &content);
        assert_ne!(a.root, b.root);
        assert!(a.root.ends_with("pokemon-firered"));
        assert!(a.states.starts_with(&a.root));
        assert!(a.saves.starts_with(&a.root));
        assert!(a.config.starts_with(&a.root));
    }

    #[test]
    fn paths_fall_back_to_content_stem() {
        let content = PathBuf::from("/roms/game.gba");
        assert!(paths_for(None, &content).root.ends_with("game"));
    }

    /// RetroArch resolves these against its own working directory, not the
    /// daemon's, so a relative data dir would scatter saves.
    #[test]
    fn paths_are_absolute_even_from_a_relative_root() {
        let _guard = ROOT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: no other thread reads the variable while the lock is held.
        unsafe { std::env::set_var(RETROARCH_ROOT_ENV, "./dev-runtime/data/retroarch") };

        let paths = paths_for(Some("e"), Path::new("/roms/game.gba"));
        assert!(paths.root.is_absolute(), "root: {}", paths.root.display());
        assert!(paths.saves.is_absolute());
        assert!(paths.states.is_absolute());
        assert!(paths.config.is_absolute());

        unsafe { std::env::remove_var(RETROARCH_ROOT_ENV) };
    }

    #[test]
    fn path_key_is_sanitized() {
        let content = PathBuf::from("/roms/game.gba");
        let paths = paths_for(Some("../../etc/passwd"), &content);
        let key = paths
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(!key.contains('/'));
        assert!(!key.starts_with('.'));
    }

    #[test]
    fn fragment_disables_config_writeback() {
        // The whole point of --appendconfig is that the user's own config is
        // untouched; config_save_on_exit defaults to true and would undo that.
        let paths = paths_for(Some("e"), Path::new("/roms/game.gba"));
        let cfg = render_append_config(&paths, RetroarchSaveState::Auto, true);
        assert!(cfg.contains("config_save_on_exit = \"false\""));
    }

    #[test]
    fn fragment_reflects_save_state_mode() {
        let paths = paths_for(Some("e"), Path::new("/roms/game.gba"));

        let auto = render_append_config(&paths, RetroarchSaveState::Auto, true);
        assert!(auto.contains("savestate_auto_save = \"true\""));
        assert!(auto.contains("savestate_auto_load = \"true\""));

        let off = render_append_config(&paths, RetroarchSaveState::Off, true);
        assert!(off.contains("savestate_auto_save = \"false\""));
        assert!(off.contains("savestate_auto_load = \"false\""));
        // The in-game save is flushed either way -- "off" is about snapshots.
        assert!(off.contains("autosave_interval = \"10\""));
    }

    #[test]
    fn fragment_points_at_the_entrys_own_directories() {
        let paths = paths_for(Some("e"), Path::new("/roms/game.gba"));
        let cfg = render_append_config(&paths, RetroarchSaveState::Auto, true);
        assert!(cfg.contains(&format!(
            "savefile_directory = \"{}\"",
            paths.saves.display()
        )));
        assert!(cfg.contains(&format!(
            "savestate_directory = \"{}\"",
            paths.states.display()
        )));
    }

    #[test]
    fn fragment_honors_kiosk_toggle() {
        let paths = paths_for(Some("e"), Path::new("/roms/game.gba"));
        assert!(
            render_append_config(&paths, RetroarchSaveState::Auto, true)
                .contains("kiosk_mode_enable = \"true\"")
        );
        assert!(
            render_append_config(&paths, RetroarchSaveState::Auto, false)
                .contains("kiosk_mode_enable = \"false\"")
        );
    }

    #[test]
    fn argv_has_core_content_and_config() {
        let content = PathBuf::from("/roms/game.gba");
        let args = Vec::new();
        let spec = spec_for(&content, &args);
        let argv = build_argv(
            &spec,
            Path::new("/state/e/append.cfg"),
            "/cores/mgba_libretro.so",
            "/roms/game.gba",
        );
        assert_eq!(argv[0], "retroarch");
        assert_eq!(
            argv,
            vec![
                "retroarch",
                "--appendconfig",
                "/state/e/append.cfg",
                "-f",
                "-L",
                "/cores/mgba_libretro.so",
                "/roms/game.gba",
            ]
        );
    }

    #[test]
    fn entry_args_come_last_so_they_win() {
        let content = PathBuf::from("/roms/game.gba");
        let args = vec!["--verbose".to_string()];
        let spec = spec_for(&content, &args);
        let argv = build_argv(&spec, Path::new("/c.cfg"), "core.so", "/roms/game.gba");
        assert_eq!(argv.last().unwrap(), "--verbose");
    }

    #[test]
    fn prepare_writes_the_fragment_and_creates_dirs() {
        let _guard = ROOT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let scratch = tempfile::tempdir().expect("tempdir");
        // SAFETY: no other thread reads the variable while the lock is held.
        unsafe { std::env::set_var(RETROARCH_ROOT_ENV, scratch.path()) };

        let content = PathBuf::from("~/roms/game.gba");
        let args = Vec::new();
        let spec = spec_for(&content, &args);
        let launch = prepare(&spec, Some("my-game"), |s| s.replace('~', "/home/kid")).unwrap();

        assert!(launch.paths.states.is_dir());
        assert!(launch.paths.saves.is_dir());
        assert!(launch.paths.config.is_file());
        // Tilde expansion reached the content path, not just the argv.
        assert!(launch.argv.contains(&"/home/kid/roms/game.gba".to_string()));
        assert!(launch.paths.root.ends_with("my-game"));

        unsafe { std::env::remove_var(RETROARCH_ROOT_ENV) };
    }

    /// Resetting returns the console to its title screen; it must not touch
    /// the child's actual saved game.
    #[test]
    fn discarding_the_auto_state_keeps_the_in_game_save() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let _guard = ROOT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: no other thread reads the variable while the lock is held.
        unsafe { std::env::set_var(RETROARCH_ROOT_ENV, scratch.path()) };

        let paths = paths_for(Some("game"), Path::new("/roms/game.gba"));
        // RetroArch files states under a per-core subdirectory of the one we
        // give it, so seed both layouts.
        let core_dir = paths.states.join("mGBA");
        std::fs::create_dir_all(&core_dir).unwrap();
        std::fs::create_dir_all(&paths.saves).unwrap();
        std::fs::write(core_dir.join("game.state.auto"), b"state").unwrap();
        std::fs::write(core_dir.join("game.state.auto.png"), b"thumb").unwrap();
        std::fs::write(paths.states.join("flat.state.auto"), b"state").unwrap();
        // Numbered manual states and the in-game save must survive.
        std::fs::write(core_dir.join("game.state1"), b"slot").unwrap();
        std::fs::write(paths.saves.join("game.srm"), b"battery").unwrap();

        assert_eq!(discard_auto_state(&paths).unwrap(), 3);

        assert!(!core_dir.join("game.state.auto").exists());
        assert!(!core_dir.join("game.state.auto.png").exists());
        assert!(!paths.states.join("flat.state.auto").exists());
        assert!(
            core_dir.join("game.state1").exists(),
            "a manual save state is not resume state"
        );
        assert!(
            paths.saves.join("game.srm").exists(),
            "resetting the console must not wipe the cartridge"
        );

        // Idempotent: resetting twice in a row is not an error.
        assert_eq!(discard_auto_state(&paths).unwrap(), 0);

        unsafe { std::env::remove_var(RETROARCH_ROOT_ENV) };
    }

    #[test]
    fn discarding_state_that_was_never_written_is_not_an_error() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let _guard = ROOT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: no other thread reads the variable while the lock is held.
        unsafe { std::env::set_var(RETROARCH_ROOT_ENV, scratch.path()) };

        let paths = paths_for(Some("never-launched"), Path::new("/roms/game.gba"));
        assert_eq!(discard_auto_state(&paths).unwrap(), 0);

        unsafe { std::env::remove_var(RETROARCH_ROOT_ENV) };
    }

    #[test]
    fn cfg_quote_escapes_quotes_and_backslashes() {
        assert_eq!(cfg_quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(cfg_quote(r"a\b"), r#""a\\b""#);
    }
}
