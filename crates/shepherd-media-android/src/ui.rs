//! The egui application: a library switcher, a settings page for managing
//! libraries and their caching options, an add-library form, and a placeholder
//! browse grid.
//!
//! `MediaApp` is cross-platform `eframe::App` code so it can run on the host via
//! the `desktop_preview` example for fast iteration, and on Android via the
//! native-activity entry point in `lib.rs`. All mutations go through
//! `shepherd_media_app::AppSettings`; the app diffs the settings each frame and
//! persists to disk when they change.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use shepherd_media_app::{
    AppSettings, CacheMode, CachingSettings, LibraryEntry, LibrarySource, PosterPolicy, Quality,
};
use shepherd_media_core::{ItemKind, Library};

use crate::resolve::{ResolveError, resolve};

/// Which screen is currently shown.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    /// Pick a library to browse (the launch screen).
    Switcher,
    /// Manage libraries and their caching options.
    Settings,
    /// Form for adding a new library.
    AddLibrary,
    /// Browse a library's contents (placeholder until resolution/playback land).
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

/// The grid's current target library and its resolution state.
struct GridView {
    library_id: String,
    state: GridState,
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
    settings: AppSettings,
    screen: Screen,
    form: NewLibraryForm,
    /// Cached resolution for the browse grid (also acts as a 1-entry cache so
    /// re-opening the same library doesn't re-resolve).
    grid: Option<GridView>,
    /// Transient one-line status (errors, confirmations) shown in the top bar.
    status: Option<String>,
}

impl MediaApp {
    /// Build the app, loading persisted settings from `settings_path` (a missing
    /// file yields empty settings — the normal first-launch case).
    pub fn new(cc: &eframe::CreationContext<'_>, settings_path: PathBuf) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (settings, status) = match AppSettings::load(&settings_path) {
            Ok(s) => (s, None),
            Err(e) => (
                AppSettings::new(),
                Some(format!("Failed to load settings: {e}")),
            ),
        };
        Self {
            settings_path,
            settings,
            screen: Screen::Switcher,
            form: NewLibraryForm::default(),
            grid: None,
            status,
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
        ui.horizontal(|ui| {
            ui.heading("Libraries");
            if ui.button("⚙ Settings").clicked() {
                next = Some(Screen::Settings);
            }
        });
        ui.separator();

        if self.settings.libraries.is_empty() {
            ui.label("No libraries configured yet.");
            if ui.button("➕ Add a library").clicked() {
                next = Some(Screen::AddLibrary);
            }
            return next;
        }

        let active = self.settings.active_library.clone();
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
                if ui
                    .add(egui::Button::new(text).min_size(egui::vec2(240.0, 36.0)))
                    .clicked()
                {
                    // Selecting a library makes it active and opens its grid.
                    let _ = self.settings.set_active(&id);
                    next = Some(Screen::Grid(id));
                }
            }
        });
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

    fn grid_screen(&mut self, ui: &mut egui::Ui, library_id: &str) -> Option<Screen> {
        self.ensure_grid_loading(ui, library_id);
        self.poll_grid();

        let mut next = None;
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                next = Some(Screen::Switcher);
            }
            let title = self
                .settings
                .get(library_id)
                .map(|e| e.label.clone())
                .unwrap_or_else(|| library_id.to_string());
            ui.heading(title);
        });
        if let Some(entry) = self.settings.get(library_id) {
            ui.label(source_summary(&entry.source));
        }
        ui.separator();

        match self.grid.as_ref().map(|g| &g.state) {
            Some(GridState::Loading(_)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading…");
                });
            }
            Some(GridState::Failed(msg)) => {
                let color = ui.visuals().error_fg_color;
                ui.colored_label(color, msg);
            }
            Some(GridState::Loaded(lib)) => {
                ui.label(format!("{} item(s)", lib.items.len()));
                ui.add_space(8.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for item in &lib.items {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(item_kind_glyph(item.kind));
                                ui.strong(&item.title);
                            });
                            if let Some(category) = &item.category {
                                ui.label(format!("Category: {category}"));
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                ui.weak(
                    "Tapping an item to play needs the libmpv backend, which is \
                     the next step.",
                );
            }
            None => {}
        }
        next
    }
}

fn item_kind_glyph(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Video => "🎬",
        ItemKind::Audio => "🎵",
    }
}

impl eframe::App for MediaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Diff settings across the frame so any mutation persists automatically.
        let before = self.settings.clone();

        self.top_bar(ui);

        let next = match self.screen.clone() {
            Screen::Switcher => self.switcher(ui),
            Screen::Settings => self.settings_screen(ui),
            Screen::AddLibrary => self.add_library_screen(ui),
            Screen::Grid(id) => self.grid_screen(ui, &id),
        };

        if let Some(screen) = next {
            self.screen = screen;
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
            .selected_text(quality_label(caching.quality))
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

fn quality_label(q: Quality) -> &'static str {
    match q {
        Quality::Best => "Best",
        Quality::Q1080 => "1080p",
        Quality::Q720 => "720p",
        Quality::Q480 => "480p",
    }
}

fn poster_label(p: PosterPolicy) -> &'static str {
    match p {
        PosterPolicy::Always => "Always",
        PosterPolicy::WifiOnly => "Wi-Fi only",
        PosterPolicy::Never => "Never",
    }
}
