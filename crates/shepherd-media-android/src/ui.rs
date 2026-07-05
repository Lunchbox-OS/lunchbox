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
        match shepherd_media_core::LibmpvPlayer::new(Quality::default().ytdl_format()) {
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
            if ui.button("➕ Add a library").clicked() {
                next = Some(Screen::AddLibrary);
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
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                next = Some(Screen::Switcher);
            }
            ui.heading("Settings");
            if ui.button("➕ Add library").clicked() {
                next = Some(Screen::AddLibrary);
            }
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
                        if ui.selectable_label(is_active, "active").clicked() {
                            pending = Some(Pending::SetActive(id.clone()));
                        }
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
                        caching_editors(ui, id, &mut entry.caching);
                    }

                    ui.horizontal(|ui| {
                        if ui.add_enabled(idx > 0, egui::Button::new("⬆")).clicked() {
                            pending = Some(Pending::MoveUp(id.clone()));
                        }
                        if ui
                            .add_enabled(idx + 1 < count, egui::Button::new("⬇"))
                            .clicked()
                        {
                            pending = Some(Pending::MoveDown(id.clone()));
                        }
                        if ui.button("🗑 Remove").clicked() {
                            pending = Some(Pending::Remove(id.clone()));
                        }
                    });
                });
            }
        });

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
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                next = Some(Screen::Settings);
            }
            ui.heading("Add library");
        });
        ui.separator();

        egui::Grid::new("add_library_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Id");
                ui.text_edit_singleline(&mut self.form.id);
                ui.end_row();

                ui.label("Label");
                ui.text_edit_singleline(&mut self.form.label);
                ui.end_row();

                ui.label("Source");
                egui::ComboBox::from_id_salt("add_kind")
                    .selected_text(self.form.kind.label())
                    .show_ui(ui, |ui| {
                        for kind in FormKind::ALL {
                            ui.selectable_value(&mut self.form.kind, kind, kind.label());
                        }
                    });
                ui.end_row();

                ui.label("Location");
                ui.add(
                    egui::TextEdit::singleline(&mut self.form.locator)
                        .hint_text(self.form.kind.locator_hint()),
                );
                ui.end_row();
            });

        ui.add_space(8.0);
        if ui.button("Add").clicked() {
            let entry = LibraryEntry {
                id: self.form.id.trim().to_string(),
                label: self.form.label.trim().to_string(),
                source: self
                    .form
                    .kind
                    .into_source(self.form.locator.trim().to_string()),
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

            let mut next = match self.screen.clone() {
                Screen::Switcher => self.switcher(ui),
                Screen::Settings => self.settings_screen(ui),
                Screen::AddLibrary => self.add_library_screen(ui),
                Screen::Grid(id) => self.grid_screen(ui, &id),
            };

            // Remote BACK navigates up the screen stack. Android delivers it as
            // BrowserBack; also accept Escape from a keyboard.
            let back = ui.input(|i| {
                i.key_pressed(egui::Key::BrowserBack) || i.key_pressed(egui::Key::Escape)
            });
            if next.is_none() && back {
                next = match self.screen {
                    Screen::Switcher => None,
                    Screen::Settings | Screen::Grid(_) => Some(Screen::Switcher),
                    Screen::AddLibrary => Some(Screen::Settings),
                };
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
fn caching_editors(ui: &mut egui::Ui, id: &str, caching: &mut CachingSettings) {
    ui.horizontal(|ui| {
        ui.label("Cache");
        egui::ComboBox::from_id_salt((id, "mode"))
            .selected_text(cache_mode_label(caching.mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut caching.mode, CacheMode::Off, "Off");
                ui.selectable_value(&mut caching.mode, CacheMode::QueueAfterPlay, "After play");
                ui.selectable_value(&mut caching.mode, CacheMode::QueueAll, "All");
            });

        ui.label("Quality");
        egui::ComboBox::from_id_salt((id, "quality"))
            .selected_text(caching.quality.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut caching.quality, Quality::Best, "Best");
                ui.selectable_value(&mut caching.quality, Quality::Q1080, "1080p");
                ui.selectable_value(&mut caching.quality, Quality::Q720, "720p");
                ui.selectable_value(&mut caching.quality, Quality::Q480, "480p");
            });
    });

    ui.horizontal(|ui| {
        ui.label("Posters");
        egui::ComboBox::from_id_salt((id, "posters"))
            .selected_text(poster_label(caching.posters))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut caching.posters, PosterPolicy::Always, "Always");
                ui.selectable_value(&mut caching.posters, PosterPolicy::WifiOnly, "Wi-Fi only");
                ui.selectable_value(&mut caching.posters, PosterPolicy::Never, "Never");
            });

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
    });
}

fn cache_mode_label(mode: CacheMode) -> &'static str {
    match mode {
        CacheMode::Off => "Off",
        CacheMode::QueueAfterPlay => "After play",
        CacheMode::QueueAll => "All",
    }
}


fn poster_label(p: PosterPolicy) -> &'static str {
    match p {
        PosterPolicy::Always => "Always",
        PosterPolicy::WifiOnly => "Wi-Fi only",
        PosterPolicy::Never => "Never",
    }
}
