//! The egui application: a library switcher, a settings page for managing
//! libraries and their caching options, an add-library form, and a browse grid
//! (the shared `shepherd-media-ui` poster grid, same as the Linux binary).
//!
//! `MediaApp` is cross-platform `eframe::App` code so it can run on the host via
//! the `desktop_preview` example for fast iteration, and on Android via the
//! native-activity entry point in `lib.rs`. All mutations go through
//! `shepherd_media_app::AppSettings`; the app diffs the settings each frame and
//! persists to disk when they change.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use shepherd_media_app::{
    AppSettings, CacheMode, CachingSettings, LibraryEntry, LibrarySource, PosterPolicy, Quality,
};
use shepherd_media_core::{
    ClassifiedUri, Library, Platform, PlatformInfo, PlayerEvent, PlayerHandle, PosterRef, Source,
    resolve_source,
};
use shepherd_media_ui::grid;
use url::Url;

use crate::playback::PlaybackView;
use crate::resolve::{ResolveError, resolve};
use crate::video_cache::VideoCache;

/// The item currently playing.
struct PlayingItem {
    title: String,
    /// For a cacheable (direct-http) source whose library has caching enabled:
    /// the URL and the cache to download it into after playback finishes.
    cache: Option<(String, VideoCache)>,
}

/// Construct the playback backend for this platform: libmpv on Android, a no-op
/// stub elsewhere (so the playback path still compiles and runs on the host).
fn make_player() -> Option<Box<dyn PlayerHandle>> {
    #[cfg(target_os = "android")]
    {
        // `fast_render`: this app targets TVs with weak GPUs (e.g. Fire TV
        // sticks), where mpv's default GL render path can't keep up with the
        // display; the `fast` profile restores full-rate playback.
        match shepherd_media_core::LibmpvPlayer::new(Quality::default().ytdl_format(), true) {
            Ok(p) => Some(Box::new(p)),
            Err(e) => {
                log::error!("libmpv init failed: {e}");
                None
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        Some(Box::new(crate::player::StubPlayer::default()))
    }
}

/// Encoded poster bytes delivered from a worker thread to the UI thread. The
/// shared grid renders them via egui's image loader (egui_extras), so we pass
/// the raw bytes through rather than decoding to a texture ourselves.
type PosterMsg = (String, Option<Vec<u8>>);

/// Per-item poster state in the grid.
enum PosterSlot {
    Pending,
    Ready(Vec<u8>),
    Unavailable,
}

/// Which screen is currently shown.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    /// Pick a library to browse (the launch screen).
    Switcher,
    /// Manage libraries and their caching options.
    Settings,
    /// Form for adding a new library.
    AddLibrary,
    /// Browse on-device storage to pick a `.toml`/`.m3u` for the add form.
    FilePicker,
    /// Show a QR/URL so a phone can hand a library URL to the add form.
    PhoneHandoff,
    /// Browse a library's contents as a poster grid.
    Grid(String),
}

/// Source-kind selector for the add-library form, mirroring [`LibrarySource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormKind {
    SafToml,
    HttpToml,
    M3u,
    Youtube,
}

impl FormKind {
    const ALL: [FormKind; 4] = [
        FormKind::SafToml,
        FormKind::HttpToml,
        FormKind::M3u,
        FormKind::Youtube,
    ];

    fn label(self) -> &'static str {
        match self {
            FormKind::SafToml => "TOML file (device)",
            FormKind::HttpToml => "TOML file (URL)",
            FormKind::M3u => "M3U / M3U8 playlist",
            FormKind::Youtube => "YouTube playlist",
        }
    }

    fn locator_hint(self) -> &'static str {
        match self {
            FormKind::SafToml => "content:// or /path/to/library.toml",
            FormKind::HttpToml => "https://host/library.toml",
            FormKind::M3u => "content:// or /path/to/playlist.m3u",
            FormKind::Youtube => "https://www.youtube.com/playlist?list=…",
        }
    }

    fn into_source(self, locator: String) -> LibrarySource {
        match self {
            FormKind::SafToml => LibrarySource::SafToml { uri: locator },
            FormKind::HttpToml => LibrarySource::HttpToml { url: locator },
            FormKind::M3u => LibrarySource::M3u { uri: locator },
            FormKind::Youtube => LibrarySource::YoutubePlaylist { url: locator },
        }
    }
}

/// Resolution state for the browse grid. Resolution runs on a worker thread
/// (network must not touch the Android UI thread), so the grid holds either the
/// in-flight receiver, the resolved library, or an error.
enum GridState {
    Loading(Receiver<Result<Library, ResolveError>>),
    Loaded(Library),
    Failed(String),
}

/// The grid's current target library and its resolution state, plus the
/// browse focus/scroll state for the shared poster grid (reset per library).
struct GridView {
    library_id: String,
    state: GridState,
    /// Index of the focused tile, moved by D-pad / arrow keys.
    focused: usize,
    /// Columns the grid laid out last frame (set by `grid::draw`); used to step
    /// focus by a row.
    columns: usize,
    scroll: grid::ScrollState,
}

/// Mutable buffer behind the add-library form.
struct NewLibraryForm {
    id: String,
    label: String,
    kind: FormKind,
    locator: String,
}

impl Default for NewLibraryForm {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            kind: FormKind::SafToml,
            locator: String::new(),
        }
    }
}

pub struct MediaApp {
    settings_path: PathBuf,
    /// Base directory for on-disk caches (posters today, videos later).
    cache_dir: PathBuf,
    settings: AppSettings,
    screen: Screen,
    form: NewLibraryForm,
    /// Current directory shown by the file browser (`Screen::FilePicker`).
    picker_dir: PathBuf,
    /// The phone hand-off server (`Screen::PhoneHandoff`), started on first use
    /// and kept running so the URL/port stay stable for the session.
    handoff: Option<crate::handoff::Handoff>,
    /// Cached resolution for the browse grid (also acts as a 1-entry cache so
    /// re-opening the same library doesn't re-resolve).
    grid: Option<GridView>,
    /// Poster textures by item id, plus the channel workers deliver decoded
    /// posters on.
    posters: HashMap<String, PosterSlot>,
    poster_tx: Sender<PosterMsg>,
    poster_rx: Receiver<PosterMsg>,
    /// The playback backend (libmpv on Android), GL-bound once at startup, plus
    /// the view that composites it and the currently-playing item.
    player: Option<Box<dyn PlayerHandle>>,
    playback: Option<PlaybackView>,
    playing: Option<PlayingItem>,
    /// A YouTube item whose stream URLs are being resolved on a worker thread
    /// before playback can start: (display title, result receiver).
    playback_pending: Option<(String, Receiver<Result<crate::youtube::StreamUrls, String>>)>,
    /// Transient one-line status (errors, confirmations) shown in the top bar.
    status: Option<String>,
    /// Cached display safe-area insets (physical px: camera cutout + rounded
    /// corners) and the ctx time we last queried them. Refreshed about once a
    /// second so a rotation that moves the cutout to the other edge is picked up.
    safe_insets: crate::insets::SafeInsets,
    insets_checked_at: f64,
    /// Whether a text field held focus this frame (the add-library form). Lets
    /// the remote's BACK leave the field before it leaves the screen.
    text_field_focused: bool,
}

impl MediaApp {
    /// Build the app, loading persisted settings from `settings_path` (a missing
    /// file yields empty settings — the normal first-launch case). `cache_dir`
    /// is the base directory for on-disk caches.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings_path: PathBuf,
        cache_dir: PathBuf,
    ) -> Self {
        cc.egui_ctx.set_visuals(tv_visuals());
        // The shared poster grid renders thumbnails via egui's image widget,
        // which needs egui_extras' loaders installed on the context.
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (settings, status) = match AppSettings::load(&settings_path) {
            Ok(s) => (s, None),
            Err(e) => (
                AppSettings::new(),
                Some(format!("Failed to load settings: {e}")),
            ),
        };
        let (poster_tx, poster_rx) = std::sync::mpsc::channel();

        // Build the player and bind it to the host GL context. bind_gl must
        // happen here because the proc-address loader is only exposed on the
        // creation context.
        let needs_render = Arc::new(AtomicBool::new(false));
        let mut player = make_player();
        if let Some(p) = player.as_mut() {
            if let Some(get_proc) = cc.get_proc_address.as_ref()
                && let Err(e) = p.bind_gl(get_proc.as_ref())
            {
                log::error!("bind_gl failed: {e}");
            }
            let flag = needs_render.clone();
            let egui_ctx = cc.egui_ctx.clone();
            p.set_redraw_callback(Box::new(move || {
                flag.store(true, Ordering::Relaxed);
                egui_ctx.request_repaint();
            }));
        }
        let playback = cc
            .gl
            .as_ref()
            .map(|gl| PlaybackView::new(gl.clone(), needs_render.clone()));

        Self {
            settings_path,
            cache_dir,
            settings,
            screen: Screen::Switcher,
            form: NewLibraryForm::default(),
            picker_dir: crate::storage::browse_root(),
            handoff: None,
            grid: None,
            posters: HashMap::new(),
            poster_tx,
            poster_rx,
            player,
            playback,
            playing: None,
            playback_pending: None,
            status,
            safe_insets: crate::insets::SafeInsets::default(),
            // Force a query on the first frame.
            insets_checked_at: f64::NEG_INFINITY,
            text_field_focused: false,
        }
    }

    fn persist(&mut self) {
        if let Err(e) = self.settings.save(&self.settings_path) {
            self.status = Some(format!("Failed to save settings: {e}"));
        }
    }

    // --- screens -----------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("shepherd-media");
            ui.separator();
            if let Some(status) = &self.status {
                let color = ui.visuals().warn_fg_color;
                ui.colored_label(color, status);
            }
        });
        ui.separator();
    }

    /// Returns a screen to navigate to, if the user requested one.
    fn switcher(&mut self, ui: &mut egui::Ui) -> Option<Screen> {
        let mut next = None;
        // Single top row: app title (+ status) on the left, Settings on the right.
        let mut settings_resp = None;
        ui.horizontal(|ui| {
            ui.heading("shepherd-media");
            if let Some(status) = &self.status {
                ui.colored_label(ui.visuals().warn_fg_color, status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let resp = ui.button("⚙ Settings");
                if resp.clicked() {
                    next = Some(Screen::Settings);
                }
                settings_resp = Some(resp);
            });
        });
        ui.separator();

        if self.settings.libraries.is_empty() {
            ui.label("No libraries configured yet.");
            let resp = ui.button("➕ Add a library");
            if resp.clicked() {
                next = Some(Screen::AddLibrary);
            }
            // No library list to focus, so the switcher's usual "focus the first
            // library" bootstrap doesn't fire here. Focus the Add button instead,
            // or the remote's center button has nothing to activate and the first
            // library can never be added on a TV.
            if ui.ctx().memory(|m| m.focused().is_none()) {
                resp.request_focus();
            }
            return next;
        }

        // On entry, focus the first *library* (not Settings), so the remote lands
        // on content. Captured during the loop and requested after.
        let nothing_focused = ui.ctx().memory(|m| m.focused().is_none());
        let active = self.settings.active_library.clone();
        let mut first_lib: Option<egui::Response> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            // Snapshot ids so we can mutate `settings` (set_active) while iterating.
            let entries: Vec<(String, String)> = self
                .settings
                .libraries
                .iter()
                .map(|e| (e.id.clone(), e.label.clone()))
                .collect();
            for (id, label) in entries {
                let is_active = active.as_deref() == Some(id.as_str());
                let text = if is_active {
                    format!("▶ {label}")
                } else {
                    label
                };
                let resp = ui.add(egui::Button::new(text).min_size(egui::vec2(240.0, 36.0)));
                if first_lib.is_none() {
                    first_lib = Some(resp.clone());
                }
                if resp.clicked() {
                    // Selecting a library makes it active and opens its grid.
                    let _ = self.settings.set_active(&id);
                    next = Some(Screen::Grid(id));
                }
            }
        });
        if nothing_focused {
            if let Some(resp) = &first_lib {
                resp.request_focus();
            }
        } else if let (Some(s), Some(f)) = (&settings_resp, &first_lib) {
            // Bridge the top-right Settings button and the library column for the
            // D-pad: egui's spatial focus can't connect them (different rows), so
            // wire Up-from-first-library → Settings and Down-from-Settings → list.
            // Cancel egui's own pending move afterward, or it would step one
            // further from the widget we just focused.
            let to_focus = if f.has_focus() && ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                Some(s)
            } else if s.has_focus() && ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                Some(f)
            } else {
                None
            };
            if let Some(target) = to_focus {
                target.request_focus();
                ui.ctx()
                    .memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            }
        }
        next
    }

    fn settings_screen(&mut self, ui: &mut egui::Ui) -> Option<Screen> {
        let mut next = None;
        // Every focusable control, in visual order, for the D-pad focus stepping
        // at the end (egui's spatial focus skips the offset combo/field columns).
        let mut controls: Vec<egui::Response> = Vec::new();
        ui.horizontal(|ui| {
            let back = ui.button("⬅ Back");
            if back.clicked() {
                next = Some(Screen::Switcher);
            }
            controls.push(back);
            ui.heading("Settings");
            let add = ui.button("➕ Add library");
            if add.clicked() {
                next = Some(Screen::AddLibrary);
            }
            controls.push(add);
        });
        ui.separator();

        let ids: Vec<String> = self
            .settings
            .libraries
            .iter()
            .map(|e| e.id.clone())
            .collect();
        if ids.is_empty() {
            ui.label("No libraries configured yet.");
            return next;
        }

        // Deferred structural mutations so we don't reorder/remove mid-iteration.
        enum Pending {
            SetActive(String),
            Remove(String),
            MoveUp(String),
            MoveDown(String),
        }
        let mut pending: Option<Pending> = None;
        let active = self.settings.active_library.clone();
        let count = ids.len();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (idx, id) in ids.iter().enumerate() {
                let is_active = active.as_deref() == Some(id.as_str());
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        let act = ui.selectable_label(is_active, "active");
                        if act.clicked() {
                            pending = Some(Pending::SetActive(id.clone()));
                        }
                        controls.push(act);
                        let label = self
                            .settings
                            .get(id)
                            .map(|e| e.label.as_str())
                            .unwrap_or(id);
                        ui.strong(label);
                        ui.label(format!("({id})"));
                    });

                    if let Some(entry) = self.settings.get(id) {
                        ui.label(source_summary(&entry.source));
                    }

                    // Caching editors operate on a fresh mutable borrow.
                    if let Some(entry) = self.settings.get_mut(id) {
                        controls.extend(caching_editors(ui, id, &mut entry.caching));
                    }

                    ui.horizontal(|ui| {
                        let up = ui.add_enabled(idx > 0, egui::Button::new("⬆"));
                        if up.clicked() {
                            pending = Some(Pending::MoveUp(id.clone()));
                        }
                        // Disabled at the ends; don't leave a dead slot in the order.
                        if idx > 0 {
                            controls.push(up);
                        }
                        let down = ui.add_enabled(idx + 1 < count, egui::Button::new("⬇"));
                        if down.clicked() {
                            pending = Some(Pending::MoveDown(id.clone()));
                        }
                        if idx + 1 < count {
                            controls.push(down);
                        }
                        let remove = ui.button("🗑 Remove");
                        if remove.clicked() {
                            pending = Some(Pending::Remove(id.clone()));
                        }
                        controls.push(remove);
                    });
                });
            }
        });

        // Drive D-pad Up/Down through every control (the Limit drag value is left
        // out — it's reachable via Left/Right from Posters, and owns its own
        // arrow handling).
        let order: Vec<&egui::Response> = controls.iter().collect();
        tv_focus_step(ui, &order);

        match pending {
            Some(Pending::SetActive(id)) => {
                let _ = self.settings.set_active(&id);
            }
            Some(Pending::Remove(id)) => {
                let _ = self.settings.remove_library(&id);
            }
            Some(Pending::MoveUp(id)) => {
                if let Some(i) = self.settings.libraries.iter().position(|e| e.id == id) {
                    let _ = self.settings.move_library(&id, i.saturating_sub(1));
                }
            }
            Some(Pending::MoveDown(id)) => {
                if let Some(i) = self.settings.libraries.iter().position(|e| e.id == id) {
                    let _ = self.settings.move_library(&id, i + 1);
                }
            }
            None => {}
        }
        next
    }

    fn add_library_screen(&mut self, ui: &mut egui::Ui) -> Option<Screen> {
        let mut next = None;
        let mut back_resp = None;
        ui.horizontal(|ui| {
            let b = ui.button("⬅ Back");
            if b.clicked() {
                next = Some(Screen::Settings);
            }
            back_resp = Some(b);
            ui.heading("Add library");
        });
        ui.separator();

        let mut field_focused = false;
        let (mut id_resp, mut label_resp, mut combo_resp, mut location_resp) =
            (None, None, None, None);
        let kind_opts: Vec<(FormKind, &str)> =
            FormKind::ALL.iter().map(|&k| (k, k.label())).collect();
        egui::Grid::new("add_library_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Id");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.form.id)
                        .hint_text("optional — from source"),
                );
                field_focused |= tv_free_field_focus(ui, &r);
                id_resp = Some(r);
                ui.end_row();

                ui.label("Label");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.form.label)
                        .hint_text("optional — from source"),
                );
                field_focused |= tv_free_field_focus(ui, &r);
                label_resp = Some(r);
                ui.end_row();

                ui.label("Source");
                combo_resp = Some(tv_combo(ui, "add_kind", &mut self.form.kind, &kind_opts));
                ui.end_row();

                ui.label("Location");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.form.locator)
                        .hint_text(self.form.kind.locator_hint()),
                );
                field_focused |= tv_free_field_focus(ui, &r);
                location_resp = Some(r);
                ui.end_row();
            });
        self.text_field_focused = field_focused;

        // The one source-specific action button (browse / hand-off), tracked for
        // D-pad focus stepping below.
        let mut action_resp = None;

        // For an on-device source, offer a keyboard-free file browser instead of
        // typing the path. Needs "All files access" (API 30+); if it isn't
        // granted, send the user to settings and let them retry.
        if matches!(self.form.kind, FormKind::SafToml | FormKind::M3u) {
            ui.add_space(4.0);
            let b = ui.button("📁 Browse device…");
            if b.clicked() {
                if crate::storage::has_all_files_access() {
                    self.picker_dir = crate::storage::browse_root();
                    next = Some(Screen::FilePicker);
                } else {
                    crate::storage::request_all_files_access();
                    self.status = Some(
                        "Grant \u{201c}All files access\u{201d}, then tap Browse again.".into(),
                    );
                }
            }
            action_resp = Some(b);
        }

        // For a URL source, let a phone (which has a keyboard) hand the URL over
        // instead of typing it on the TV.
        if matches!(self.form.kind, FormKind::HttpToml | FormKind::Youtube) {
            ui.add_space(4.0);
            let b = ui.button("📱 Add from phone…");
            if b.clicked() {
                match self.ensure_handoff() {
                    Ok(()) => next = Some(Screen::PhoneHandoff),
                    Err(e) => self.status = Some(format!("Couldn't start hand-off: {e}")),
                }
            }
            action_resp = Some(b);
        }

        ui.add_space(8.0);
        let add_resp = ui.button("Add");

        // Let the D-pad step through every control in tab order (Left/Right still
        // do spatial moves / text-cursor edits).
        let order: Vec<&egui::Response> = [
            back_resp.as_ref(),
            id_resp.as_ref(),
            label_resp.as_ref(),
            combo_resp.as_ref(),
            location_resp.as_ref(),
            action_resp.as_ref(),
            Some(&add_resp),
        ]
        .into_iter()
        .flatten()
        .collect();
        tv_focus_step(ui, &order);

        if add_resp.clicked() {
            let source = self
                .form
                .kind
                .into_source(self.form.locator.trim().to_string());
            // Id and Label are optional: derive them from the source when left
            // blank so a remote-only user need not type them. A derived id is
            // de-duplicated against existing libraries; an explicitly typed
            // duplicate still surfaces an error.
            let typed_id = self.form.id.trim();
            let id = if typed_id.is_empty() {
                self.settings.unique_id(&source.suggested_id())
            } else {
                typed_id.to_string()
            };
            let typed_label = self.form.label.trim();
            let label = if typed_label.is_empty() {
                source.suggested_label()
            } else {
                typed_label.to_string()
            };
            let entry = LibraryEntry {
                id,
                label,
                source,
                caching: CachingSettings::default(),
            };
            match self.settings.add_library(entry) {
                Ok(()) => {
                    self.form = NewLibraryForm::default();
                    self.status = Some("Library added.".to_string());
                    next = Some(Screen::Settings);
                }
                Err(e) => self.status = Some(e.to_string()),
            }
        }
        next
    }

    /// Keyboard-free file browser: pick an on-device `.toml`/`.m3u` for the add
    /// form. A vertical column of buttons, so the D-pad steps through it and the
    /// first entry auto-focuses like the other screens. Picking a file drops its
    /// real path into the form (relative media then resolves against it).
    fn file_picker_screen(&mut self, ui: &mut egui::Ui) -> Option<Screen> {
        let mut next = None;
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                next = Some(Screen::AddLibrary);
            }
            ui.heading("Pick a file");
        });
        ui.label(self.picker_dir.display().to_string());
        ui.separator();

        // Snapshot the directory once (dirs first, then matching files, both
        // case-insensitive), so the button loop can mutate `picker_dir` freely.
        let mut dirs: Vec<(String, PathBuf)> = Vec::new();
        let mut files: Vec<(String, PathBuf)> = Vec::new();
        let mut read_err = None;
        match std::fs::read_dir(&self.picker_dir) {
            Ok(rd) => {
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    if is_dir {
                        dirs.push((name, entry.path()));
                    } else if is_library_file(&name) {
                        files.push((name, entry.path()));
                    }
                }
                dirs.sort_by_key(|a| a.0.to_lowercase());
                files.sort_by_key(|a| a.0.to_lowercase());
            }
            Err(e) => read_err = Some(e.to_string()),
        }

        if let Some(e) = read_err {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Cannot read this folder: {e}"),
            );
            if ui.button("Grant “All files access”").clicked() {
                crate::storage::request_all_files_access();
            }
        }

        let floor = std::path::Path::new(crate::storage::FLOOR);
        let size = egui::vec2(360.0, 34.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            // Up one level, as long as we stay at or below the floor.
            if self.picker_dir.as_path() != floor
                && let Some(parent) = self.picker_dir.parent()
                && parent.starts_with(floor)
                && ui.add(egui::Button::new("⬆ ..").min_size(size)).clicked()
            {
                self.picker_dir = parent.to_path_buf();
            }
            for (name, path) in &dirs {
                if ui
                    .add(egui::Button::new(format!("📁 {name}")).min_size(size))
                    .clicked()
                {
                    self.picker_dir = path.clone();
                }
            }
            for (name, path) in &files {
                if ui
                    .add(egui::Button::new(format!("🎬 {name}")).min_size(size))
                    .clicked()
                {
                    // Match the source kind to the extension so a playlist becomes
                    // an M3u source; the add form auto-derives id/label from it.
                    self.form.kind = if is_m3u(name) {
                        FormKind::M3u
                    } else {
                        FormKind::SafToml
                    };
                    self.form.locator = path.to_string_lossy().into_owned();
                    next = Some(Screen::AddLibrary);
                }
            }
            if dirs.is_empty() && files.is_empty() {
                ui.label("No folders or .toml/.m3u files here.");
            }
        });
        next
    }

    /// Start the phone hand-off server if it isn't already running (kept alive
    /// so the URL/port stay stable across visits to the screen).
    fn ensure_handoff(&mut self) -> std::io::Result<()> {
        if self.handoff.is_none() {
            self.handoff = Some(crate::handoff::start()?);
        }
        Ok(())
    }

    /// Show the hand-off URL + QR and poll for a URL submitted from the phone.
    /// When one arrives, fill the add form (source kind detected from the URL)
    /// and return to it.
    fn phone_handoff_screen(&mut self, ui: &mut egui::Ui) -> Option<Screen> {
        let mut next = None;
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                next = Some(Screen::AddLibrary);
            }
            ui.heading("Add from phone");
        });
        ui.separator();

        let Some(handoff) = self.handoff.as_ref() else {
            ui.label("Hand-off server isn't running.");
            return next;
        };

        // A URL arrived from the phone: fill the form and go back to it.
        if let Ok(url) = handoff.rx.try_recv() {
            let url = url.trim().to_string();
            self.form.kind = if is_youtube(&url) {
                FormKind::Youtube
            } else {
                FormKind::HttpToml
            };
            self.form.locator = url;
            self.status = Some("Received from phone.".to_string());
            // The submission arrives with no user input on the TV, so nothing
            // else would schedule the frame that paints the add form — request
            // it explicitly.
            ui.ctx().request_repaint();
            return Some(Screen::AddLibrary);
        }

        ui.add_space(8.0);
        // Landscape: instructions on the left, QR on the right sized to the
        // space that's actually left so it never runs off the bottom.
        ui.columns(2, |cols| {
            cols[0].label("On a phone on the same Wi-Fi, open this address:");
            cols[0].heading(&handoff.url);
            cols[0].add_space(12.0);
            cols[0].label("Paste a URL there and tap Send — it appears here automatically.");

            let ui = &mut cols[1];
            ui.label("or scan:");
            if let Some((w, dark)) = crate::handoff::qr_matrix(&handoff.url) {
                // Fit the QR within the column, both dimensions, with a margin.
                let max_side = (ui.available_height() - 8.0)
                    .min(ui.available_width())
                    .max(96.0);
                draw_qr(ui, max_side, w, &dark);
            }
        });

        // Poll for the submission while this screen is visible.
        ui.ctx().request_repaint_after(Duration::from_millis(200));
        next
    }

    /// Start resolving `library_id` on a worker thread unless the grid is
    /// already showing (or loading) it.
    fn ensure_grid_loading(&mut self, ui: &egui::Ui, library_id: &str) {
        let already = self
            .grid
            .as_ref()
            .is_some_and(|g| g.library_id == library_id);
        if already {
            return;
        }
        let state = match self.settings.get(library_id) {
            Some(entry) => {
                let (tx, rx) = std::sync::mpsc::channel();
                let source = entry.source.clone();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ = tx.send(resolve(&source));
                    ctx.request_repaint(); // wake the UI when the result lands
                });
                GridState::Loading(rx)
            }
            None => GridState::Failed("This library no longer exists.".to_string()),
        };
        self.grid = Some(GridView {
            library_id: library_id.to_string(),
            state,
            focused: 0,
            columns: 4,
            scroll: grid::ScrollState::default(),
        });
    }

    /// Advance a loading grid if its worker has produced a result.
    fn poll_grid(&mut self) {
        if let Some(GridView {
            state: GridState::Loading(rx),
            ..
        }) = self.grid.as_ref()
        {
            match rx.try_recv() {
                Ok(Ok(lib)) => self.set_grid_state(GridState::Loaded(lib)),
                Ok(Err(e)) => self.set_grid_state(GridState::Failed(e.to_string())),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.set_grid_state(GridState::Failed("resolver thread died".to_string()))
                }
            }
        }
    }

    fn set_grid_state(&mut self, state: GridState) {
        if let Some(g) = self.grid.as_mut() {
            g.state = state;
        }
    }

    /// Store any poster bytes that workers have finished fetching. egui's image
    /// loader decodes + uploads them on demand (keyed by the `bytes://` URI the
    /// grid builds), so we only hold the encoded bytes here.
    fn poll_posters(&mut self, _ctx: &egui::Context) {
        while let Ok((id, bytes)) = self.poster_rx.try_recv() {
            let slot = match bytes {
                Some(b) => PosterSlot::Ready(b),
                None => PosterSlot::Unavailable,
            };
            self.posters.insert(id, slot);
        }
    }

    /// Kick off poster loads for items in the loaded library that don't have a
    /// slot yet, honoring the library's poster policy.
    fn prefetch_posters(&mut self, ui: &egui::Ui, library_id: &str) {
        let load = self
            .settings
            .get(library_id)
            .map(|e| crate::posters::should_load(e.caching.posters))
            .unwrap_or(false);

        let pending: Vec<(String, Option<PosterRef>)> = if let Some(GridView {
            state: GridState::Loaded(lib),
            ..
        }) = &self.grid
        {
            lib.items
                .iter()
                .filter(|it| !self.posters.contains_key(&it.id))
                .map(|it| (it.id.clone(), it.poster.clone()))
                .collect()
        } else {
            Vec::new()
        };

        let cache = crate::posters::PosterCache::new(self.cache_dir.join("posters"));
        for (id, poster) in pending {
            match poster {
                Some(poster) if load => {
                    self.posters.insert(id.clone(), PosterSlot::Pending);
                    let tx = self.poster_tx.clone();
                    let ctx = ui.ctx().clone();
                    let cache = cache.clone();
                    std::thread::spawn(move || {
                        let bytes = cache.load(&poster);
                        let _ = tx.send((id, bytes));
                        ctx.request_repaint();
                    });
                }
                // No poster, or policy says don't load: mark resolved so we
                // fall back to the kind glyph and don't reconsider it.
                _ => {
                    self.posters.insert(id, PosterSlot::Unavailable);
                }
            }
        }
    }

    fn grid_screen(&mut self, ui: &mut egui::Ui, library_id: &str) -> Option<Screen> {
        self.ensure_grid_loading(ui, library_id);
        self.poll_grid();
        self.poll_posters(ui.ctx());
        self.prefetch_posters(ui, library_id);

        let title = self
            .settings
            .get(library_id)
            .map(|e| e.label.clone())
            .unwrap_or_else(|| library_id.to_string());

        // Loading / failed render simply; a loaded library uses the shared
        // poster grid (the same view as the Linux binary).
        let loaded = matches!(
            self.grid.as_ref().map(|g| &g.state),
            Some(GridState::Loaded(_))
        );
        if !loaded {
            match self.grid.as_ref().map(|g| &g.state) {
                Some(GridState::Loading(_)) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Loading {title}…"));
                    });
                }
                Some(GridState::Failed(msg)) => {
                    let color = ui.visuals().error_fg_color;
                    ui.colored_label(color, msg.clone());
                }
                _ => {}
            }
            return None;
        }

        // Move the focus index with the D-pad / arrow keys (the grid tiles are
        // custom-painted, so they don't use egui's own focus), then draw. The
        // center button (Enter) and a tap both select the focused/clicked item.
        let mut selected: Option<String> = None;
        {
            let posters = &self.posters;
            if let Some(g) = self.grid.as_mut()
                && let GridState::Loaded(lib) = &g.state
            {
                let n = lib.items.len();
                if n > 0 {
                    let cols = g.columns.max(1);
                    ui.input(|i| {
                        if i.key_pressed(egui::Key::ArrowRight) {
                            g.focused = (g.focused + 1).min(n - 1);
                        }
                        if i.key_pressed(egui::Key::ArrowLeft) {
                            g.focused = g.focused.saturating_sub(1);
                        }
                        if i.key_pressed(egui::Key::ArrowDown) {
                            g.focused = (g.focused + cols).min(n - 1);
                        }
                        if i.key_pressed(egui::Key::ArrowUp) {
                            g.focused = g.focused.saturating_sub(cols);
                        }
                    });
                    g.focused = g.focused.min(n - 1);
                    if ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && let Some(item) = lib.items.get(g.focused)
                        && resolve_source(item, &PlatformInfo::current()).is_some()
                    {
                        selected = Some(item.id.clone());
                    }
                    let clicked = grid::draw(
                        ui,
                        &mut g.scroll,
                        &title,
                        &lib.items,
                        &mut g.focused,
                        &mut g.columns,
                        &|id| match posters.get(id) {
                            Some(PosterSlot::Ready(b)) => Some(b.clone()),
                            _ => None,
                        },
                    );
                    if clicked.is_some() {
                        selected = clicked;
                    }
                }
            }
        }
        if let Some(id) = selected {
            self.start_playback(ui.ctx(), &id);
        }
        None
    }

    /// Resolve the platform source for `item_id` in the loaded library, prefer a
    /// cached local copy, and hand it to the player. Sets the playing item on
    /// success.
    fn start_playback(&mut self, ctx: &egui::Context, item_id: &str) {
        // Phase 1: gather what we need under shared borrows of self (grid +
        // settings), recording any status message to apply afterwards.
        let mut deferred_status: Option<String> = None;
        let prepared = (|| {
            let g = self.grid.as_ref()?;
            let GridState::Loaded(lib) = &g.state else {
                return None;
            };
            let item = lib.items.iter().find(|it| it.id == item_id)?;
            let info = PlatformInfo::current();
            let source = match resolve_source(item, &info) {
                Some(s) => s.clone(),
                None => {
                    deferred_status =
                        Some(format!("No source for `{}` on this platform.", item.title));
                    return None;
                }
            };
            let caching = self
                .settings
                .get(&g.library_id)
                .map(|e| e.caching.clone())
                .unwrap_or_default();
            Some((g.library_id.clone(), item.title.clone(), source, caching))
        })();
        if let Some(s) = deferred_status {
            self.status = Some(s);
        }
        let Some((library_id, title, source, caching)) = prepared else {
            return;
        };

        // YouTube: resolve the stream URL on a worker before playback (network
        // must not run on the UI thread, and there's no yt-dlp on PATH for
        // mpv's own ytdl hook to use).
        if let ClassifiedUri::YouTube(watch) = &source.uri {
            let watch = watch.to_string();
            let quality = caching.quality;
            let ctx = ctx.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = match crate::youtube::provider() {
                    Some(p) => crate::youtube::resolve_stream_url(p.as_ref(), &watch, quality),
                    None => Err("YouTube playback isn't available on this platform.".to_string()),
                };
                let _ = tx.send(result);
                ctx.request_repaint();
            });
            self.playback_pending = Some((title, rx));
            return;
        }

        // Phase 2: consult the video cache and start playback.
        let mut play_source = source.clone();
        let mut playing_cache = None;
        if let ClassifiedUri::DirectHttp(url) = &source.uri {
            let url = url.to_string();
            let cache = VideoCache::new(
                self.cache_dir.join("videos").join(&library_id),
                caching.max_bytes,
            );
            if let Some(path) = cache.cached_path(&url) {
                play_source = Source {
                    platforms: source.platforms.clone(),
                    uri: ClassifiedUri::Local(path),
                    player_hint: source.player_hint,
                };
            }
            if caching.mode != CacheMode::Off {
                playing_cache = Some((url, cache));
            }
        }

        match self.player.as_mut() {
            Some(p) => match p.play(&play_source) {
                Ok(()) => {
                    if let Some(pv) = self.playback.as_mut() {
                        pv.note_started();
                    }
                    self.playing = Some(PlayingItem {
                        title,
                        cache: playing_cache,
                    });
                }
                Err(e) => self.status = Some(format!("Playback failed: {e}")),
            },
            None => self.status = Some("No player available on this platform.".to_string()),
        }
    }

    /// Drive playback for a frame: drain player events (ending playback on
    /// EOF/close/error) and composite the video + overlay. Returns to the grid
    /// when playback ends or the user leaves.
    fn run_playback(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Drain events without holding a borrow across the mutation.
        let mut ended = false;
        if let Some(p) = self.player.as_mut() {
            while let Some(ev) = p.poll_event() {
                match ev {
                    PlayerEvent::EndOfFile | PlayerEvent::Closed | PlayerEvent::Error(_) => {
                        ended = true;
                    }
                    PlayerEvent::Started => {}
                }
            }
        }
        if ended {
            if let Some(p) = self.player.as_mut() {
                let _ = p.stop();
            }
            // Download-after-play: cache the just-finished item so the next play
            // is local. Runs on a worker (download + LRU eviction are blocking).
            if let Some(item) = self.playing.take()
                && let Some((url, cache)) = item.cache
            {
                std::thread::spawn(move || {
                    if let Err(e) = cache.store(&url) {
                        log::warn!("video cache store failed: {e}");
                    }
                });
            }
            return;
        }

        let title = self
            .playing
            .as_ref()
            .map(|x| x.title.clone())
            .unwrap_or_default();
        let leave = match (self.playback.as_mut(), self.player.as_mut()) {
            (Some(pv), Some(p)) => pv.draw(ui, frame, p.as_mut(), &title),
            _ => {
                ui.label("No player available on this platform.");
                ui.button("⬅ Back").clicked()
            }
        };
        if leave {
            if let Some(p) = self.player.as_mut() {
                let _ = p.stop();
            }
            self.playing = None;
        }
    }

    /// If a pending YouTube stream resolution has completed, start (or fail)
    /// playback with the resolved direct URL.
    fn poll_playback_pending(&mut self) {
        let Some((_, rx)) = self.playback_pending.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("YouTube resolver thread died.".to_string()),
        };
        let (title, _) = self.playback_pending.take().unwrap();
        let streams = match result {
            Ok(u) => u,
            Err(e) => {
                self.status = Some(e);
                return;
            }
        };
        let url = match Url::parse(&streams.video) {
            Ok(u) => u,
            Err(e) => {
                self.status = Some(format!("Bad stream URL: {e}"));
                return;
            }
        };
        let src = Source {
            platforms: vec![Platform::Any],
            uri: ClassifiedUri::DirectHttp(url),
            player_hint: None,
        };
        match self.player.as_mut() {
            Some(p) => {
                // YouTube DASH gives separate tracks: attach the audio URL as an
                // external track so the video-only stream plays with sound.
                p.set_external_audio(streams.audio);
                match p.play(&src) {
                    Ok(()) => {
                        if let Some(pv) = self.playback.as_mut() {
                            pv.note_started();
                        }
                        self.playing = Some(PlayingItem { title, cache: None });
                    }
                    Err(e) => self.status = Some(format!("Playback failed: {e}")),
                }
            }
            None => self.status = Some("No player available on this platform.".to_string()),
        }
    }
}

/// Dark theme tuned for 10-foot D-pad use (Fire TV / Google TV): a focused
/// widget renders with `widgets.active`, so make that an unmistakable highlight.
fn tv_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    let focus = egui::Color32::from_rgb(70, 140, 240);
    let w = &mut visuals.widgets;
    w.active.bg_fill = focus;
    w.active.weak_bg_fill = focus;
    w.active.bg_stroke = egui::Stroke::new(3.0, egui::Color32::WHITE);
    w.active.fg_stroke = egui::Stroke::new(2.0, egui::Color32::WHITE);
    w.active.expansion = 3.0;
    visuals
}

impl eframe::App for MediaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Diff settings across the frame so any mutation persists automatically.
        let before = self.settings.clone();

        // Promote a finished YouTube resolution into active playback.
        self.poll_playback_pending();

        // Playback takes over the whole surface while an item is playing and
        // fills it edge-to-edge (the video is composited full-screen), so it is
        // deliberately NOT inset. Only the browse/settings screens below are
        // inset, so navigation clears the camera cutout and rounded corners
        // while video still uses the entire display as it did before.
        if self.playing.is_some() {
            self.run_playback(ui, frame);
            if self.settings != before {
                self.persist();
            }
            return;
        }

        // Non-playback screens: inset content into the display's safe rectangle.
        // eframe's background clear already fills the whole non-rectangular
        // display, so the background stays edge-to-edge while content stays
        // clear of the cutout and rounded corners. Insets refresh ~once a second.
        let now = ui.ctx().input(|i| i.time);
        if now - self.insets_checked_at > 1.0 {
            self.safe_insets = crate::insets::query();
            self.insets_checked_at = now;
        }
        let ppp = ui.ctx().pixels_per_point().max(0.01);
        let ins = self.safe_insets;
        let full = ui.max_rect();
        let safe = egui::Rect::from_min_max(
            full.min + egui::vec2(ins.left / ppp, ins.top / ppp),
            full.max - egui::vec2(ins.right / ppp, ins.bottom / ppp),
        );
        // Guard against pathological insets swallowing the whole window.
        let safe = if safe.width() > 1.0 && safe.height() > 1.0 {
            safe
        } else {
            full
        };
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(safe).layout(*ui.layout()));
        let ui = &mut content;

        if self.playback_pending.is_some() {
            self.top_bar(ui);
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.spinner();
                ui.label("Resolving YouTube stream…");
            });
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else {
            // The switcher draws its own collapsed header (title + Settings); the
            // other screens get the shared top bar.
            if !matches!(self.screen, Screen::Switcher) {
                self.top_bar(ui);
            }

            // D-pad / remote support (Fire TV, Google TV): egui moves focus with
            // the arrow keys (Android maps the D-pad to them) but can't bootstrap
            // focus from nothing, so keep one widget focused at all times. This
            // also auto-focuses the first control when a screen appears.
            // DPAD_CENTER arrives as Enter, which activates the focused widget.
            // The browse grid manages its own focus by index (custom-painted
            // tiles), and the switcher focuses its first library explicitly, so
            // only bootstrap egui focus on the remaining screens.
            if !matches!(self.screen, Screen::Grid(_) | Screen::Switcher)
                && ui.ctx().memory(|m| m.focused().is_none())
            {
                ui.ctx()
                    .memory_mut(|m| m.move_focus(egui::FocusDirection::Next));
            }

            // Snapshot popup state before the screen renders: egui closes popups
            // on Escape during rendering, so checking afterward would miss a
            // just-closed one and let BACK also navigate. Android sends BACK as
            // BrowserBack (not Escape), so egui leaves the popup open and we
            // dismiss it ourselves below.
            let popup_was_open = egui::Popup::is_any_open(ui.ctx());

            // Only the add-library form has text fields; clear the flag so it
            // never lingers true on a screen that can't have a focused field.
            self.text_field_focused = false;
            let mut next = match self.screen.clone() {
                Screen::Switcher => self.switcher(ui),
                Screen::Settings => self.settings_screen(ui),
                Screen::AddLibrary => self.add_library_screen(ui),
                Screen::FilePicker => self.file_picker_screen(ui),
                Screen::PhoneHandoff => self.phone_handoff_screen(ui),
                Screen::Grid(id) => self.grid_screen(ui, &id),
            };

            // Remote BACK navigates up the screen stack. Android delivers it as
            // BrowserBack; also accept Escape from a keyboard.
            let back = ui.input(|i| {
                i.key_pressed(egui::Key::BrowserBack) || i.key_pressed(egui::Key::Escape)
            });
            if next.is_none() && back && popup_was_open {
                // BACK first dismisses an open popup (the Source dropdown) rather
                // than leaving the screen behind it.
                egui::Popup::close_all(ui.ctx());
            } else if next.is_none() && back && self.text_field_focused {
                // BACK from inside a text field leaves the field, not the screen;
                // a second BACK then navigates up as usual. Focus falls back to
                // the screen's first control on the next frame's bootstrap.
                ui.ctx().memory_mut(|m| m.stop_text_input());
            } else if next.is_none() && back {
                match self.screen {
                    // Top of the stack: BACK exits the app, matching the TV
                    // expectation that BACK from the home screen leaves rather
                    // than doing nothing. Finishing the activity over JNI is
                    // deterministic; winit's ViewportCommand::Close doesn't
                    // reliably end a NativeActivity.
                    Screen::Switcher => crate::exit::finish(),
                    Screen::Settings | Screen::Grid(_) => next = Some(Screen::Switcher),
                    Screen::AddLibrary => next = Some(Screen::Settings),
                    Screen::FilePicker | Screen::PhoneHandoff => next = Some(Screen::AddLibrary),
                }
            }

            if let Some(screen) = next {
                self.screen = screen;
            }
        }

        if self.settings != before {
            self.persist();
        }
    }
}

/// Stop a focused single-line text field from swallowing the vertical D-pad.
///
/// A focused `TextEdit` locks the arrow keys for cursor movement, so on a remote
/// (which has no other way to move focus) the field becomes a trap. Text here is
/// entered via the phone hand-off or the file browser, not the remote, so free
/// the *vertical* arrows: the field no longer consumes Up/Down, leaving them for
/// the form's explicit tab-order stepping (see `add_library_screen`). Horizontal
/// arrows stay with the cursor (harmless on a remote, still useful when editing
/// in the desktop preview). Overriding the lock filter after the widget runs wins
/// for this frame's end-of-frame focus handling. Returns whether the field is
/// focused, so the caller can make BACK leave the field, not the screen.
fn tv_free_field_focus(ui: &egui::Ui, resp: &egui::Response) -> bool {
    if resp.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: false,
                    escape: false,
                },
            );
        });
    }
    resp.has_focus()
}

/// A D-pad-friendly [`egui::ComboBox`] over a `(value, label)` list.
///
/// egui only closes a combo popup on a pointer click or Escape, so a remote's
/// Enter picks an option but leaves the popup open, and Android's BACK (delivered
/// as `BrowserBack`, not Escape) can't dismiss it. This wrapper detects the pick,
/// closes the popup, and keeps focus on the combo instead of letting it drop to
/// the first widget. BACK-dismisses-while-open is handled once, globally, in the
/// update loop's back handler. Returns the combo button response (for focus
/// stepping). `current` is updated in place.
fn tv_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    current: &mut T,
    options: &[(T, &str)],
) -> egui::Response {
    let selected_text = options
        .iter()
        .find(|(v, _)| *v == *current)
        .map(|(_, label)| *label)
        .unwrap_or_default();
    let before = *current;
    let resp = egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for (value, label) in options {
                ui.selectable_value(current, *value, *label);
            }
        })
        .response;
    if *current != before {
        egui::Popup::close_all(ui.ctx());
        resp.request_focus();
    }
    resp
}

/// Move focus through `order` (in tab order) on a D-pad Up/Down press, wrapping
/// at the ends.
///
/// egui's spatial focus navigation picks the geometrically nearest widget, which
/// on these forms skips controls in an offset column (e.g. the add-library fields
/// next to the left-column buttons), leaving them unreachable by remote. Driving
/// focus explicitly guarantees every control is reachable. No-op while a popup is
/// open, so Up/Down then move through the popup's own options instead. Caller
/// builds `order` from the frame's control responses (necessarily imperative,
/// given egui).
fn tv_focus_step(ui: &egui::Ui, order: &[&egui::Response]) {
    if egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let step = ui.input(|i| {
        i.key_pressed(egui::Key::ArrowDown) as i32 - i.key_pressed(egui::Key::ArrowUp) as i32
    });
    if step != 0
        && let Some(cur) = order.iter().position(|r| r.has_focus())
    {
        let n = order.len() as i32;
        let target = order[(((cur as i32 + step) % n + n) % n) as usize];
        target.request_focus();
        // Cancel egui's own pending spatial move so it doesn't step further.
        ui.ctx()
            .memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    }
}

/// Whether `url` points at a YouTube host (so a phone-submitted URL becomes a
/// YouTube playlist source rather than a plain HTTP TOML).
fn is_youtube(url: &str) -> bool {
    let u = url.to_lowercase();
    ["youtube.com", "youtu.be", "youtube-nocookie.com"]
        .iter()
        .any(|host| u.contains(host))
}

/// Paint a QR code (`width` × `width` modules, row-major `dark` flags) as black
/// squares on white, with a 4-module quiet zone, fitting within `max_side`
/// points (whole-pixel modules, so it stays crisp).
fn draw_qr(ui: &mut egui::Ui, max_side: f32, width: usize, dark: &[bool]) {
    let quiet = 4usize;
    let modules = width + quiet * 2;
    let module_px = (max_side / modules as f32).floor().max(1.0);
    let side = module_px * modules as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
    for y in 0..width {
        for x in 0..width {
            if dark[y * width + x] {
                let min = rect.min
                    + egui::vec2(
                        (x + quiet) as f32 * module_px,
                        (y + quiet) as f32 * module_px,
                    );
                painter.rect_filled(
                    egui::Rect::from_min_size(min, egui::vec2(module_px, module_px)),
                    0.0,
                    egui::Color32::BLACK,
                );
            }
        }
    }
}

/// Whether `name` is an M3U/M3U8 playlist file (by extension).
fn is_m3u(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".m3u") || lower.ends_with(".m3u8")
}

/// Whether `name` is a library file the browser should offer (`.toml` or an
/// M3U/M3U8 playlist).
fn is_library_file(name: &str) -> bool {
    name.to_lowercase().ends_with(".toml") || is_m3u(name)
}

/// One-line human description of where a library comes from.
fn source_summary(source: &LibrarySource) -> String {
    let (kind, locator) = match source {
        LibrarySource::SafToml { uri } => ("TOML (device)", uri.as_str()),
        LibrarySource::HttpToml { url } => ("TOML (URL)", url.as_str()),
        LibrarySource::M3u { uri } => ("M3U", uri.as_str()),
        LibrarySource::YoutubePlaylist { url } => ("YouTube playlist", url.as_str()),
    };
    format!("{kind}: {locator}")
}

/// Caching/quality combo boxes and the cache-size control for one library.
/// Returns the combo responses (Cache, Quality, Posters) so the caller can wire
/// them into the screen's D-pad focus order.
fn caching_editors(
    ui: &mut egui::Ui,
    id: &str,
    caching: &mut CachingSettings,
) -> [egui::Response; 3] {
    let (cache_resp, quality_resp) = ui
        .horizontal(|ui| {
            ui.label("Cache");
            let cache = tv_combo(
                ui,
                (id, "mode"),
                &mut caching.mode,
                &[
                    (CacheMode::Off, "Off"),
                    (CacheMode::QueueAfterPlay, "After play"),
                    (CacheMode::QueueAll, "All"),
                ],
            );

            ui.label("Quality");
            let quality = tv_combo(
                ui,
                (id, "quality"),
                &mut caching.quality,
                &[
                    (Quality::Best, "Best"),
                    (Quality::Q1080, "1080p"),
                    (Quality::Q720, "720p"),
                    (Quality::Q480, "480p"),
                ],
            );
            (cache, quality)
        })
        .inner;

    let posters_resp = ui
        .horizontal(|ui| {
            ui.label("Posters");
            let posters = tv_combo(
                ui,
                (id, "posters"),
                &mut caching.posters,
                &[
                    (PosterPolicy::Always, "Always"),
                    (PosterPolicy::WifiOnly, "Wi-Fi only"),
                    (PosterPolicy::Never, "Never"),
                ],
            );

            ui.label("Limit");
            let mut mib = caching.max_bytes / (1024 * 1024);
            if ui
                .add(
                    egui::DragValue::new(&mut mib)
                        .range(0..=1_048_576)
                        .suffix(" MiB"),
                )
                .changed()
            {
                caching.max_bytes = mib * 1024 * 1024;
            }
            posters
        })
        .inner;

    [cache_resp, quality_resp, posters_resp]
}
