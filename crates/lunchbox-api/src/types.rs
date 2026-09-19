//! Shared types for the lunchboxd API

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use lunchbox_util::{EntryId, GroupId, LimitSubject, SessionId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Entry kind tag for capability matching
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EntryKindTag {
    Process,
    Snap,
    Steam,
    Flatpak,
    Vm,
    Media,
    Retroarch,
    Ebook,
    Custom,
}

impl EntryKindTag {
    /// Every kind, in declaration order.
    ///
    /// Exists so the per-kind defaults below can be *enumerated* rather than
    /// mirrored: the config editor needs the same answers, and
    /// `lunchbox-wire-codegen` walks this list to generate them.
    pub const ALL: [EntryKindTag; 9] = [
        EntryKindTag::Process,
        EntryKindTag::Snap,
        EntryKindTag::Steam,
        EntryKindTag::Flatpak,
        EntryKindTag::Vm,
        EntryKindTag::Media,
        EntryKindTag::Retroarch,
        EntryKindTag::Ebook,
        EntryKindTag::Custom,
    ];

    /// The wire and config spelling of this tag, matching its serde rename.
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKindTag::Process => "process",
            EntryKindTag::Snap => "snap",
            EntryKindTag::Steam => "steam",
            EntryKindTag::Flatpak => "flatpak",
            EntryKindTag::Vm => "vm",
            EntryKindTag::Media => "media",
            EntryKindTag::Retroarch => "retroarch",
            EntryKindTag::Ebook => "ebook",
            EntryKindTag::Custom => "custom",
        }
    }

    /// Whether the HUD's "X" confirms before ending this activity, absent an
    /// explicit `confirm_on_close` (issue #78).
    ///
    /// The prompt exists because the button is easy to hit by accident and
    /// most activities lose unsaved state when they are closed — a game
    /// mid-level, a drawing. A reading activity has nothing to lose: the
    /// position is written on the way out, and reopening returns to the page.
    /// So the prompt is pure friction there, on the one activity a child is
    /// most likely to open and close repeatedly.
    ///
    /// An entry that sets the field explicitly always wins; this is only what
    /// happens when it is silent.
    pub fn confirms_on_close_by_default(self) -> bool {
        !matches!(self, EntryKindTag::Ebook)
    }

    /// The input-compat sidecars this activity runs absent an explicit
    /// `input_compat` (issue #160).
    ///
    /// Only `ebook` asks for one. A gamepad is the one controller a reading
    /// device is likely to have and the reader cannot use: Okular listens for
    /// arrow keys, `Page Up` / `Page Down` and the scroll wheel, and a pad
    /// produces none of them on its own. The productivity preset maps the
    /// D-pad to the arrow keys, so a pad turns pages the moment a book opens,
    /// with nothing to configure.
    ///
    /// It costs an idle bridge process when no pad is plugged in, which is why
    /// this is a per-kind answer rather than a global one — and the bridge
    /// handles hotplug, so a pad connected mid-book works.
    ///
    /// An entry that lists `input_compat` replaces this wholesale, and an
    /// empty list turns it off; the default only applies when the field is
    /// absent.
    pub fn default_input_compat(self) -> Vec<InputCompatMode> {
        match self {
            EntryKindTag::Ebook => vec![InputCompatMode::GamepadProductivity],
            _ => Vec::new(),
        }
    }
}

/// A known Steam "launch interstitial" — one of the blocking modals Steam can
/// show between a launch request and the game actually starting (cloud-sync
/// warnings, controller advisories, etc.). The kiosk can be configured to
/// auto-dismiss specific kinds; see `service.steam.auto_dismiss_interstitials`.
///
/// This enum is the canonical catalog: config validates against it, and the
/// host adapter attaches the per-kind CEF detection signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InterstitialKind {
    /// "Unable to Sync" Steam Cloud warning shown when launching offline with
    /// un-uploaded saves. Affirmative action: "Play anyway". (Verified.)
    CloudSync,
    /// "Grab a controller…" advisory for controller-recommended games launched
    /// without a controller. Affirmative action: "OK". (Verified.)
    ControllerRecommended,
    /// First-launch "intro to Steam Input" notice. Affirmative action: "OK".
    /// (Best-effort signature.)
    SteamInputIntro,
    /// Game *requires* a controller. Dismissing launches a game that cannot be
    /// played without one, so this is risky. (Best-effort signature.)
    ControllerRequired,
    /// Game requires a VR headset. Dismissing launches something unusable
    /// without VR hardware, so this is risky. (Best-effort signature.)
    VrRequired,
}

impl InterstitialKind {
    /// Every known kind, in catalog order.
    pub const ALL: [InterstitialKind; 5] = [
        InterstitialKind::CloudSync,
        InterstitialKind::ControllerRecommended,
        InterstitialKind::SteamInputIntro,
        InterstitialKind::ControllerRequired,
        InterstitialKind::VrRequired,
    ];

    /// The kinds auto-dismissed by default: the verified, benign ones.
    pub const DEFAULT_AUTO_DISMISS: [InterstitialKind; 2] = [
        InterstitialKind::CloudSync,
        InterstitialKind::ControllerRecommended,
    ];

    /// Stable config slug for this kind (matches the serde snake_case name).
    pub fn slug(self) -> &'static str {
        match self {
            InterstitialKind::CloudSync => "cloud_sync",
            InterstitialKind::ControllerRecommended => "controller_recommended",
            InterstitialKind::SteamInputIntro => "steam_input_intro",
            InterstitialKind::ControllerRequired => "controller_required",
            InterstitialKind::VrRequired => "vr_required",
        }
    }

    /// Parse a config slug into a kind.
    pub fn from_slug(slug: &str) -> Option<Self> {
        InterstitialKind::ALL.into_iter().find(|k| k.slug() == slug)
    }

    /// Whether auto-dismissing this kind launches something the user likely
    /// can't actually use (missing required hardware). Risky kinds require an
    /// explicit opt-in to enable.
    pub fn is_risky(self) -> bool {
        matches!(
            self,
            InterstitialKind::ControllerRequired | InterstitialKind::VrRequired
        )
    }
}

/// How a [`EntryKind::Media`] activity opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MediaMode {
    /// Open the poster grid over the whole library; the user picks items.
    #[default]
    Browse,
    /// Play a single item end to end; the grid is never shown.
    Play,
}

impl MediaMode {
    /// The `lunchbox-media` subcommand for this mode.
    pub fn subcommand(self) -> &'static str {
        match self {
            MediaMode::Browse => "browse",
            MediaMode::Play => "play",
        }
    }
}

/// Maximum video quality for a [`EntryKind::Media`] activity.
///
/// Mirrors `lunchbox_media_app::Quality`; kept here so the wire schema and the
/// config layer don't depend on the media crates. `lunchbox-media`'s `cli`
/// module holds the test that keeps the two spellings in agreement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MediaQuality {
    /// No height restriction — the best available.
    #[serde(rename = "best")]
    Best,
    /// Up to 1080p (default).
    #[default]
    #[serde(rename = "1080p")]
    Q1080,
    /// Up to 720p.
    #[serde(rename = "720p")]
    Q720,
    /// Up to 480p.
    #[serde(rename = "480p")]
    Q480,
}

impl MediaQuality {
    /// The `--quality` value for this preset.
    pub fn as_flag(self) -> &'static str {
        match self {
            MediaQuality::Best => "best",
            MediaQuality::Q1080 => "1080p",
            MediaQuality::Q720 => "720p",
            MediaQuality::Q480 => "480p",
        }
    }
}

/// How a [`EntryKind::Media`] activity orders its library.
///
/// Mirrors `lunchbox-media`'s `--sort-by` values; see [`MediaQuality`] for
/// where that agreement is tested.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MediaSortBy {
    /// Preserve the order from the library file or playlist (default).
    #[default]
    Library,
    /// Display title, case-insensitive.
    Title,
    /// Stable item id.
    Id,
    /// Item kind (audio before video).
    Kind,
    /// Optional category string, case-insensitive.
    Category,
    /// Optional duration in seconds, ascending.
    Duration,
}

impl MediaSortBy {
    /// The `--sort-by` value for this ordering.
    pub fn as_flag(self) -> &'static str {
        match self {
            MediaSortBy::Library => "library",
            MediaSortBy::Title => "title",
            MediaSortBy::Id => "id",
            MediaSortBy::Kind => "kind",
            MediaSortBy::Category => "category",
            MediaSortBy::Duration => "duration",
        }
    }
}

/// Entry kind with launch details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    Process {
        /// Command to run (required)
        command: String,
        /// Additional command-line arguments
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        cwd: Option<PathBuf>,
    },
    /// Snap application - uses systemd scope-based process management
    Snap {
        /// The snap name (e.g., "mc-installer")
        snap_name: String,
        /// Command to run (defaults to snap_name if not specified)
        #[serde(default)]
        command: Option<String>,
        /// Additional command-line arguments
        #[serde(default)]
        args: Vec<String>,
        /// Additional environment variables
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Steam game launched via the Steam snap (Linux)
    Steam {
        /// Steam App ID (e.g., 504230 for Celeste)
        app_id: u32,
        /// Additional command-line arguments passed to Steam
        #[serde(default)]
        args: Vec<String>,
        /// Additional environment variables
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Flatpak application - uses systemd scope-based process management
    Flatpak {
        /// The Flatpak application ID (e.g., "org.prismlauncher.PrismLauncher")
        app_id: String,
        /// Additional command-line arguments
        #[serde(default)]
        args: Vec<String>,
        /// Additional environment variables
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Vm {
        driver: String,
        #[serde(default)]
        args: HashMap<String, serde_json::Value>,
    },
    /// A `lunchbox-media` library activity (issue #127).
    ///
    /// The fields mirror the flags `lunchbox-media` accepts, so lunchboxd can
    /// build the invocation itself instead of an admin restating it as a
    /// `Process` argv. `connectivity_check` is not among them: it is resolved
    /// from the entry's `internet` policy at spawn time and reaches the host
    /// adapter through `SpawnOptions`.
    Media {
        /// Library source: a path to a `.toml`/`.m3u`/`.m3u8` file, or a
        /// YouTube playlist URL. `~` is expanded for paths at spawn time.
        library: String,
        /// Whether to open the poster grid or play a single item.
        #[serde(default)]
        mode: MediaMode,
        /// The item to play. Required by (and only meaningful for)
        /// [`MediaMode::Play`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
        /// Maximum video quality for playback and background downloads.
        #[serde(default)]
        quality: MediaQuality,
        /// Field used to order library items before display or lookup.
        #[serde(default)]
        sort_by: MediaSortBy,
        /// Reverse the final item order. Combines with `sort_by`.
        #[serde(default)]
        reverse: bool,
        /// Remember playback positions for this library across sessions.
        #[serde(default)]
        resume: bool,
        /// Whether lunchboxd may prefetch this library's remote items in the
        /// background. `None` inherits `service.media.prefetch`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefetch: Option<bool>,
        /// Whether to skip SponsorBlock segments in this library. `None`
        /// inherits `service.media.sponsorblock.enabled`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sponsorblock: Option<bool>,
    },
    /// A single piece of content played through the RetroArch libretro
    /// frontend, launched directly on its CLI (`retroarch -L <core> <content>`).
    ///
    /// Distinct from [`EntryKind::Process`] because RetroArch needs settings
    /// materialized around the launch to behave in a kiosk: save state on
    /// close, restore it on open, flush the in-game save periodically, and
    /// stay out of its own menu. The host adapter renders those into a config
    /// fragment it passes with `--appendconfig`; the user's own `retroarch.cfg`
    /// is never edited. See `lunchbox-host-linux::retroarch`.
    Retroarch {
        /// Core short name, e.g. `"mgba"` → `mgba_libretro.so`, resolved
        /// against the usual libretro core directories. Mutually exclusive
        /// with `core_path`.
        #[serde(default)]
        core: Option<String>,
        /// Absolute path to a `*_libretro.so`, bypassing name resolution.
        #[serde(default)]
        core_path: Option<PathBuf>,
        /// The content (ROM / disc image) to load.
        content: PathBuf,
        /// Whether closing the activity saves state and opening restores it.
        #[serde(default)]
        save_state: RetroarchSaveState,
        /// The RetroArch binary. Defaults to `retroarch` on `PATH`.
        #[serde(default = "default_retroarch_command")]
        command: String,
        /// Extra arguments, appended after the ones shepherd derives.
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        /// Lock RetroArch's own menu so the activity can't be used to browse
        /// the filesystem or change emulator settings. On by default: this is
        /// a supervised kiosk.
        #[serde(default = "default_true")]
        kiosk: bool,
        /// Offer a reset ("reboot the console") button on the HUD. On by
        /// default, because `save_state = "auto"` otherwise makes the
        /// console's own power-on screen unreachable — there is no way back to
        /// the title screen from inside a resumed save state.
        #[serde(default = "default_true")]
        reset: bool,
    },
    /// One book, opened in a document reader locked down to reading it
    /// (issue #160).
    ///
    /// The reader keeps the page: shepherd's job is to hand it a private
    /// configuration that closes every door out of the book, and to close the
    /// window politely at the end of the session so the position is written.
    /// See [`lunchbox_host_linux::ebook`] for what is generated.
    Ebook {
        /// The book. Absolute, or `~/`-prefixed; expanded at launch.
        book: PathBuf,
        /// Which reader to drive. Only `okular` is implemented.
        #[serde(default)]
        viewer: EbookViewer,
        /// Page to open on the *first* launch, 1-based. Ignored once the
        /// reader has a remembered position for this book.
        #[serde(default)]
        open_at: Option<u32>,
        /// How pages are laid out. `facing` (the default) suits a landscape
        /// panel; `single` a portrait one.
        #[serde(default)]
        layout: EbookLayout,
        /// Point size for the reflowed text of an EPUB. Changing it
        /// repaginates the book, which moves a remembered position, so pick it
        /// before the book is first opened.
        #[serde(default = "default_ebook_font_size")]
        font_size: u32,
        /// Font family for the same. Must be installed on the device.
        #[serde(default = "default_ebook_font_family")]
        font_family: String,
        /// The reader binary. Defaults to the viewer's usual name.
        #[serde(default)]
        command: Option<String>,
        /// Extra arguments, appended after the ones shepherd derives.
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        /// Lock the reader down: no file dialog, no printing, no settings, no
        /// menubar or toolbar. On by default — this is a supervised kiosk, and
        /// off is only for an admin checking what the reader looks like
        /// unrestricted.
        #[serde(default = "default_true")]
        kiosk: bool,
    },
    Custom {
        type_name: String,
        payload: serde_json::Value,
    },
}

/// Default for [`EntryKind::Retroarch::command`].
pub(crate) fn default_retroarch_command() -> String {
    "retroarch".to_string()
}

pub(crate) fn default_ebook_font_size() -> u32 {
    16
}

pub(crate) fn default_ebook_font_family() -> String {
    "Noto Serif".to_string()
}

/// Which reader an [`EntryKind::Ebook`] activity drives.
///
/// Open rather than closed on purpose: the config surface here — a book and a
/// place in it — is reader-agnostic, even though only one reader is wired up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EbookViewer {
    /// Okular (`okular`), with `okular-extra-backends` for EPUB. Covers EPUB,
    /// PDF, CBZ, DjVu and FictionBook, and is the only reader in Ubuntu with a
    /// documented way to disable its own escape hatches.
    #[default]
    Okular,
}

impl EbookViewer {
    /// The binary this viewer runs as, when the entry does not name one.
    pub fn default_command(self) -> &'static str {
        match self {
            EbookViewer::Okular => "okular",
        }
    }
}

/// How an [`EntryKind::Ebook`] activity lays pages out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EbookLayout {
    /// Two pages side by side, like an open book. Fits a landscape panel: a
    /// single portrait page fitted to 16:9 is letterboxed and small.
    Facing,
    /// The same, with the first page alone — so the spreads fall where a
    /// printed book's would, cover on its own and chapter openings on the
    /// right. The default: it costs nothing over `facing` and matches what a
    /// child holding a paper book expects.
    #[default]
    FacingFirstCentered,
    /// One page at a time. The right choice on a portrait screen.
    Single,
    /// One continuous column, scrolled rather than paged, fitted to the width.
    ///
    /// The only layout a **touch-only** device can navigate: dragging scrolls
    /// it. The paged layouts turn the page on a key, a gamepad D-pad or a
    /// scroll wheel, and a touchscreen produces none of those — Okular grabs
    /// only the pinch gesture, and has no swipe-to-turn anywhere in its
    /// desktop view.
    Scroll,
}

impl EbookLayout {
    /// The value Okular's `[PageView] ViewMode` takes for this layout.
    pub fn okular_view_mode(self) -> &'static str {
        match self {
            EbookLayout::Facing => "Facing",
            EbookLayout::FacingFirstCentered => "FacingFirstCentered",
            // Scrolling a two-page spread is nobody's idea of reading.
            EbookLayout::Single | EbookLayout::Scroll => "Single",
        }
    }

    /// Whether the view scrolls continuously instead of turning pages.
    pub fn is_continuous(self) -> bool {
        matches!(self, EbookLayout::Scroll)
    }

    /// Okular's `[Zoom] ZoomMode`: fit the width of a scrolled column, fit the
    /// whole page when the page is the unit of navigation.
    pub fn okular_zoom_mode(self) -> u8 {
        match self {
            EbookLayout::Scroll => 1,
            _ => 2,
        }
    }

    /// Whether turning the page needs an input a touchscreen cannot produce.
    ///
    /// The basis of the `EbookNoPageTurn` diagnostic: a paged layout on a
    /// touch-only device leaves a child stranded on page one.
    pub fn needs_keys_to_turn_pages(self) -> bool {
        !self.is_continuous()
    }
}

/// How a [`EntryKind::Retroarch`] activity treats its save state across
/// close and re-open.
///
/// This is the emulator's *snapshot*, not the game's own save file. The
/// in-game save (SRAM / battery save) is flushed on a clean exit either way,
/// and periodically while playing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RetroarchSaveState {
    /// Write a save state when the activity closes and load it on the next
    /// open, so the child resumes exactly where they stopped — mid-battle,
    /// mid-cutscene, wherever the session ended.
    ///
    /// Note this makes the console's own power-on screen unreachable, which is
    /// what the HUD's reset button is for.
    #[default]
    Auto,
    /// Leave save states alone. Every launch boots the content from scratch;
    /// only the in-game save carries over.
    Off,
}

impl RetroarchSaveState {
    /// Whether shepherd should turn on RetroArch's auto save-state handling.
    pub fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }
}

/// Input compatibility mode for an activity.
///
/// Some activities don't process raw touch or gamepad events from Wayland and
/// need a shim to translate input at the compositor level. Modes are mostly
/// orthogonal: an activity can stack `TouchToMouse` (or `TabletToTouch`, or
/// `DisableTouch`) with one of the `Gamepad*` modes. The touch-handling modes
/// are the exception — `TouchToMouse`, `TabletToTouch`, and `DisableTouch` all
/// grab or produce the touchscreen, so at most one of them can be active at a
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InputCompatMode {
    /// Grab touchscreens and emit synthesized pointer events via
    /// `zwlr_virtual_pointer_v1` for the lifetime of the activity.
    TouchToMouse,
    /// Grab absolute pointers / tablets and emit synthesized touch events for
    /// activities that only handle touch input — the inverse of
    /// `TouchToMouse`. Useful for developing touch support against
    /// mouse/pen-only hardware, or VMs whose pointer is an absolute tablet.
    TabletToTouch,
    /// Grab every touchscreen and discard its events for the lifetime of the
    /// activity, effectively disabling the touchscreen. Unlike `TouchToMouse`
    /// it emits nothing — useful for activities that misbehave on touch input
    /// but should still be playable with a mouse or gamepad.
    DisableTouch,
    /// Remap a gamepad to mouse + keyboard using the productivity preset:
    /// triggers = LMB, shoulders = RMB, left stick = mouse, right stick =
    /// scroll, stick-click toggles which stick drives the mouse, D-pad =
    /// arrow keys, A = Enter, Start = Escape.
    GamepadProductivity,
    /// Remap a gamepad to mouse + keyboard using the GPD/FPS preset:
    /// LT = LMB, RT = RMB, LB = MMB, left stick = WASD, right stick = mouse,
    /// D-pad = scroll, A = Space, X = R, B = E, Y = F.
    GamepadGpd,
}

impl InputCompatMode {
    /// True if this mode is one of the gamepad presets.
    pub fn is_gamepad(self) -> bool {
        matches!(self, Self::GamepadProductivity | Self::GamepadGpd)
    }

    /// True if this mode grabs or produces the touchscreen. Such modes are
    /// mutually exclusive — stacking two of them would have them fight over
    /// the same devices (e.g. two `EVIOCGRAB`s) or form a loop.
    pub fn handles_touch(self) -> bool {
        matches!(
            self,
            Self::TouchToMouse | Self::TabletToTouch | Self::DisableTouch
        )
    }
}

/// A category of physical input device an activity can depend on (issue #96).
///
/// Distinct from [`InputCompatMode`], which changes how input is *translated*
/// while an activity runs. `InputDeviceType` is a *gating* concept: an activity
/// can require one or more of these device types to be connected before it is
/// shown or launchable. The canonical example is a "learn to type" activity
/// installed on a gaming handheld that should only appear once a physical
/// keyboard is attached.
///
/// Camera/microphone and MIDI are intentionally omitted for now; the issue
/// marks them as future work and this enum is closed, so configuring one is a
/// parse error rather than a silently-ignored value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InputDeviceType {
    /// A relative pointing device (mouse, trackball, trackpad).
    Mouse,
    /// A finger touchscreen (an absolute, direct-input touch device).
    Touch,
    /// A physical alphabetic keyboard.
    Keyboard,
    /// A gamepad / game controller / joystick.
    Gamepad,
}

impl InputDeviceType {
    /// Lowercase, human-facing label ("mouse", "touch", ...). Matches the
    /// snake_case config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mouse => "mouse",
            Self::Touch => "touch",
            Self::Keyboard => "keyboard",
            Self::Gamepad => "gamepad",
        }
    }
}

impl std::fmt::Display for InputDeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a supervised browser activity launches its window.
///
/// Translated by the host adapter into Chrome command-line flags. Shared by
/// `lunchbox-config`'s validated `BrowserPolicy` and `lunchbox-host-api`'s
/// `BrowserSpec` so there is a single source of truth for the mode vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BrowserMode {
    /// Fullscreen, no browser chrome (`--kiosk`).
    Kiosk,
    /// Single application window (`--app=<url>`).
    App,
    /// Normal browser window.
    Windowed,
}

/// Per-activity tunables for input compatibility sidecars.
///
/// All fields are optional; sidecars apply their own defaults when a field is
/// `None`. Only the gamepad fields are populated today, but the struct lives
/// alongside the mode list so future tunables for other modes can be added
/// without another schema change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InputCompatOptions {
    /// Gamepad analog-stick deadzone as a fraction of full deflection (0..1).
    /// Below this magnitude the stick is treated as centered.
    pub gamepad_deadzone: Option<f32>,
    /// Gamepad mouse speed in pixels per second at full stick deflection.
    pub gamepad_mouse_speed: Option<f32>,
    /// Gamepad scroll speed in discrete wheel units per second at full
    /// deflection.
    pub gamepad_scroll_speed: Option<f32>,
}

impl InputCompatOptions {
    /// True if every field is `None`.
    pub fn is_empty(&self) -> bool {
        self.gamepad_deadzone.is_none()
            && self.gamepad_mouse_speed.is_none()
            && self.gamepad_scroll_speed.is_none()
    }
}

impl EntryKind {
    pub fn tag(&self) -> EntryKindTag {
        match self {
            EntryKind::Process { .. } => EntryKindTag::Process,
            EntryKind::Snap { .. } => EntryKindTag::Snap,
            EntryKind::Steam { .. } => EntryKindTag::Steam,
            EntryKind::Flatpak { .. } => EntryKindTag::Flatpak,
            EntryKind::Vm { .. } => EntryKindTag::Vm,
            EntryKind::Media { .. } => EntryKindTag::Media,
            EntryKind::Retroarch { .. } => EntryKindTag::Retroarch,
            EntryKind::Ebook { .. } => EntryKindTag::Ebook,
            EntryKind::Custom { .. } => EntryKindTag::Custom,
        }
    }

    /// Whether this activity can be reset in place — torn down and brought
    /// back at its starting state without ending the session.
    ///
    /// Only RetroArch entries, and only when they ask for the button. It is
    /// the save-state resume that creates the need: once every launch restores
    /// where the child left off, the console's own power-on screen is
    /// otherwise unreachable.
    pub fn supports_reset(&self) -> bool {
        matches!(self, EntryKind::Retroarch { reset: true, .. })
    }

    /// Whether the HUD's "X" should confirm before ending this activity, when
    /// the entry does not say either way (issue #78).
    ///
    /// The answer depends only on the kind, so it lives on
    /// [`EntryKindTag::confirms_on_close_by_default`] — where the config
    /// editor's copy can be generated from it instead of mirrored by hand.
    pub fn confirms_on_close_by_default(&self) -> bool {
        self.tag().confirms_on_close_by_default()
    }

    /// The input-compat sidecars this activity gets when the entry lists none
    /// (issue #160). See [`EntryKindTag::default_input_compat`].
    pub fn default_input_compat(&self) -> Vec<InputCompatMode> {
        self.tag().default_input_compat()
    }

    /// Whether the HUD should offer page-turn buttons for this activity
    /// (issue #160).
    ///
    /// Turning a page in a document reader is bound to keys, a gamepad D-pad
    /// or a scroll wheel — and a touchscreen produces none of those, while the
    /// reader itself has no swipe gesture. On a touch-only device that leaves
    /// a child on page one, so the buttons live where every activity's
    /// controls already live: shepherd's own HUD, which is on the overlay
    /// layer, always reachable, and cannot be locked out by the reader.
    pub fn supports_page_turn(&self) -> bool {
        matches!(self, EntryKind::Ebook { .. })
    }

    /// Whether a graceful stop should ask the compositor to close this
    /// activity's window before it signals the process (issue #160).
    ///
    /// A `SIGTERM` is a request to *die*; closing the window is a request to
    /// *finish*. Applications that save on window close and install no signal
    /// handler — Okular is the measured case, and it is a large class — lose
    /// everything to the signal-only path, so for them the close request is
    /// the difference between remembering the page and starting over.
    ///
    /// Opt-in per kind rather than universal, because a close request is not
    /// always a quit:
    ///
    /// - **Steam** treats it as "hide to tray", so the close would be ignored
    ///   and the stop would only get slower.
    /// - **RetroArch** already has a verified single-`SIGTERM` shutdown that
    ///   writes its save state (#125). Nothing is broken there to fix.
    /// - **Snap and flatpak** activities are signalled through their own
    ///   cgroups, which is a different lever with its own reasons.
    pub fn wants_polite_close(&self) -> bool {
        matches!(self, EntryKind::Ebook { .. })
    }
}

/// A token gate's current state, for caregiver UIs (issue #8).
///
/// Banked time is a currency: source activities earn it and the gated activity
/// spends it. Without this a management UI can only report that something is
/// locked, never how close it is to unlocking, and a manual grant would be
/// made blind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TokenStatus {
    /// Time banked and not yet spent.
    pub balance: Duration,
    /// Balance needed to open the gate. Zero means any balance opens it.
    pub minimum: Duration,
    /// Whether the gate is open right now: the balance is above zero and at
    /// least `minimum`. That holds every time, not only the first (issue #193).
    pub unlocked: bool,
    /// Ceiling on the balance. None means unlimited. A grant past this is
    /// clawed back, so a UI should say so rather than let it vanish.
    pub max_balance: Option<Duration>,
    /// Whether the balance survives local midnight.
    pub carry_over: bool,
}

/// View of an entry for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntryView {
    pub entry_id: EntryId,
    pub label: String,
    pub icon_ref: Option<String>,
    pub kind_tag: EntryKindTag,
    pub enabled: bool,
    /// The group this entry belongs to (issue #5), if any. Management UIs use
    /// it to show that an activity's schedule and budget are shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupId>,
    pub reasons: Vec<ReasonCode>,
    /// The entry's own token gate (issue #8), if it has one. A member of a
    /// token-gated group carries its own gate only; the category's is on the
    /// `GroupView`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenStatus>,
    /// Maximum run duration if started now. None means:
    /// - If enabled=false: entry is not available
    /// - If enabled=true: entry has no time limit (unlimited)
    pub max_run_if_started_now: Option<Duration>,
}

/// View of a group for UI display (issue #5).
///
/// A group's limits are shared by its members, so a management UI needs to
/// show the *category's* state — combined usage against the combined quota,
/// and whatever is currently restricting it — separately from any one member.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GroupView {
    pub group_id: GroupId,
    pub label: String,
    /// Members, in policy order.
    pub member_ids: Vec<EntryId>,
    /// Whether the group's own restrictions currently permit its members.
    /// Individual members may still be unavailable for their own reasons.
    pub enabled: bool,
    /// Why the group is restricting its members, if it is. These are the
    /// unwrapped reasons — the same ones members carry inside
    /// `ReasonCode::GroupRestricted`.
    pub reasons: Vec<ReasonCode>,
    /// Combined usage across all members today.
    pub used_today: Duration,
    /// Effective daily quota after any override delta. None means unlimited.
    pub daily_quota: Option<Duration>,
    /// Longest session the group's limits would currently allow a member.
    /// None means the group imposes no cap of its own.
    pub max_run_if_started_now: Option<Duration>,
    /// The category's token gate (issue #8), if it has one. Shared by every
    /// member, so it belongs here rather than on any one of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenStatus>,
}

/// Structured reason codes for why an entry is unavailable
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReasonCode {
    /// Outside allowed time window
    OutsideTimeWindow {
        /// When the next window opens (if known)
        next_window_start: Option<DateTime<Local>>,
    },
    /// Daily quota exhausted
    QuotaExhausted { used: Duration, quota: Duration },
    /// Cooldown period active
    CooldownActive { available_at: DateTime<Local> },
    /// Another session is active
    SessionActive {
        entry_id: EntryId,
        /// Time remaining in current session. None means unlimited.
        remaining: Option<Duration>,
    },
    /// Host doesn't support this entry kind
    UnsupportedKind { kind: EntryKindTag },
    /// The activity kind has not finished warming up yet (e.g. Steam is still
    /// performing its initial load). See per-kind readiness (issue #76).
    NotReady { kind: EntryKindTag },
    /// Entry is explicitly disabled
    Disabled { reason: Option<String> },
    /// Internet connectivity is required but unavailable
    InternetUnavailable { check: Option<String> },
    /// Entry is manually disabled for the day via a daily override
    ManuallyDisabled { until: NaiveDate },
    /// The device is in administrator mode (issue #154), so nothing launches as
    /// an activity. Not a restriction on the child in the sense the others are:
    /// it clears the moment the caregiver leaves the mode, and it applies to
    /// every entry at once.
    AdminMode,
    /// One or more required input devices (issue #96) are not currently
    /// connected. `devices` lists the missing device types, sorted and
    /// deduplicated.
    RequiredInputUnavailable { devices: Vec<InputDeviceType> },
    /// A protection this entry's configuration requires cannot be applied on
    /// this host, so the entry does not launch (issue #143) — today, an
    /// `[entries.firewall]` on a host where enforcement is unavailable.
    ///
    /// Carries no detail on purpose. This is the child-facing half: to them the
    /// activity is simply unavailable, and nothing they can do changes it. The
    /// administrator-facing half — which protection, why, and how to fix it —
    /// is the matching `Diagnostic`.
    ProtectionUnavailable,
    /// Not enough time banked on this entry's token gate (issue #8): the
    /// activity has to be earned by spending time on its source activities.
    TokensInsufficient {
        /// Time currently banked toward this entry.
        balance: Duration,
        /// Balance needed before it unlocks. Zero means any balance above zero
        /// unlocks it, i.e. the entry is simply out of banked time.
        required: Duration,
    },
    /// The restriction comes from the entry's group rather than the entry
    /// itself (issue #5) — e.g. the whole category's daily quota is spent.
    /// `label` is the group's display name, for explaining it to a caregiver.
    GroupRestricted {
        group: GroupId,
        label: String,
        reason: Box<ReasonCode>,
    },
}

/// Warning severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    Info,
    Warn,
    Critical,
}

/// Warning threshold configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WarningThreshold {
    /// Seconds before expiry to issue this warning
    pub seconds_before: u64,
    pub severity: WarningSeverity,
    pub message_template: Option<String>,
}

/// Session end reason
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEndReason {
    /// Session expired (time limit reached)
    Expired,
    /// User requested stop
    UserStop,
    /// Admin requested stop
    AdminStop,
    /// Process exited on its own
    ProcessExited { exit_code: Option<i32> },
    /// Policy change terminated session
    PolicyStop,
    /// Service shutdown
    ServiceShutdown,
    /// Launch failed
    LaunchFailed { error: String },
}

/// Current session state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Approved and spawning; the activity has not mapped a window yet.
    Launching,
    /// The activity is running normally.
    Running,
    /// Running, and at least one time warning has been issued.
    Warned,
    /// Past its deadline and being wound down.
    Expiring,
    /// Teardown has been requested and the activity is being stopped.
    ///
    /// The session is still current: the activity is on screen until the host
    /// confirms otherwise, so nothing else may launch and shells must keep the
    /// launcher out of the way. Shells should render this as a
    /// non-interactive "closing" state — without it a child gets no feedback
    /// that their press registered, which is why they pressed again on
    /// 2026-08-20 (issue #136).
    Stopping,
    /// Settled and cleared; no activity is running.
    Ended,
}

/// Active session information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub entry_id: EntryId,
    pub label: String,
    pub state: SessionState,
    pub started_at: DateTime<Local>,
    /// Session deadline. None means unlimited (no time limit).
    pub deadline: Option<DateTime<Local>>,
    /// Time remaining. None means unlimited.
    pub time_remaining: Option<Duration>,
    pub warnings_issued: Vec<u64>,
    /// Whether the HUD should confirm before its "X" button ends this
    /// session (issue #78). Defaults to `true` when absent so older payloads
    /// keep the safe behaviour.
    #[serde(default = "default_confirm_on_close")]
    pub confirm_on_close: bool,
    /// Whether the HUD should offer a reset button for this session — see
    /// [`EntryKind::supports_reset`]. Defaults to `false` when absent, so an
    /// older payload simply doesn't show the button.
    #[serde(default)]
    pub can_reset: bool,
    /// Whether the HUD should show page-turn buttons for this session. See
    /// [`EntryKind::supports_page_turn`].
    #[serde(default)]
    pub can_turn_pages: bool,
}

/// Default for [`SessionInfo::confirm_on_close`] / the `SessionStarted` event:
/// confirmation is enabled unless a config explicitly opts out.
pub(crate) fn default_confirm_on_close() -> bool {
    true
}

/// Status of a single internet connectivity check target
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InternetStatusView {
    /// Original check string as configured (e.g. "https://example.com")
    pub target: String,
    /// Whether the last check succeeded
    pub available: bool,
}

/// Full service state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ServiceStateSnapshot {
    pub api_version: u32,
    pub policy_loaded: bool,
    pub current_session: Option<SessionInfo>,
    pub entry_count: usize,
    /// Available entries for UI display
    #[serde(default)]
    pub entries: Vec<EntryView>,
    /// Latest known status of each configured internet connectivity check.
    /// Empty when no connectivity checks are configured.
    #[serde(default)]
    pub internet_status: Vec<InternetStatusView>,
    /// Administrator-facing conditions currently true of this device (issue
    /// #143) — a missing dependency, a protection that is not in effect. Rides
    /// the snapshot so every client has the current set on subscribe; deltas
    /// arrive as `EventPayload::DiagnosticsChanged`.
    #[serde(default)]
    pub diagnostics: crate::DiagnosticSet,
    /// Whether the device is in administrator mode (issue #154) — the kiosk's
    /// restrictions relaxed so a caregiver can set activities up in place.
    ///
    /// Every client that behaves differently in the mode reads it from here
    /// rather than tracking it: the shells change what they draw, and the
    /// window panels stop calling admin-launched windows orphans. (The screen
    /// staying awake is not one of them — that check moved inside the daemon
    /// with issue #144, and `set_screen_power` reads the engine directly.)
    /// Absent from an older payload means "not in admin mode", which is the
    /// safe reading.
    #[serde(default)]
    pub admin_mode: bool,
    /// Whether the screen is locked (issue #154).
    ///
    /// Only ever set inside administrator mode: it is what makes walking away
    /// from a half-configured device safe, and it is deliberately not something
    /// a child's session can enter. Clearing it is a management RPC — there is
    /// no local affordance, which is the entire point.
    #[serde(default)]
    pub locked: bool,
}

/// Role for authorization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    /// UI/HUD - can view state, launch entries, stop current
    Shell,
    /// Local admin - can also extend, reload config
    Admin,
    /// Read-only observer
    Observer,
}

impl ClientRole {
    pub fn can_launch(&self) -> bool {
        matches!(self, ClientRole::Shell | ClientRole::Admin)
    }

    pub fn can_stop(&self) -> bool {
        matches!(self, ClientRole::Shell | ClientRole::Admin)
    }

    pub fn can_extend(&self) -> bool {
        matches!(self, ClientRole::Admin)
    }

    pub fn can_reload_config(&self) -> bool {
        matches!(self, ClientRole::Admin)
    }
}

/// Stop mode for session termination
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StopMode {
    /// Try graceful termination first
    Graceful,
    /// Force immediate termination
    Force,
}

/// Health status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HealthStatus {
    pub live: bool,
    pub ready: bool,
    pub policy_loaded: bool,
    pub host_adapter_ok: bool,
    pub store_ok: bool,
}

/// What kind of thing an audio output is.
///
/// Advisory only: it drives presentation (an icon, a label) and never policy.
/// It cannot be determined for every device — a generic USB interface reports a
/// nondescript `analog-output` route and no udev form-factor — so `Unknown` is a
/// routine outcome, not a failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AudioOutputKind {
    Speakers,
    Headphones,
    Hdmi,
    Digital,
    LineOut,
    Bluetooth,
    #[default]
    Unknown,
}

/// The audio output a volume reading applies to.
///
/// `key` is `<device.name>:output:<route.name>` — the same key WirePlumber uses
/// to persist per-route volume, so our notion of "an output" cannot drift from
/// the volume PipeWire remembers for it. It is stable across reboots and, for
/// USB devices, across being moved to a different port.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AudioOutput {
    /// Stable identity. Use this to correlate, never the description.
    pub key: String,
    /// Human-readable label for display. Localized and mutable.
    pub description: String,
    pub kind: AudioOutputKind,
}

/// An audio output the device has seen, together with any per-output volume
/// limit the parent has set for it.
///
/// These rows are how per-output limits are configured: lunchboxd records every
/// output it observes, the management UIs list them, and the parent sets a cap
/// on the row they recognise. Nothing has to be predicted or hand-written —
/// which matters because an output often cannot be classified at all (see
/// [`AudioOutputKind`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AudioOutputRecord {
    /// Identity, display label, and advisory kind.
    pub output: AudioOutput,
    /// Cap for this output. `None` means no per-output cap; the global
    /// `[service.volume]` limit applies instead.
    pub max_volume: Option<u8>,
    /// Floor for this output. `None` means no per-output floor.
    pub min_volume: Option<u8>,
    /// When the device last observed this output. Lets the UI show recently used
    /// devices first and lets a parent prune ones that are long gone.
    pub last_seen: DateTime<Local>,
    /// Whether this is the output currently selected. Runtime state, not stored.
    pub active: bool,
    /// Whether the device is plugged in right now, so it can be switched to.
    ///
    /// Rows outlive the hardware — that is the point, so a cap set on the
    /// headphones survives unplugging them — which means a row can name a device
    /// that is not here. Defaults to `true` so a client talking to a daemon that
    /// predates this field offers the choice and lets the attempt fail loudly,
    /// rather than greying out every device it could actually switch to.
    #[serde(default = "default_true")]
    pub available: bool,
}

fn default_true() -> bool {
    true
}

/// Volume status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VolumeInfo {
    /// Volume percentage (0-100)
    pub percent: u8,
    /// Whether audio is muted
    pub muted: bool,
    /// Whether volume control is available
    pub available: bool,
    /// The detected sound backend (e.g., "pipewire", "pulseaudio", "alsa")
    pub backend: Option<String>,
    /// Current restrictions on volume
    pub restrictions: VolumeRestrictions,
    /// The output this reading applies to. `None` on hosts without PipeWire, or
    /// when the default sink cannot be resolved to a known output.
    #[serde(default)]
    pub output: Option<AudioOutput>,
}

/// Volume restrictions that are currently in effect
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VolumeRestrictions {
    /// Maximum volume percentage allowed
    pub max_volume: Option<u8>,
    /// Minimum volume percentage allowed
    pub min_volume: Option<u8>,
    /// Whether mute toggle is allowed
    pub allow_mute: bool,
    /// Whether volume changes are allowed at all
    pub allow_change: bool,
}

impl VolumeRestrictions {
    /// Create unrestricted volume settings
    pub fn unrestricted() -> Self {
        Self {
            max_volume: None,
            min_volume: None,
            allow_mute: true,
            allow_change: true,
        }
    }

    /// Clamp a volume value to the allowed range
    pub fn clamp_volume(&self, percent: u8) -> u8 {
        let min = self.min_volume.unwrap_or(0);
        let max = self.max_volume.unwrap_or(100);
        percent.clamp(min, max)
    }
}

impl VolumeInfo {
    /// Get an icon name for the current volume status
    pub fn icon_name(&self) -> &'static str {
        if self.muted || self.percent == 0 {
            "audio-volume-muted-symbolic"
        } else if self.percent < 33 {
            "audio-volume-low-symbolic"
        } else if self.percent < 66 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-high-symbolic"
        }
    }
}

/// Screen brightness status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BrightnessInfo {
    /// Brightness percentage (0-100)
    pub percent: u8,
    /// Whether brightness control is available on this host
    pub available: bool,
    /// The detected brightness backend (e.g. "sysfs", "brightnessctl")
    pub backend: Option<String>,
    /// Name of the backlight device being controlled, if any
    pub device: Option<String>,
    /// Current restrictions on brightness
    pub restrictions: BrightnessRestrictions,
    /// Whether an ambient light sensor is present, so automatic brightness
    /// can be offered at all. When false, `auto_enabled` is always false.
    #[serde(default)]
    pub auto_available: bool,
    /// Whether automatic (ambient-light) brightness is currently enabled.
    #[serde(default)]
    pub auto_enabled: bool,
}

/// Brightness restrictions that are currently in effect
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BrightnessRestrictions {
    /// Maximum brightness percentage allowed
    pub max_brightness: Option<u8>,
    /// Minimum brightness percentage allowed
    pub min_brightness: Option<u8>,
    /// Whether brightness changes are allowed at all
    pub allow_change: bool,
}

impl BrightnessRestrictions {
    /// Create unrestricted brightness settings
    pub fn unrestricted() -> Self {
        Self {
            max_brightness: None,
            min_brightness: None,
            allow_change: true,
        }
    }

    /// Clamp a brightness value to the allowed range
    pub fn clamp_brightness(&self, percent: u8) -> u8 {
        let min = self.min_brightness.unwrap_or(0);
        let max = self.max_brightness.unwrap_or(100);
        percent.clamp(min, max)
    }
}

impl BrightnessInfo {
    /// Get an icon name for the current brightness status.
    //
    // Adwaita and Yaru only ship a single `display-brightness-symbolic`
    // glyph; the percentage-tiered `*-low/medium/high-symbolic` names
    // that older GNOME themes used no longer resolve, so the HUD would
    // render the missing-image placeholder if we returned those.
    pub fn icon_name(&self) -> &'static str {
        "display-brightness-symbolic"
    }
}

/// A compositor output video mode: pixel resolution and refresh rate.
///
/// `refresh_mhz` is millihertz, matching sway's `get_outputs` JSON (60 Hz is
/// `60000`). Refresh participates in equality, but [`VideoMode::area`] ignores
/// it so "highest resolution" comparisons are purely by pixel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub refresh_mhz: u32,
}

impl VideoMode {
    /// Pixel area, used to rank modes by resolution.
    pub fn area(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// Which screen edge the HUD occupies (issue #171).
///
/// Configurable globally under `[service.hud]` and per entry, because the
/// right answer depends on both the hardware (a tall panel gives up less to a
/// side bar) and the activity (a game whose own UI lives along the top).
///
/// The vertical form is "the HUD rotated 90 degrees to the left": same
/// controls, same order, read bottom-to-top with the end-session button at the
/// top. `Right` is deliberately not offered yet — nothing in the layout
/// forecloses it, but no config or code path ships for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HudOrientation {
    /// A horizontal bar along the top edge. The default, and what every device
    /// shipped before issue #171 uses.
    #[default]
    Top,
    /// A horizontal bar along the bottom edge.
    Bottom,
    /// A vertical bar down the left edge.
    Left,
}

impl HudOrientation {
    /// Whether the bar runs down the screen rather than across it.
    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Left)
    }
}

/// How the kiosk drives displays when an external monitor is docked (issue #87).
///
/// Exactly one logical output is ever active in every variant, so the
/// one-activity-at-a-time invariant always holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    /// Only the internal/primary panel is active — the state when no external
    /// display is connected.
    SingleInternal,
    /// The external display mirrors the primary. Default whenever an external
    /// display connects.
    Mirror,
    /// The primary panel is disabled and the external display drives the
    /// session at its native resolution.
    ExternalOnly,
}

impl DisplayMode {
    /// The mode the HUD toggle flips to from the current one. `Mirror` and
    /// `ExternalOnly` toggle between each other; `SingleInternal` has no
    /// external display to toggle, so it maps to itself.
    pub fn toggled(self) -> Self {
        match self {
            DisplayMode::Mirror => DisplayMode::ExternalOnly,
            DisplayMode::ExternalOnly => DisplayMode::Mirror,
            DisplayMode::SingleInternal => DisplayMode::SingleInternal,
        }
    }
}

/// Snapshot of the compositor's display arrangement, broadcast to shells so the
/// HUD can show/hide and label its mirror/external toggle (issue #87).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DisplayState {
    pub mode: DisplayMode,
    /// Connector name of the primary (internal, first-enumerated) output.
    pub primary: Option<String>,
    /// Connector name of the external/secondary output, if one is connected.
    pub secondary: Option<String>,
}

impl DisplayState {
    /// True when an external display is connected — the condition under which
    /// the HUD reveals its mode toggle.
    pub fn has_secondary(&self) -> bool {
        self.secondary.is_some()
    }
}

/// A parent-set daily override for a single entry
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DailyOverride {
    /// What the override applies to: an entry, or a whole group (issue #5).
    /// Serializes as a bare entry ID, or `group:<id>` for a group, so overrides
    /// written before groups existed round-trip unchanged.
    pub subject: LimitSubject,
    pub date: NaiveDate,
    /// Override the entry's availability for this day.
    /// `Some(false)` blocks it entirely; `Some(true)` allows it outside its time window.
    /// `None` means no availability override (quota delta may still apply).
    pub availability: Option<bool>,
    /// Signed adjustment to today's quota in seconds.
    /// Positive = extra time; negative = reduced time; `None` = no change.
    pub quota_delta_seconds: Option<i64>,
    pub created_at: DateTime<Local>,
    pub updated_at: DateTime<Local>,
}

/// Screen-time usage for a single entry on a single day
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageStat {
    pub entry_id: EntryId,
    pub label: String,
    pub date: NaiveDate,
    pub duration_seconds: u64,
}

/// One launchable application from the system's `.desktop` files, as
/// administrator mode's app picker sees it (issue #154).
///
/// Enumerated by `lunchbox_config::desktop`, which does the Desktop Entry
/// parsing; this is only the shape that crosses the wire. Deliberately carries
/// no `Exec`: what a client may do is ask for an id to be launched, not hand
/// the daemon a command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DesktopApp {
    /// The desktop file ID — the path relative to its `applications`
    /// directory with `/` replaced by `-`, e.g. `org.kde.krita.desktop`. The
    /// spec's own identifier, and what `launch_desktop_app` takes.
    pub id: String,
    /// Display name, localized to the device's locale where the file offers a
    /// translation.
    pub name: String,
    /// One-line description (`Comment`), localized the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Icon theme name or absolute path, straight from `Icon`. Resolved by
    /// whichever toolkit draws it, exactly as for a configured entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Whether the application expects a terminal emulator. Shown so a picker
    /// can mark it: on a device with no terminal installed, launching one of
    /// these fails, and saying so up front beats a launch that appears to do
    /// nothing.
    pub terminal: bool,
}

/// An action that can be performed on a window through the management API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    /// Ask the window to close (sway `kill`).
    Close,
    /// Move the window to the scratchpad to hide it from view.
    Hide,
    /// Pull the window out of the scratchpad so it is shown again.
    Show,
    /// Give the window keyboard focus, raising it above the others (sway
    /// `focus`).
    ///
    /// Note that on a window currently *on* the scratchpad this also pulls it
    /// off — verified against sway 1.11, where focusing a stashed window
    /// clears `in_scratchpad` and makes it visible — so it overlaps
    /// [`WindowAction::Show`] for that case rather than being a no-op.
    /// Clients that list the two placements separately should therefore still
    /// offer `Show` on a scratchpad row and `Focus` on an on-screen one, so
    /// each row has one obvious action, not because `Focus` would fail there.
    Focus,
}

/// Who shepherd believes a window belongs to.
///
/// The compositor cannot answer this — it reports pids, not intent. The host
/// fills it in by matching each window against what it is actually
/// supervising, which is what lets an admin UI tell "the game the child is
/// playing" apart from "something on the screen that no session owns".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WindowOwner {
    /// Shepherd's own furniture: the launcher, the HUD, the pairing UI, the
    /// mirror, and background processes it keeps warm (the preloaded Steam
    /// client). Expected to outlive every session.
    Shepherd,
    /// A process shepherd is supervising for the current session — the
    /// activity itself, something in its process group, a Steam game
    /// launched on its behalf, or one of its input sidecars.
    Activity,
    /// An activity that outlived its own teardown. Its session is over and
    /// the host is still working on killing it — the same condition that
    /// writes an `ActivityEscaped` audit record.
    Escaped,
    /// No process shepherd knows about. Either something started outside
    /// shepherd entirely, or an activity that got away without the host ever
    /// noticing — the case supervision cannot fix on its own, and the reason
    /// this field exists.
    Unowned,
}

/// Debug snapshot of a single window known to the host's compositor.
///
/// Currently surfaced via the management API for debugging the Sway tree —
/// in particular, to see which windows have been moved to the scratchpad
/// (e.g. the hidden Steam client) versus which are on-screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WindowInfo {
    /// Compositor-assigned window/container id.
    pub id: u64,
    /// Window title, if the application set one.
    pub name: Option<String>,
    /// Wayland app_id, if available.
    pub app_id: Option<String>,
    /// X11 class (xwayland windows), if available.
    pub window_class: Option<String>,
    /// Owning process id, if reported by the compositor.
    pub pid: Option<u32>,
    /// Workspace name the window belongs to, if any. `__i3_scratch` is the
    /// scratchpad pseudo-workspace.
    pub workspace: Option<String>,
    /// True if the window currently lives on the scratchpad (hidden).
    pub in_scratchpad: bool,
    /// True if the window is currently being rendered.
    pub visible: bool,
    /// True if the window has keyboard focus.
    pub focused: bool,
    /// What shepherd is supervising behind this window, if anything.
    ///
    /// A host that cannot attribute windows reports every one of them as
    /// unowned.
    pub owner: WindowOwner,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire spellings the web UI and the companion app switch on. Both
    /// clients hard-code these strings, and a silent rename here would
    /// downgrade every orphan to an ordinary row rather than failing.
    #[test]
    fn window_owner_wire_spelling() {
        let spellings = [
            (WindowOwner::Shepherd, "\"shepherd\""),
            (WindowOwner::Activity, "\"activity\""),
            (WindowOwner::Escaped, "\"escaped\""),
            (WindowOwner::Unowned, "\"unowned\""),
        ];
        for (owner, json) in spellings {
            assert_eq!(serde_json::to_string(&owner).unwrap(), json);
            assert_eq!(serde_json::from_str::<WindowOwner>(json).unwrap(), owner);
        }
    }

    /// Both clients switch on these strings, and the companion pins the same
    /// four from the Kotlin side (`WireTest`). A rename now fails twice rather
    /// than silently turning a button into a no-op.
    #[test]
    fn window_action_wire_spelling() {
        let spellings = [
            (WindowAction::Close, "\"close\""),
            (WindowAction::Hide, "\"hide\""),
            (WindowAction::Show, "\"show\""),
            (WindowAction::Focus, "\"focus\""),
        ];
        for (action, json) in spellings {
            assert_eq!(serde_json::to_string(&action).unwrap(), json);
            assert_eq!(serde_json::from_str::<WindowAction>(json).unwrap(), action);
        }
    }

    #[test]
    fn entry_kind_serialization() {
        let kind = EntryKind::Process {
            command: "scummvm".into(),
            args: vec!["-f".into()],
            env: HashMap::new(),
            cwd: None,
        };

        let json = serde_json::to_string(&kind).unwrap();
        let parsed: EntryKind = serde_json::from_str(&json).unwrap();

        assert_eq!(kind, parsed);
    }

    #[test]
    fn only_retroarch_entries_that_ask_for_it_support_reset() {
        let retroarch = |reset| EntryKind::Retroarch {
            core: Some("mgba".into()),
            core_path: None,
            content: "/roms/game.gba".into(),
            save_state: RetroarchSaveState::Auto,
            command: "retroarch".into(),
            args: vec![],
            env: HashMap::new(),
            kiosk: true,
            reset,
        };

        assert!(retroarch(true).supports_reset());
        assert!(!retroarch(false).supports_reset());
        // Nothing else can be restarted in place: the host has no way to put
        // another kind back at a meaningful starting state.
        assert!(
            !EntryKind::Process {
                command: "scummvm".into(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            }
            .supports_reset()
        );
    }

    #[test]
    fn reason_code_serialization() {
        let reason = ReasonCode::QuotaExhausted {
            used: Duration::from_secs(3600),
            quota: Duration::from_secs(3600),
        };

        let json = serde_json::to_string(&reason).unwrap();
        assert!(json.contains("quota_exhausted"));
    }
}
