//! The egui application: a library switcher, a settings page for managing
//! libraries and their caching options, an add-library form, and the library
//! screen (the shared `lunchbox-media-ui` view, same as the Linux binary).
//!
//! `MediaApp` is cross-platform `eframe::App` code so it can run on the host via
//! the `desktop_preview` example for fast iteration, and on Android via the
//! native-activity entry point in `lib.rs`. All mutations go through
//! `lunchbox_media_app::AppSettings`; the app diffs the settings each frame and
//! persists to disk when they change.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use lunchbox_media_app::{
    AppSettings, CacheMode, CachingSettings, LibraryEntry, LibrarySource, PosterPolicy, Quality,
    ResumeStore, ResumeTracker,
};
use lunchbox_media_core::{
    ClassifiedUri, Library, Platform, PlatformInfo, PlayerEvent, PlayerHandle, PosterRef,
    RetryBudget, Source, resolve_source,
};
use lunchbox_media_ui::library::{self, Hero, LibraryView};
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
    /// The resolved source and its external audio (YouTube DASH), kept so a
    /// transient playback error can be retried without re-resolving.
    source: Source,
    external_audio: Option<String>,
    /// Bounded restart budget for this item after a transient playback error.
    retries: RetryBudget,
}

/// A YouTube item whose stream URLs are being resolved on a worker thread
/// before playback can start.
struct PendingPlayback {
    /// Library item id, carried through so the resume bookkeeping can key on it
    /// once playback actually starts.
    item_id: String,
    title: String,
    watch: String,
    rx: Receiver<StreamResult>,
}

/// How long a focused grid item must stay focused before its stream is
/// prefetched, so quickly scrolling past items doesn't kick off resolves.
const PREFETCH_DWELL: Duration = Duration::from_millis(400);

/// How long a resolved YouTube stream stays usable from the cache. googlevideo
/// URLs embed a multi-hour expiry; keep this comfortably under it.
const STREAM_CACHE_TTL: Duration = Duration::from_secs(4 * 3600);

/// After every playlist is resolved, warm the stream URLs of this many leading
/// items in each so the first videos a user is likely to pick start instantly.
const PREFETCH_FIRST_N: usize = 10;

/// The single in-flight background resolve driven by `drive_prefetch`.
enum Prefetch {
    /// Resolving a library's contents (playlist → items).
    Library {
        id: String,
        rx: Receiver<Result<Library, ResolveError>>,
    },
    /// Resolving a YouTube item's stream URLs.
    Video {
        watch: String,
        rx: Receiver<StreamResult>,
    },
}

/// Construct the playback backend for this platform: libmpv on Android, a no-op
/// stub elsewhere (so the playback path still compiles and runs on the host).
fn make_player() -> Option<Box<dyn PlayerHandle>> {
    #[cfg(target_os = "android")]
    {
        // `fast_render` keeps mpv's scaling cheap on weak TV GPUs. It matters
        // less now that frames go straight to a Surface — the display hardware
        // does the scaling — but costs nothing to keep.
        //
        // `AndroidSurface` is what makes `hwdec=mediacodec` (not `-copy`)
        // reachable. The Surface itself is attached per playback (see
        // `play_with_surface`), since it does not exist yet at this point.
        match lunchbox_media_core::LibmpvPlayer::new(
            Quality::default().ytdl_format(),
            true,
            lunchbox_media_core::VideoOutput::AndroidSurface,
        ) {
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

/// Start playback of `source`, with the video Surface attached first.
///
/// Every play must go through here. `vo=mediacodec_embed` does not tolerate a
/// missing window — it asserts and aborts the process — and playback starts
/// from three places: a direct play, a resolved YouTube stream, and a retry
/// after a transient error. Attaching in any one of them leaves the other two
/// crashing.
fn play_with_surface(player: &mut dyn PlayerHandle, source: &Source) -> Result<(), String> {
    crate::surface::attach(player)?;
    player.play(source).map_err(|e| e.to_string())
}

/// Encoded poster bytes delivered from a worker thread to the UI thread. The
/// shared grid renders them via egui's image loader (egui_extras), so we pass
/// the raw bytes through rather than decoding to a texture ourselves.
type PosterMsg = (String, Option<Vec<u8>>);

/// The outcome of a YouTube stream resolution delivered from a worker thread:
/// the resolved stream URLs, or an error message to show.
type StreamResult = Result<crate::youtube::StreamUrls, String>;

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
/// library screen's focus and scroll (reset per library).
struct GridView {
    library_id: String,
    state: GridState,
    /// The shared library screen, which owns its focus (moved by the D-pad,
    /// which arrives as arrow keys) and its scroll.
    view: LibraryView,
    /// Saved playback positions for this library, attached once its contents
    /// resolve. `None` when its "Resume playback" option is off — which is what
    /// turns the whole feature off: nothing is recorded and nothing offered.
    resume: Option<ResumeTracker>,
    /// Whether the per-library state (resume, and the SponsorBlock skipper)
    /// has been attached yet — the library has to resolve first, so it cannot
    /// be done when the view is created.
    state_attached: bool,
    /// The item the "keep watching" row offers, until anything is played.
    resume_offer: Option<String>,
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
    /// Directory holding one resume file per library. Beside the settings
    /// rather than under `cache_dir`: a cache can be cleared at any time and
    /// re-downloaded, whereas playback positions can only be re-earned by
    /// watching everything again.
    resume_dir: PathBuf,
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
    playback: PlaybackView,
    playing: Option<PlayingItem>,
    /// Where the video sits inside the window, in egui points: what the
    /// SurfaceView is placed at, and what the letterbox bars are painted
    /// around. `None` when no file is open. Held so the activity is only told
    /// when it changes rather than every frame.
    video_rect: Option<crate::surface::VideoRect>,
    /// Shared bucket cache for SponsorBlock lookups (issue #159). Buckets are
    /// not per library — a video's segments are the same whichever playlist
    /// reached it — so one cache serves them all.
    sponsorblock_cache: Arc<crate::sponsorblock::SponsorBlockCache>,
    /// The active library's skipper, or `None` when that library has the
    /// setting off. `None` is the off switch: with no watcher there is no path
    /// that contacts the service.
    sponsorblock: Option<crate::sponsorblock::SkipWatcher>,
    /// A YouTube item whose stream URLs are being resolved on a worker thread
    /// before playback can start.
    playback_pending: Option<PendingPlayback>,
    /// Resolved YouTube streams cached by watch URL, so a play that was
    /// prefetched (or recently played) skips the ~3s yt-dlp resolve. Entries
    /// carry the time they were resolved and expire via `STREAM_CACHE_TTL`
    /// (googlevideo URLs are only valid for a few hours).
    stream_cache: HashMap<String, (Instant, crate::youtube::StreamUrls)>,
    /// Resolved library contents (playlists) by library id, warmed in the
    /// background so opening a library is instant. Stored in display order
    /// (the per-library `reverse` is already applied).
    library_cache: HashMap<String, Library>,
    /// The single in-flight background resolve (at most one at a time, to avoid
    /// piling yt-dlp work on a weak TV). Started by `drive_prefetch` by priority:
    /// the focused item, then unresolved playlists, then their first-N videos.
    prefetch: Option<Prefetch>,
    /// Library ids and video watch URLs the background warm-up has already tried,
    /// so a resolve that fails (e.g. a DRM video) isn't retried forever. The
    /// focused-item prefetch and an actual play ignore this and resolve anew.
    prefetch_attempted: HashSet<String>,
    /// The focused grid index and when it became focused, used to debounce
    /// prefetch so only an item the user pauses on gets resolved.
    prefetch_focus: Option<(usize, Instant)>,
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
        // The shared library screen renders thumbnails via egui's image widget,
        // which needs egui_extras' loaders installed on the context.
        egui_extras::install_image_loaders(&cc.egui_ctx);
        // The library screen and the player set their type in Baloo 2; the
        // app's own screens keep egui's faces and the TV visuals above.
        lunchbox_media_ui::theme::install_fonts(&cc.egui_ctx);
        let (settings, status) = match AppSettings::load(&settings_path) {
            Ok(s) => (s, None),
            Err(e) => (
                AppSettings::new(),
                Some(format!("Failed to load settings: {e}")),
            ),
        };
        let (poster_tx, poster_rx) = std::sync::mpsc::channel();

        // No GL binding here any more: mpv decodes into the activity's video
        // SurfaceView rather than into our framebuffer, so `bind_gl`,
        // `render` and the redraw callback are all no-ops in this mode.
        let player = make_player();
        let playback = PlaybackView::new();
        let sponsorblock_cache = Arc::new(crate::sponsorblock::SponsorBlockCache::new(
            cache_dir.join("sponsorblock"),
        ));

        let resume_dir = settings_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("resume");

        Self {
            settings_path,
            cache_dir,
            resume_dir,
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
            video_rect: None,
            sponsorblock_cache,
            sponsorblock: None,
            playing: None,
            playback_pending: None,
            stream_cache: HashMap::new(),
            library_cache: HashMap::new(),
            prefetch: None,
            prefetch_attempted: HashSet::new(),
            prefetch_focus: None,
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
            ui.heading("lunchbox-media");
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
            ui.heading("lunchbox-media");
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
        // A library whose "Reverse" toggle flipped this frame, applied to its
        // already-loaded grid below (so it takes effect without re-resolving).
        let mut reverse_toggled: Option<String> = None;
        // Likewise for "Resume playback" and "Skip sponsors": both are attached
        // to the grid per library, so a flip of either has to re-attach them.
        let mut library_state_toggled: Option<String> = None;
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
                        ui.horizontal(|ui| {
                            // Reverse the item order (mirrors the Linux `--reverse`).
                            let rev = ui.checkbox(&mut entry.reverse, "Reverse order");
                            if rev.changed() {
                                reverse_toggled = Some(id.clone());
                            }
                            controls.push(rev);
                            // Remember playback positions (mirrors `--resume`).
                            let res = ui
                                .checkbox(&mut entry.resume, "Resume playback")
                                .on_hover_text(
                                    "Remember where each item was left off, and offer to \
                                     continue the last one watched.",
                                );
                            if res.changed() {
                                library_state_toggled = Some(id.clone());
                            }
                            controls.push(res);
                            // Skip sponsored spans (mirrors the Linux
                            // `--sponsorblock-categories`, which lunchboxd fills
                            // in from `service.media.sponsorblock`).
                            let sb = ui
                                .checkbox(&mut entry.sponsorblock, "Skip sponsors")
                                .on_hover_text(
                                    "Jump over sponsor reads, self-promotion, \
                                     \"like and subscribe\", intros and end cards in \
                                     YouTube videos, using the SponsorBlock database. \
                                     Off sends nothing to sponsor.ajay.app.",
                                );
                            if sb.changed() {
                                library_state_toggled = Some(id.clone());
                            }
                            controls.push(sb);
                        });
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

        // A "Reverse" toggle takes effect immediately: flip the already-loaded
        // grid in place rather than re-resolving (a YouTube re-fetch is slow).
        // Fresh loads apply the flag in `poll_grid`, so the two stay consistent.
        if let Some(id) = reverse_toggled
            && let Some(g) = self.grid.as_mut()
            && g.library_id == id
            && let GridState::Loaded(lib) = &mut g.state
        {
            lib.items.reverse();
            g.view = LibraryView::new();
        }

        // Same for "Resume playback" and "Skip sponsors", which are attached
        // together: turning resume on loads that library's saved positions (and
        // may offer to continue an item) and turning it off drops them, while
        // the segment skipper exists only while its own box is ticked.
        if let Some(id) = library_state_toggled
            && self.grid.as_ref().is_some_and(|g| g.library_id == id)
        {
            self.attach_library_state(&id);
        }

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
                reverse: false,
                resume: false,
                sponsorblock: false,
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
        let state = if let Some(lib) = self.library_cache.get(library_id) {
            // Warmed by the background prefetch — show it immediately.
            GridState::Loaded(lib.clone())
        } else {
            match self.settings.get(library_id) {
                Some(entry) => {
                    GridState::Loading(Self::spawn_library_resolve(ui.ctx(), entry.source.clone()))
                }
                None => GridState::Failed("This library no longer exists.".to_string()),
            }
        };
        self.grid = Some(GridView {
            library_id: library_id.to_string(),
            state,
            view: LibraryView::new(),
            resume: None,
            state_attached: false,
            resume_offer: None,
        });
    }

    /// Attach the per-library playback state once the library's contents are
    /// known: its saved positions (and whether to offer to continue the last
    /// item watched), and its SponsorBlock skipper.
    ///
    /// Deferred until the library resolves because the resume half needs its
    /// item ids: positions for departed items are dropped, and an offer is only
    /// made for an item the library still has. The skipper does not need them,
    /// but it is switched per library by the same settings card, so the two
    /// travel together and a toggle of either re-runs this.
    fn attach_library_state(&mut self, library_id: &str) {
        // The segment skipper follows the same library switch, and off is the
        // absence of one.
        self.sponsorblock = self
            .settings
            .get(library_id)
            .is_some_and(|e| e.sponsorblock)
            .then(|| {
                crate::sponsorblock::SkipWatcher::new(
                    self.sponsorblock_cache.clone(),
                    lunchbox_media_core::sponsorblock::DEFAULT_CATEGORIES.to_vec(),
                )
            })
            .flatten();

        let enabled = self
            .settings
            .get(library_id)
            .map(|e| e.resume)
            .unwrap_or(false);
        let path = self.resume_dir.join(format!("{library_id}.toml"));
        let Some(g) = self.grid.as_mut() else {
            return;
        };
        let GridState::Loaded(lib) = &g.state else {
            return;
        };
        g.state_attached = true;
        if !enabled {
            g.resume = None;
            g.resume_offer = None;
            return;
        }
        let (store, err) = ResumeStore::load_or_empty(path);
        if let Some(e) = err {
            log::warn!("ignoring unreadable resume state: {e}");
        }
        let mut tracker = ResumeTracker::new(store);
        let ids: Vec<&str> = lib.items.iter().map(|i| i.id.as_str()).collect();
        tracker.retain_known(ids.iter().copied());
        // Only an item left partway through: one watched to the end has
        // nothing to continue.
        g.resume_offer = tracker
            .last_item_in(ids.iter().copied())
            .filter(|id| tracker.start_position(id).is_some())
            .map(str::to_string);
        g.resume = Some(tracker);
    }

    /// Advance a loading grid if its worker has produced a result.
    fn poll_grid(&mut self) {
        if let Some(GridView {
            state: GridState::Loading(rx),
            ..
        }) = self.grid.as_ref()
        {
            match rx.try_recv() {
                Ok(Ok(lib)) => {
                    // Cache it (applying the per-library `reverse`, mirroring the
                    // Linux binary's `--reverse`) so the grid — and any re-open —
                    // shows it in display order.
                    let id = self.grid.as_ref().map(|g| g.library_id.clone());
                    if let Some(id) = id {
                        self.cache_library(id.clone(), lib);
                        if let Some(cached) = self.library_cache.get(&id) {
                            self.set_grid_state(GridState::Loaded(cached.clone()));
                        }
                    }
                }
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
        // library screen (the same view as the Linux binary).
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

        // The library has resolved: its per-library state can be attached now.
        if !self.grid.as_ref().is_some_and(|g| g.state_attached) {
            self.attach_library_state(library_id);
        }

        // The library screen reads the D-pad (arrow keys) and the centre button
        // (Enter) itself, and a tap plays what it lands on.
        let mut selected: Option<String> = None;
        {
            let posters = &self.posters;
            if let Some(g) = self.grid.as_mut()
                && let GridState::Loaded(lib) = &g.state
            {
                let resume = g.resume.as_ref();
                let hero = g
                    .resume_offer
                    .as_deref()
                    .and_then(|id| lib.items.iter().find(|i| i.id == id))
                    .and_then(|item| {
                        let saved = resume?.saved(&item.id)?;
                        Some(Hero {
                            item,
                            position: saved.position_seconds,
                            duration: item
                                .duration_seconds
                                .map(|d| d as f64)
                                .or(saved.duration_seconds),
                        })
                    });
                selected = g.view.draw(
                    ui,
                    &library::Library {
                        title: &title,
                        items: &lib.items,
                        hero,
                        poster: &|id| match posters.get(id) {
                            Some(PosterSlot::Ready(b)) => Some(b.clone()),
                            _ => None,
                        },
                        progress: &|item| {
                            let saved = resume?.saved(&item.id)?;
                            let duration = item
                                .duration_seconds
                                .map(|d| d as f64)
                                .or(saved.duration_seconds);
                            library::watched_fraction(saved.position_seconds, duration)
                        },
                    },
                );
                if selected.is_some() {
                    // Playing anything retires the "keep watching" row: it is
                    // how the library opens, not something to come back to.
                    g.resume_offer = None;
                }
            }
        }
        if let Some(id) = selected {
            self.start_playback(ui.ctx(), &id);
        } else {
            self.maybe_prefetch_focused(ui.ctx());
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

        // Arm the segment lookup here, before the YouTube resolve forks the two
        // paths: this is the last point where the source is still the library's
        // own URI rather than a resolved stream URL with no video id in it.
        if let Some(watcher) = self.sponsorblock.as_mut() {
            let video_id = match &source.uri {
                ClassifiedUri::YouTube(url) => lunchbox_media_core::uri::youtube_video_id(url),
                _ => None,
            };
            watcher.note_item_started(video_id);
        }

        // YouTube: resolve the stream URL on a worker before playback (network
        // must not run on the UI thread, and there's no yt-dlp on PATH for
        // mpv's own ytdl hook to use).
        if let ClassifiedUri::YouTube(watch) = &source.uri {
            let watch = watch.to_string();
            let quality = caching.quality;
            // A prefetch (or a recent play) may have already resolved this
            // stream; if so, hand the cached URLs straight to the play path via
            // a pre-filled channel so playback starts without the ~3s resolve.
            let rx = match self.stream_cache.get(&watch) {
                Some((t, streams)) if t.elapsed() < STREAM_CACHE_TTL => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let _ = tx.send(Ok(streams.clone()));
                    rx
                }
                // A background prefetch for this exact item may already be
                // resolving; adopt its receiver rather than starting a second
                // resolve for the same URL (which would run concurrently and
                // waste work on a weak device).
                _ => match self.prefetch.take() {
                    Some(Prefetch::Video { watch: w, rx }) if w == watch => rx,
                    other => {
                        self.prefetch = other;
                        Self::spawn_resolve(ctx, watch.clone(), quality)
                    }
                },
            };
            self.playback_pending = Some(PendingPlayback {
                item_id: item_id.to_string(),
                title,
                watch,
                rx,
            });
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
                // Playing from cache is the moment interest is recorded, so
                // eviction stops treating this file as a replaceable guess.
                cache.mark_played(&url);
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
        // Resume: hand the saved position to the player with the file, so
        // nothing before it is decoded (a seek afterwards would show the
        // opening seconds first).
        let start = self.resume_position(item_id);
        match self.player.as_mut() {
            Some(p) => {
                p.set_start_position(start);
                match play_with_surface(p.as_mut(), &play_source) {
                    Ok(()) => {
                        self.playback.note_started();
                        self.playing = Some(PlayingItem {
                            title,
                            cache: playing_cache,
                            source: play_source,
                            external_audio: None,
                            retries: RetryBudget::new(),
                        });
                        self.note_resume_started(item_id);
                    }
                    Err(e) => self.status = Some(format!("Playback failed: {e}")),
                }
            }
            None => self.status = Some("No player available on this platform.".to_string()),
        }
    }

    /// The saved position for `item_id`, or `None` when the library's resume
    /// option is off (or it has no saved position).
    fn resume_position(&self, item_id: &str) -> Option<f64> {
        self.grid.as_ref()?.resume.as_ref()?.start_position(item_id)
    }

    /// Mark `item_id` as the library's most recently watched item and start
    /// tracking its position. No-op when the library's resume option is off.
    fn note_resume_started(&mut self, item_id: &str) {
        if let Some(tracker) = self.grid.as_mut().and_then(|g| g.resume.as_mut()) {
            tracker.note_started(item_id, Instant::now());
        }
    }

    /// Stop playback.
    ///
    /// Every path that ends playback goes through here. Note what it does *not*
    /// do: it leaves mpv's window alone. See `surface.rs` for why detaching
    /// here aborts the process.
    fn end_playback(&mut self) {
        if let Some(p) = self.player.as_mut() {
            let _ = p.stop();
        }
        if let Some(watcher) = self.sponsorblock.as_mut() {
            watcher.note_stopped();
        }
        // The video rectangle is deliberately left alone: see
        // `track_video_bounds`. It is replaced when the next file opens.
        self.playing = None;
    }

    /// Drive playback for a frame: drain player events (ending playback on
    /// EOF/close/error) and draw the transport overlay. The video itself is not
    /// ours to draw — mpv renders it into the SurfaceView behind this window.
    /// Returns to the grid when playback ends or the user leaves.
    fn run_playback(&mut self, ui: &mut egui::Ui) {
        // Drain events without holding a borrow across the mutation. A clean
        // end (EOF/close) leaves; an error is transient — retry it below.
        let mut ended = false;
        let mut errored: Option<String> = None;
        if let Some(p) = self.player.as_mut() {
            while let Some(ev) = p.poll_event() {
                match ev {
                    PlayerEvent::EndOfFile | PlayerEvent::Closed => ended = true,
                    PlayerEvent::Error(e) => errored = Some(e),
                    PlayerEvent::Started => {}
                }
            }
        }

        // A flaky stream ends the file with an error (e.g. a googlevideo
        // connection dropping just after it opens). Retry the same source a few
        // times before giving up — these usually recover on a second try; only
        // surface the error and return to the grid once retries are exhausted.
        if let Some(err) = errored
            && !ended
        {
            let retry = self
                .playing
                .as_mut()
                .is_some_and(|it| it.retries.try_retry());
            if retry {
                let (src, audio) = {
                    let it = self.playing.as_ref().expect("checked above");
                    log::warn!("playback error, restarting playback: {err}");
                    (it.source.clone(), it.external_audio.clone())
                };
                // Restart where the failure hit rather than at the beginning
                // (only known when the library's resume option is on).
                let start = self
                    .grid
                    .as_ref()
                    .and_then(|g| g.resume.as_ref())
                    .and_then(|tracker| tracker.live_position());
                if let Some(p) = self.player.as_mut() {
                    p.set_external_audio(audio);
                    p.set_start_position(start);
                    if let Err(e) = play_with_surface(p.as_mut(), &src) {
                        self.status = Some(format!("Playback failed: {e}"));
                        ended = true;
                    }
                }
            } else {
                self.status = Some(format!("Playback failed: {err}"));
                ended = true;
            }
        }

        if ended {
            // Write out where the item got to before the player forgets it.
            self.finish_resume_tracking();
            // Download-after-play: cache the just-finished item so the next play
            // is local. Runs on a worker (download + LRU eviction are blocking).
            let finished = self.playing.take();
            self.end_playback();
            if let Some(item) = finished
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

        // Still playing: skip anything the viewer has reached, then feed the
        // position into the resume state (batched — this runs every frame).
        // In that order, so a position saved this frame is the one on the far
        // side of a skip rather than inside it.
        self.track_video_bounds(ui.max_rect().size(), ui.ctx().pixels_per_point());
        self.apply_segment_skip();
        self.track_resume_progress();

        let title = self
            .playing
            .as_ref()
            .map(|x| x.title.clone())
            .unwrap_or_default();
        let leave = match self.player.as_mut() {
            Some(p) => self.playback.draw(ui, p.as_mut(), &title, self.video_rect),
            None => {
                ui.label("No player available on this platform.");
                ui.button("⬅ Back").clicked()
            }
        };
        if leave {
            self.finish_resume_tracking();
            self.end_playback();
        }
    }

    /// Keep the video SurfaceView placed at the rectangle the playing file
    /// should occupy, and remember it so the bars can be painted around it.
    ///
    /// Polled rather than pushed once at play time: the size is not known when
    /// playback is requested, only once the file is open, and it can change
    /// again mid-playback (a stream switching representation, a playlist moving
    /// on). The activity is told only when the rectangle actually changes, so
    /// the steady state is one property read per frame and no JNI at all.
    fn track_video_bounds(&mut self, window: egui::Vec2, pixels_per_point: f32) {
        let size = self.player.as_ref().and_then(|p| p.video_size());
        let rect = size.and_then(|(w, h)| {
            crate::surface::fit_video((window.x, window.y), (w as f32, h as f32))
        });
        // A size that has *stopped* being known is not a reason to resize. The
        // surface still holds the last frame of the file that just ended, and
        // stretching it back across the window to say "nothing is playing" is
        // the very distortion this exists to avoid. The next file sets its own
        // bounds when it opens.
        let Some(rect) = rect else { return };
        let rect = Some(rect);
        if rect != self.video_rect {
            self.video_rect = rect;
            // The activity lays views out in window pixels; egui works in
            // points, so this is the one place the two have to agree.
            crate::surface::set_video_bounds(rect.map(|r| crate::surface::VideoRect {
                x: r.x * pixels_per_point,
                y: r.y * pixels_per_point,
                width: r.width * pixels_per_point,
                height: r.height * pixels_per_point,
            }));
        }
    }

    /// Seek past a SponsorBlock segment the viewer has reached.
    ///
    /// No-op when the library has the setting off, when the lookup has not
    /// answered yet, or when the player cannot yet report a duration — every one
    /// of which simply plays the video as it is.
    fn apply_segment_skip(&mut self) {
        let Some(watcher) = self.sponsorblock.as_mut() else {
            return;
        };
        let (position, duration) = match self.player.as_ref() {
            Some(p) => (p.position(), p.duration()),
            None => return,
        };
        let Some(skip) = watcher.poll(position, duration) else {
            return;
        };
        if let Some(p) = self.player.as_mut() {
            match p.seek_absolute(skip.target) {
                Ok(()) => self.playback.note_skipped(skip.category),
                Err(e) => log::warn!("could not skip a SponsorBlock segment: {e}"),
            }
        }
    }

    /// Feed the player's current position into the library's resume state.
    /// No-op when its resume option is off or nothing is playing.
    fn track_resume_progress(&mut self) {
        if self.playing.is_none() {
            return;
        }
        let (position, duration) = match self.player.as_ref() {
            Some(p) => (p.position(), p.duration()),
            None => return,
        };
        if let Some(tracker) = self.grid.as_mut().and_then(|g| g.resume.as_mut()) {
            tracker.progress(position, duration, Instant::now());
        }
    }

    /// Playback is over (EOF, error, or the viewer left): record the final
    /// position and write it out. An item that reached its end is forgotten, so
    /// the next play starts over.
    fn finish_resume_tracking(&mut self) {
        if let Some(tracker) = self.grid.as_mut().and_then(|g| g.resume.as_mut()) {
            tracker.finished();
        }
    }

    /// Resolve a YouTube `watch` URL to its stream URLs on a worker thread
    /// (network must not run on the UI thread), delivering the result on the
    /// returned receiver and repainting when it lands.
    fn spawn_resolve(
        ctx: &egui::Context,
        watch: String,
        quality: Quality,
    ) -> Receiver<StreamResult> {
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
        rx
    }

    /// Prefetch the focused grid item and the two around it once focus has
    /// settled, so tapping play — or moving to a neighbour and playing — starts
    /// near-instantly. Resolves one at a time (focused first).
    fn maybe_prefetch_focused(&mut self, ctx: &egui::Context) {
        // Don't compete with an in-flight play resolution, an active playback,
        // or an already-running prefetch.
        if self.playback_pending.is_some() || self.playing.is_some() || self.prefetch.is_some() {
            return;
        }
        // The focused index and item count (grid must be loaded and non-empty).
        let focused = self.grid.as_ref().and_then(|g| match &g.state {
            GridState::Loaded(lib) => {
                let id = g.view.focused_item()?;
                Some((lib.items.iter().position(|i| i.id == id)?, lib.items.len()))
            }
            _ => None,
        });
        let (focused, n) = match focused {
            Some(found) => found,
            None => {
                self.prefetch_focus = None;
                return;
            }
        };
        // Restart the dwell timer whenever the focus moves; only prefetch once it
        // has rested on this item for `PREFETCH_DWELL`.
        match self.prefetch_focus {
            Some((i, since)) if i == focused => {
                if since.elapsed() < PREFETCH_DWELL {
                    ctx.request_repaint_after(PREFETCH_DWELL);
                    return;
                }
            }
            _ => {
                self.prefetch_focus = Some((focused, Instant::now()));
                ctx.request_repaint_after(PREFETCH_DWELL);
                return;
            }
        }
        // The focused item plus its two neighbours, focused first. Resolve the
        // first that isn't already cached or attempted; the slot chains through
        // the rest across frames as each completes.
        let window = [
            Some(focused),
            focused.checked_sub(1),
            (focused + 1 < n).then_some(focused + 1),
        ];
        for idx in window.into_iter().flatten() {
            let Some((watch, quality)) = self.item_watch(idx) else {
                continue;
            };
            let fresh = self
                .stream_cache
                .get(&watch)
                .is_some_and(|(t, _)| t.elapsed() < STREAM_CACHE_TTL);
            if fresh || self.prefetch_attempted.contains(&watch) {
                continue;
            }
            self.prefetch_attempted.insert(watch.clone());
            let rx = Self::spawn_resolve(ctx, watch.clone(), quality);
            self.prefetch = Some(Prefetch::Video { watch, rx });
            return;
        }
    }

    /// The YouTube watch URL and quality for the grid item at `idx`, if it is a
    /// YouTube source in the currently loaded library.
    fn item_watch(&self, idx: usize) -> Option<(String, Quality)> {
        let g = self.grid.as_ref()?;
        let GridState::Loaded(lib) = &g.state else {
            return None;
        };
        let item = lib.items.get(idx)?;
        let ClassifiedUri::YouTube(watch) = &resolve_source(item, &PlatformInfo::current())?.uri
        else {
            return None;
        };
        let quality = self
            .settings
            .get(&g.library_id)
            .map(|e| e.caching.quality)
            .unwrap_or_default();
        Some((watch.to_string(), quality))
    }

    /// Resolve a library's contents (playlist → items) on a worker thread,
    /// delivering the result on the returned receiver and repainting when done.
    fn spawn_library_resolve(
        ctx: &egui::Context,
        source: LibrarySource,
    ) -> Receiver<Result<Library, ResolveError>> {
        let ctx = ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(resolve(&source));
            ctx.request_repaint();
        });
        rx
    }

    /// Store a resolved library in the warm cache, applying the per-library
    /// `reverse` so it matches display order (as the grid shows it).
    fn cache_library(&mut self, id: String, mut lib: Library) {
        if self.settings.get(&id).is_some_and(|e| e.reverse) {
            lib.items.reverse();
        }
        self.library_cache.insert(id, lib);
    }

    /// Bank a completed background resolve into its cache. Failures are dropped
    /// silently — a real open/play will resolve again and report any error.
    fn poll_prefetch(&mut self) {
        match self.prefetch.take() {
            Some(Prefetch::Video { watch, rx }) => match rx.try_recv() {
                Ok(Ok(streams)) => {
                    self.stream_cache.insert(watch, (Instant::now(), streams));
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {}
                Err(TryRecvError::Empty) => self.prefetch = Some(Prefetch::Video { watch, rx }),
            },
            Some(Prefetch::Library { id, rx }) => match rx.try_recv() {
                Ok(Ok(lib)) => self.cache_library(id, lib),
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {}
                Err(TryRecvError::Empty) => self.prefetch = Some(Prefetch::Library { id, rx }),
            },
            None => {}
        }
    }

    /// Warm caches in the background while the app is idle: resolve every
    /// playlist first, then the leading videos of each. Runs one resolve at a
    /// time and yields the slot to the higher-priority focused-item prefetch.
    fn drive_background_prefetch(&mut self, ctx: &egui::Context) {
        if self.playing.is_some() || self.playback_pending.is_some() || self.prefetch.is_some() {
            return;
        }
        // Every playlist before any video, per the warm-up order.
        if let Some((id, source)) = self.next_unresolved_library() {
            self.prefetch_attempted.insert(id.clone());
            let rx = Self::spawn_library_resolve(ctx, source);
            self.prefetch = Some(Prefetch::Library { id, rx });
            return;
        }
        if let Some((watch, quality)) = self.next_firstn_video() {
            self.prefetch_attempted.insert(watch.clone());
            let rx = Self::spawn_resolve(ctx, watch.clone(), quality);
            self.prefetch = Some(Prefetch::Video { watch, rx });
        }
    }

    /// The first configured library not yet resolved (skipping one the grid is
    /// already loading, and any the warm-up already tried).
    fn next_unresolved_library(&self) -> Option<(String, LibrarySource)> {
        let loading = self.grid.as_ref().and_then(|g| match g.state {
            GridState::Loading(_) => Some(g.library_id.as_str()),
            _ => None,
        });
        self.settings.libraries.iter().find_map(|e| {
            (!self.library_cache.contains_key(&e.id)
                && !self.prefetch_attempted.contains(&e.id)
                && Some(e.id.as_str()) != loading)
                .then(|| (e.id.clone(), e.source.clone()))
        })
    }

    /// The first not-yet-cached YouTube stream among the leading
    /// `PREFETCH_FIRST_N` items of each resolved library.
    fn next_firstn_video(&self) -> Option<(String, Quality)> {
        let info = PlatformInfo::current();
        for entry in &self.settings.libraries {
            let Some(lib) = self.library_cache.get(&entry.id) else {
                continue;
            };
            for item in lib.items.iter().take(PREFETCH_FIRST_N) {
                let Some(source) = resolve_source(item, &info) else {
                    continue;
                };
                let ClassifiedUri::YouTube(watch) = &source.uri else {
                    continue;
                };
                let watch = watch.to_string();
                let fresh = self
                    .stream_cache
                    .get(&watch)
                    .is_some_and(|(t, _)| t.elapsed() < STREAM_CACHE_TTL);
                if !fresh && !self.prefetch_attempted.contains(&watch) {
                    return Some((watch, entry.caching.quality));
                }
            }
        }
        None
    }

    /// If a pending YouTube stream resolution has completed, start (or fail)
    /// playback with the resolved direct URL.
    fn poll_playback_pending(&mut self) {
        let Some(pending) = self.playback_pending.as_ref() else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("YouTube resolver thread died.".to_string()),
        };
        let PendingPlayback {
            item_id,
            title,
            watch,
            ..
        } = self.playback_pending.take().unwrap();
        let streams = match result {
            Ok(u) => u,
            Err(e) => {
                self.status = Some(e);
                return;
            }
        };
        // Cache the resolution so replaying this item is instant (harmless if it
        // came from the cache already).
        self.stream_cache
            .insert(watch, (Instant::now(), streams.clone()));
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
        let start = self.resume_position(&item_id);
        match self.player.as_mut() {
            Some(p) => {
                // YouTube DASH gives separate tracks: attach the audio URL as an
                // external track so the video-only stream plays with sound.
                let audio = streams.audio;
                p.set_external_audio(audio.clone());
                p.set_start_position(start);
                match play_with_surface(p.as_mut(), &src) {
                    Ok(()) => {
                        self.playback.note_started();
                        self.playing = Some(PlayingItem {
                            title,
                            cache: None,
                            source: src,
                            external_audio: audio,
                            retries: RetryBudget::new(),
                        });
                        self.note_resume_started(&item_id);
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
    w.active.bg_stroke = egui::Stroke::new(3.0_f32, egui::Color32::WHITE);
    w.active.fg_stroke = egui::Stroke::new(2.0_f32, egui::Color32::WHITE);
    w.active.expansion = 3.0;
    visuals
}

impl eframe::App for MediaApp {
    /// Clear to transparent while playing so the video SurfaceView behind this
    /// window shows through; opaque everywhere else, where there is nothing
    /// behind us and a see-through UI would look broken.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.playing.is_some() {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            visuals.panel_fill.to_normalized_gamma_f32()
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Diff settings across the frame so any mutation persists automatically.
        let before = self.settings.clone();

        // Promote a finished YouTube resolution into active playback, and bank
        // any completed background prefetch.
        self.poll_playback_pending();
        self.poll_prefetch();

        // Playback takes over the whole surface while an item is playing and
        // fills it edge-to-edge (the video is composited full-screen), so it is
        // deliberately NOT inset. Only the browse/settings screens below are
        // inset, so navigation clears the camera cutout and rounded corners
        // while video still uses the entire display as it did before.
        if self.playing.is_some() {
            self.run_playback(ui);
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

            // Warm playlists and their leading videos in the background whenever
            // the slot isn't taken by the focused-item prefetch above.
            self.drive_background_prefetch(ui.ctx());

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
/// egui's combo popup doesn't reliably take keyboard focus when opened, so a
/// remote's Up/Down either does nothing or spatially escapes to a neighbouring
/// widget instead of moving through the options. So while the popup is open we
/// consume Up/Down ourselves and cycle `current` in place (the popup's selected
/// highlight follows it); the list stays open so the choice is visible. Enter
/// toggles the popup shut (egui's own button behaviour), and BACK dismisses it
/// via the update loop's global handler. A pointer click on an option still
/// commits and closes. Returns the combo button response (for focus stepping).
fn tv_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + Copy,
    current: &mut T,
    options: &[(T, &str)],
) -> egui::Response {
    // The popup's open state is keyed by the combo's own id; recompute it the
    // same way `ComboBox::from_id_salt` does (`make_persistent_id(Id::new(salt))`)
    // so we can query it before rendering.
    let combo_id = ui.make_persistent_id(egui::Id::new(id_salt));
    if egui::ComboBox::is_open(ui.ctx(), combo_id) {
        // Own the D-pad while open: Up/Down cycle the value, Enter commits and
        // closes (egui closes the popup only on a *pointer* click, not Enter, and
        // its popup focus is too unreliable to steer with a remote). Consume the
        // keys so egui doesn't also act on them (spatial focus escape / re-toggle).
        let (step, commit) = ui.input_mut(|i| {
            let down = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
            let up = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            let enter = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            (down as i32 - up as i32, enter)
        });
        if step != 0
            && let Some(idx) = options.iter().position(|(v, _)| *v == *current)
        {
            let n = options.len() as i32;
            *current = options[(idx as i32 + step).rem_euclid(n) as usize].0;
        }
        if commit {
            egui::Popup::close_all(ui.ctx());
            ui.ctx().memory_mut(|m| m.request_focus(combo_id));
        }
    }

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
    // A pointer click on an option changes the value without going through the
    // cycling above; commit it (close + keep focus on the combo).
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
