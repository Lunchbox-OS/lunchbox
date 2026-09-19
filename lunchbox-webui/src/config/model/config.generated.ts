// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from `crates/lunchbox-config/src/schema.rs` by
// `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`.
// Edit the Rust types and re-run instead.
//
// These describe the *projection* the editor renders — what serde produces
// when the daemon's parser reads a file — not TOML syntax. Where the file
// format accepts a shorthand (`input_compat = "touch_to_mouse"` as well as a
// list), the projection always carries the canonical form shown here.

/**
 * How an [`EntryKind::Ebook`] activity lays pages out.
 */
export type EbookLayout =
  /**
   * Two pages side by side, like an open book. Fits a landscape panel: a
   * single portrait page fitted to 16:9 is letterboxed and small.
   */
  | "facing"
  /**
   * The same, with the first page alone — so the spreads fall where a
   * printed book's would, cover on its own and chapter openings on the
   * right. The default: it costs nothing over `facing` and matches what a
   * child holding a paper book expects.
   */
  | "facing_first_centered"
  /**
   * One page at a time. The right choice on a portrait screen.
   */
  | "single"
  /**
   * One continuous column, scrolled rather than paged, fitted to the width.
   *
   * The only layout a **touch-only** device can navigate: dragging scrolls
   * it. The paged layouts turn the page on a key, a gamepad D-pad or a
   * scroll wheel, and a touchscreen produces none of those — Okular grabs
   * only the pinch gesture, and has no swipe-to-turn anywhere in its
   * desktop view.
   */
  | "scroll";

/**
 * Which reader an [`EntryKind::Ebook`] activity drives.
 *
 * Open rather than closed on purpose: the config surface here — a book and a
 * place in it — is reader-agnostic, even though only one reader is wired up.
 */
export type EbookViewer =
  /**
   * Okular (`okular`), with `okular-extra-backends` for EPUB. Covers EPUB,
   * PDF, CBZ, DjVu and FictionBook, and is the only reader in Ubuntu with a
   * documented way to disable its own escape hatches.
   */
  | "okular";

/**
 * Automatic screen-brightness configuration (ambient-light driven).
 */
export interface RawAutoBrightnessConfig {
  /**
   * Ambient light (lux) at or above which the screen sits at `max_percent`.
   */
  bright_lux?: number | null;
  /**
   * Ambient light (lux) at or below which the screen sits at `min_percent`.
   */
  dim_lux?: number | null;
  /**
   * Whether automatic brightness starts enabled. This is only the default;
   * the runtime state (toggled from the HUD or management API) is persisted
   * and takes precedence once set.
   */
  enabled?: boolean;
  /**
   * Brightness percent at the bright end of the curve (0-100).
   */
  max_percent?: number | null;
  /**
   * Brightness percent at the dim end of the curve (0-100).
   */
  min_percent?: number | null;
  /**
   * How often to sample the light sensor, in seconds.
   */
  poll_interval_seconds?: number | null;
}

/**
 * Availability configuration
 */
export interface RawAvailability {
  /**
   * If true, entry is always available (ignores windows)
   */
  always?: boolean;
  /**
   * Time windows when entry is available
   */
  windows?: RawTimeWindow[];
}

/**
 * Bluetooth LE management transport configuration.
 */
export interface RawBleManagementConfig {
  /**
   * Which Bluetooth controller to serve on, when the host has more
   * than one.
   *
   * Accepts a controller address (`"DC:56:7B:1F:7D:EA"`, preferred)
   * or an interface name (`"hci1"`). Defaults to whichever adapter
   * BlueZ lists first, which is **not** stable: the index tracks
   * probe order, so re-plugging a dongle, a rebind, or a boot that
   * enumerates USB differently can silently move the daemon onto the
   * other radio. The address is burned into the controller and is the
   * only identifier BlueZ exposes that both distinguishes adapters
   * and survives that.
   */
  adapter?: string | null;
  /**
   * Advertised local name and the device name returned in `DeviceInfo`.
   * Defaults to `"lunchbox"`. Pick something the companion app can
   * disambiguate when multiple Lunchbox devices are in range.
   */
  device_name?: string | null;
  /**
   * Whether the BLE management transport is enabled (default: false).
   */
  enabled?: boolean;
}

/**
 * Screen-brightness control configuration
 */
export interface RawBrightnessConfig {
  /**
   * Whether brightness changes are allowed at all (default: true)
   */
  allow_change?: boolean;
  /**
   * Automatic (ambient-light) brightness. Only honored under
   * `[service.brightness]`; a copy on a per-entry `[entries.brightness]`
   * override is ignored, since auto brightness is a device-global mode.
   */
  auto?: RawAutoBrightnessConfig | null;
  /**
   * Maximum brightness percentage allowed (0-100)
   */
  max_brightness?: number | null;
  /**
   * Minimum brightness percentage allowed (0-100).
   * Use this to prevent the screen from being driven all the way to 0%
   * (which most panels interpret as "off" — confusing for a child user).
   */
  min_brightness?: number | null;
}

/**
 * Per-entry supervised-browser policy.
 *
 * Materialized at spawn time into a Chromium [managed-policy JSON][policies]
 * file plus a set of Chrome command-line flags. Hostname allowlisting is
 * enforced by the browser itself via `URLAllowlist`/`URLBlocklist` (no
 * extensions); pair with [`RawFirewallConfig`] for coarse IP-layer
 * defense-in-depth. lunchbox-launcher only wraps Chrome through documented
 * controls — it does not patch the browser or circumvent any protections.
 *
 * [policies]: https://chromeenterprise.google/policies/
 */
export interface RawBrowserConfig {
  /**
   * Disable DevTools (`DeveloperToolsDisabled`). Default true.
   */
  disable_dev_tools?: boolean;
  /**
   * Block extension installation (`ExtensionInstallBlocklist = ["*"]`).
   * Default true.
   */
  disable_extensions?: boolean;
  /**
   * Disable incognito mode (`IncognitoModeAvailability`). Default true.
   */
  disable_incognito?: boolean;
  /**
   * Window mode: "kiosk"/"app" both open a chromeless window (no tabs or
   * omnibox) via Chrome's `--app`, or "windowed" (normal browser window).
   * Default "kiosk". Note: Lunchbox's sway compositor denies clients true
   * fullscreen to keep the HUD visible, so "kiosk" does not use `--kiosk`
   * (which would fall back to a toolbar'd window); it behaves like "app".
   */
  mode?: string;
  /**
   * Filesystem segment selecting the on-disk user-data-dir. Entries that
   * share a `profile_id` share cookies/logins; each unique id is isolated.
   * Must be a single safe path segment (no separators, not `.`/`..`).
   */
  profile_id: string;
  /**
   * URL opened on launch. Must be an http(s) URL when set.
   */
  start_url?: string | null;
  /**
   * Chromium `URLAllowlist` patterns. Empty = no allowlist (all URLs
   * permitted, subject to `url_blocklist`).
   */
  url_allowlist?: string[];
  /**
   * Chromium `URLBlocklist` patterns, applied after the allowlist.
   */
  url_blocklist?: string[];
  /**
   * Wipe the on-disk profile directory after the session ends (handled by
   * the host adapter's post-exit cleanup, not by Chrome). Default false.
   */
  wipe_on_exit?: boolean;
}

/**
 * Raw configuration as parsed from TOML
 */
export interface RawConfig {
  /**
   * Config schema version
   */
  config_version: number;
  /**
   * List of allowed entries
   */
  entries?: RawEntry[];
  /**
   * Groups of entries sharing a schedule and limits (issue #5)
   */
  groups?: RawGroup[];
  /**
   * Global service settings
   */
  service?: RawServiceConfig;
}

/**
 * Days specification
 */
export type RawDays = string | string[];

/**
 * External monitor / docking settings (issue #87).
 */
export interface RawDisplayConfig {
  /**
   * Master switch for docking support. When false, lunchboxd leaves display
   * configuration entirely to sway (default: true).
   */
  docking_enabled?: boolean;
  /**
   * Route audio to the external video device while a secondary display is in
   * use, in both mirror and external-only modes (default: true).
   */
  mirror_audio?: boolean;
}

/**
 * Raw entry definition
 */
export interface RawEntry {
  /**
   * Availability time windows
   */
  availability?: RawAvailability | null;
  /**
   * Screen-brightness restrictions for this entry (overrides global)
   */
  brightness?: RawBrightnessConfig | null;
  /**
   * Supervised-browser policy (Chromium enterprise policy + profile).
   * Compose with `kind = { type = "flatpak", app_id = "com.google.Chrome" }`
   * and an optional `[entries.firewall]` to build the web-browser activity.
   */
  browser?: RawBrowserConfig | null;
  /**
   * Ask for confirmation before the HUD "X" (End session) button ends this
   * activity. Since the button is easy to hit by accident and many
   * activities lose unsaved state when force-closed, the HUD shows a
   * confirmation prompt first (issue #78). Only affects the "X" button —
   * closing via the API, time expiration, or the process exiting is
   * unaffected.
   *
   * Absent, the default comes from the entry's kind: on for everything that
   * can lose work, off for `ebook`, which cannot — see
   * [`lunchbox_api::EntryKind::confirms_on_close_by_default`]. Set it
   * explicitly to override that either way.
   */
  confirm_on_close?: boolean | null;
  /**
   * Explicitly disabled
   */
  disabled?: boolean;
  /**
   * Reason for disabling
   */
  disabled_reason?: string | null;
  /**
   * Network firewall rules applied while this entry is running
   */
  firewall?: RawFirewallConfig | null;
  /**
   * Group this entry belongs to (issue #5). The group's schedule and limits
   * apply on top of this entry's own; the strictest of each wins.
   */
  group?: string | null;
  /**
   * Put the HUD on a different screen edge while this activity runs
   * (issue #171). Absent, the activity inherits `[service.hud]`.
   *
   * Unlike `confirm_on_close` this has no kind-dependent default: which
   * edge suits an activity is a property of its own UI and of the panel it
   * runs on, not of how it is launched, so nothing is inferred from the
   * entry kind.
   */
  hud_orientation?: RawHudOrientation | null;
  /**
   * Icon reference (opaque, interpreted by shell)
   */
  icon?: string | null;
  /**
   * Unique stable ID
   */
  id: string;
  /**
   * Input compatibility modes for this entry. Each mode runs an
   * orthogonal sidecar — touch-to-mouse and gamepad presets can be
   * stacked. Accepts a single string (`input_compat = "touch_to_mouse"`)
   * or a list (`input_compat = ["touch_to_mouse", "gamepad_productivity"]`).
   *
   * Absent, the default comes from the entry's kind — see
   * [`lunchbox_api::EntryKind::default_input_compat`], which gives an
   * `ebook` the gamepad preset that turns its D-pad into arrow keys. A
   * list given here replaces that wholesale, and `input_compat = []` is
   * how an entry asks for no sidecar at all.
   */
  input_compat?: RawInputCompat[] | null;
  /**
   * Tunables for input-compat sidecars (analog deadzones, speeds).
   */
  input_compat_options?: RawInputCompatOptions | null;
  /**
   * Internet requirement for this entry
   */
  internet?: RawEntryInternet | null;
  /**
   * Entry kind and launch details
   */
  kind: RawEntryKind;
  /**
   * Display label
   */
  label: string;
  /**
   * Time limits
   */
  limits?: RawLimits | null;
  /**
   * Physical input devices this entry depends on (issue #96). The entry is
   * only shown / launchable while every listed device type is connected;
   * e.g. `requires_input = "keyboard"` hides a typing tutor until a keyboard
   * is attached. Accepts a single string (`requires_input = "keyboard"`) or
   * a list (`requires_input = ["keyboard", "mouse"]`). Empty / absent means
   * no input requirement.
   */
  requires_input?: RawInputDevice[];
  /**
   * Token gate (issue #8): time banked by other activities unlocks this one
   */
  tokens?: RawTokens | null;
  /**
   * Volume restrictions for this entry (overrides global)
   */
  volume?: RawVolumeConfig | null;
  /**
   * Warning configuration
   */
  warnings?: RawWarningThreshold[] | null;
  /**
   * Drop the compositor output scale to 1.0 for the duration of this
   * activity so XWayland clients render at the panel's native resolution.
   * Sway doesn't pass scale through to XWayland (issue #45), so without
   * this an XWayland game at `output * scale 1.5` only fills 1280x720 of
   * a 1920x1080 panel. lunchboxd compensates by telling the HUD to apply
   * a counter-scale factor so it stays a normal size while the activity
   * runs.
   */
  xwayland_native_resolution?: boolean;
}

/**
 * Per-entry internet requirement
 */
export interface RawEntryInternet {
  /**
   * Override connectivity check target for this entry
   */
  check?: string | null;
  /**
   * Whether lunchboxd tells the activity itself about the connectivity
   * check, in addition to using it for availability. Today only the
   * `media` kind consumes it: browse mode polls the target and hides
   * library items that have no local source while it fails, instead of
   * leaving the child a grid of tiles that error on tap.
   *
   * On by default whenever a check resolves (this entry's, else
   * `service.internet.check`). Set `false` to launch the activity without
   * one — the grid then shows every item regardless of connectivity.
   */
  forward_check?: boolean;
  /**
   * Whether this entry requires internet connectivity
   */
  required?: boolean;
}

/**
 * Raw entry kind
 */
export type RawEntryKind =
  | {
      type: "process";
      /**
       * Additional command-line arguments
       */
      args?: string[];
      /**
       * Command to run (required)
       */
      command: string;
      cwd?: string | null;
      env?: Record<string, string>;
    }
  /**
   * Snap application - uses systemd scope-based process management
   */
  | {
      type: "snap";
      /**
       * Additional command-line arguments
       */
      args?: string[];
      /**
       * Command to run (defaults to snap_name if not specified)
       */
      command?: string | null;
      /**
       * Additional environment variables
       */
      env?: Record<string, string>;
      /**
       * The snap name (e.g., "mc-installer")
       */
      snap_name: string;
    }
  /**
   * Steam game launched via the Steam snap (Linux)
   */
  | {
      type: "steam";
      /**
       * Steam App ID (e.g., 504230 for Celeste)
       */
      app_id: number;
      /**
       * Additional command-line arguments passed to Steam
       */
      args?: string[];
      /**
       * Additional environment variables
       */
      env?: Record<string, string>;
    }
  /**
   * Flatpak application - uses systemd scope-based process management
   */
  | {
      type: "flatpak";
      /**
       * The Flatpak application ID (e.g., "org.prismlauncher.PrismLauncher")
       */
      app_id: string;
      /**
       * Additional command-line arguments
       */
      args?: string[];
      /**
       * Additional environment variables
       */
      env?: Record<string, string>;
    }
  | {
      type: "vm";
      args?: Record<string, unknown>;
      driver: string;
    }
  /**
   * A `lunchbox-media` library activity (issue #127).
   *
   * The fields mirror the flags `lunchbox-media` accepts, so lunchboxd
   * builds the invocation itself. The connectivity check is not among them:
   * it is inherited from `[entries.internet]` / `[service.internet]` — see
   * [`RawEntryInternet::forward_check`].
   */
  | {
      type: "media";
      /**
       * The item id to play. Required by, and only valid with,
       * `mode = "play"`.
       */
      item?: string | null;
      /**
       * Path to a library `.toml`, `.m3u`, or `.m3u8`, or a YouTube
       * playlist URL. `~` is expanded at launch for paths.
       */
      library: string;
      /**
       * `"browse"` (default) opens the poster grid; `"play"` plays a
       * single `item` end to end.
       */
      mode?: RawMediaMode;
      /**
       * Let lunchboxd download this library's remote items in the
       * background (issue #127). Defaults to `service.media.prefetch`; set
       * `false` to exclude just this library — e.g. a live stream, or a
       * playlist too large to be worth the disk.
       */
      prefetch?: boolean | null;
      /**
       * Maximum video quality: `best`, `1080p` (default), `720p`, `480p`.
       */
      quality?: RawMediaQuality;
      /**
       * Remember playback positions for this library, so a re-opened item
       * picks up where it stopped. Off by default: with it off nothing
       * about what was watched is written to disk.
       */
      resume?: boolean;
      /**
       * Reverse the final item order. Combines with `sort_by`.
       */
      reverse?: boolean;
      /**
       * Item ordering: `library` (default), `title`, `id`, `kind`,
       * `category`, `duration`.
       */
      sort_by?: RawMediaSortBy;
      /**
       * Skip SponsorBlock segments in this library (issue #159). `None`
       * inherits `service.media.sponsorblock.enabled`; `false` turns it off
       * for this library alone, which is the shape the need actually takes —
       * a channel whose "sponsor" spans are part of the show.
       *
       * Which categories to skip stays a household decision, on the service
       * table; this is only whether to skip at all.
       */
      sponsorblock?: boolean | null;
    }
  /**
   * A single piece of content played through RetroArch. See
   * [`lunchbox_api::EntryKind::Retroarch`] for what Lunchbox sets up around
   * the launch.
   */
  | {
      type: "retroarch";
      /**
       * Extra arguments, appended after the ones Lunchbox derives.
       */
      args?: string[];
      /**
       * The RetroArch binary; defaults to `retroarch` on `PATH`.
       */
      command?: string;
      /**
       * The content (ROM / disc image) to load. Must be absolute or start
       * with `~/`.
       */
      content: string;
      /**
       * Core short name (`"mgba"`); resolved to `mgba_libretro.so`.
       * Exactly one of `core` / `core_path` is required.
       */
      core?: string | null;
      /**
       * Absolute path to the core, bypassing name resolution.
       */
      core_path?: string | null;
      /**
       * Additional environment variables
       */
      env?: Record<string, string>;
      /**
       * Lock RetroArch's own menu. On by default.
       */
      kiosk?: boolean;
      /**
       * Offer the HUD's reset ("reboot the console") button. On by default.
       */
      reset?: boolean;
      /**
       * `"auto"` (default) saves state on close and restores it on open;
       * `"off"` boots the content fresh every time.
       */
      save_state?: RetroarchSaveState;
    }
  /**
   * A single book, opened in a reader locked down to reading it. See
   * [`lunchbox_api::EntryKind::Ebook`] for what Lunchbox sets up around the
   * launch.
   */
  | {
      type: "ebook";
      /**
       * Extra arguments, appended after the ones Lunchbox derives.
       */
      args?: string[];
      /**
       * The book to open. Must be absolute or start with `~/`.
       */
      book: string;
      /**
       * The reader binary; defaults to the viewer's own name.
       */
      command?: string | null;
      /**
       * Additional environment variables
       */
      env?: Record<string, string>;
      /**
       * Font family for the same. Default "Noto Serif".
       */
      font_family?: string;
      /**
       * Point size of an EPUB's reflowed text. Default 16. Changing it
       * repaginates, which moves a remembered position.
       */
      font_size?: number;
      /**
       * Lock the reader's own escape hatches. On by default.
       */
      kiosk?: boolean;
      /**
       * `facing_first_centered` (default), `facing`, `single`, or
       * `scroll`.
       */
      layout?: EbookLayout;
      /**
       * Page to open on the first launch, 1-based. Ignored once the reader
       * remembers a position for this book.
       */
      open_at?: number | null;
      /**
       * Which reader to drive. `okular` (default) is the only one wired up.
       */
      viewer?: EbookViewer;
    }
  | {
      type: "custom";
      payload?: unknown;
      type_name: string;
    };

/**
 * Remote file management over the web interface (issue #195).
 *
 * A hardened kiosk account denies SSH and keeps its home at mode 0700, so
 * `scp` and every SFTP file manager are shut out of exactly the directory a
 * parent needs to put a book, a ROM or a video into. lunchboxd already runs
 * as that user, so the web interface is the one door that is already open.
 *
 * There is no runtime toggle. The surface is reachable by a signed-in
 * administrator whenever `enabled` is true, the same way the config editor
 * is — a caller who can reach it can already write a policy containing
 * `kind = { type = "process", command = ... }`, so withholding a file write
 * from that same credential protects nothing.
 */
export interface RawFileManagerConfig {
  /**
   * Whether the file routes exist at all. `false` removes them from the
   * router rather than answering 403, so a household that does not want
   * the surface does not have one.
   */
  enabled?: boolean;
  /**
   * Offer removable drives mounted under `/media` and `/run/media`.
   */
  external_media?: boolean;
  /**
   * Extra directories to offer, beyond the home directory and removable
   * drives — a NAS mount, or a library kept on a second disk.
   */
  extra_roots?: RawFileManagerRoot[];
  /**
   * Refuse an upload that would leave the device's own disk with less than
   * this much free space. 0 disables the check.
   *
   * Separate from `service.media.free_space_floor_bytes`, which bounds a
   * background prefetch: this one bounds a person, and a kiosk whose disk
   * is full is a session that will not start.
   *
   * **Only the device's own disk.** A removable drive filling up costs
   * nobody an evening, and a floor applied to one would make every drive
   * smaller than the floor — which is most USB sticks — unwritable.
   */
  free_space_floor_bytes?: number;
  /**
   * Largest single upload, in bytes. 0 removes the cap.
   */
  max_upload_bytes?: number;
}

/**
 * One extra place the file manager may browse (issue #195).
 */
export interface RawFileManagerRoot {
  /**
   * What the web interface calls it. Must be unique.
   */
  label: string;
  /**
   * Absolute path. Validation refuses `/` and the system directories, so a
   * typo here cannot turn the file manager into a root browser.
   */
  path: string;
}

/**
 * Per-entry firewall configuration
 *
 * Enforced via systemd `IPAddressAllow=`/`IPAddressDeny=` properties on the
 * per-session scope. Hostname matching is **not** performed at the kernel
 * layer; pair with a browser-side allowlist (e.g. Chrome `URLAllowlist`) when
 * hostname resolution is needed.
 */
export interface RawFirewallConfig {
  /**
   * Allowlisted destinations (CIDR or systemd address tokens like "any",
   * "localhost", "link-local", "multicast")
   */
  allow?: string[];
  /**
   * Default policy when no `allow` or `deny` rule matches.
   * "deny" (default) blocks all traffic except `allow` entries.
   * "allow" permits all traffic except `deny` entries.
   */
  default?: string;
  /**
   * Denylisted destinations (applied after `allow`)
   */
  deny?: string[];
}

/**
 * A group of entries that share an availability schedule and a set of limits
 * (issue #5).
 *
 * The daily quota is the *combined* usage of every member, so once the
 * category's budget is spent all of its activities disappear at once.
 */
export interface RawGroup {
  /**
   * Availability windows shared by every member
   */
  availability?: RawAvailability | null;
  /**
   * Unique stable ID, referenced by `group = "..."` on entries
   */
  id: string;
  /**
   * Display label, used when explaining why a member is unavailable
   */
  label: string;
  /**
   * Limits shared by every member. `daily_quota_seconds` is the combined
   * total across members; `max_run_seconds` and `cooldown_seconds` apply to
   * each member's session.
   */
  limits?: RawLimits | null;
  /**
   * Token gate on the whole group: earning unlocks every member at once
   */
  tokens?: RawTokens | null;
}

/**
 * Global HUD settings.
 */
export interface RawHudConfig {
  /**
   * Which screen edge the HUD occupies, for every activity that does not
   * override it. Defaults to `top`.
   */
  orientation?: RawHudOrientation | null;
}

/**
 * Screen edge for the HUD (issue #171).
 */
export type RawHudOrientation =
  /**
   * A horizontal bar along the top edge (the default).
   */
  | "top"
  /**
   * A horizontal bar along the bottom edge.
   */
  | "bottom"
  /**
   * A vertical bar down the left edge: the HUD rotated a quarter turn, for
   * hardware or activities where a side strip costs less of the screen than
   * a top bar.
   */
  | "left";

/**
 * Input compatibility mode
 */
export type RawInputCompat =
  /**
   * Translate touchscreen input into mouse events via a sidecar that
   * grabs touch devices and uses the Wayland virtual-pointer protocol.
   */
  | "touch_to_mouse"
  /**
   * Translate absolute pointer / tablet input into touch events via a
   * sidecar that grabs the device and emits a virtual touchscreen. The
   * inverse of `TouchToMouse`; the two must not be combined.
   */
  | "tablet_to_touch"
  /**
   * Grab every touchscreen and discard its events, disabling the
   * touchscreen for the duration of the activity. Mutually exclusive with
   * `TouchToMouse` and `TabletToTouch`.
   */
  | "disable_touch"
  /**
   * Productivity preset: triggers = LMB, shoulders = RMB, left stick =
   * mouse, right stick = scroll, stick-click toggles which stick drives
   * the mouse, D-pad = arrow keys, A = Enter, Start = Escape.
   */
  | "gamepad_productivity"
  /**
   * GPD/FPS preset: LT = LMB, RT = RMB, LB = MMB, left stick = WASD,
   * right stick = mouse, D-pad = scroll, A = Space, X = R, B = E, Y = F.
   */
  | "gamepad_gpd";

/**
 * Per-entry tunables forwarded to input-compat sidecars.
 */
export interface RawInputCompatOptions {
  /**
   * Stick deadzone as a fraction of full deflection (0..1).
   */
  gamepad_deadzone?: number | null;
  /**
   * Mouse speed in pixels per second at full stick deflection.
   */
  gamepad_mouse_speed?: number | null;
  /**
   * Scroll speed in discrete wheel units per second at full deflection.
   */
  gamepad_scroll_speed?: number | null;
}

/**
 * A category of physical input device an activity can depend on (issue #96).
 *
 * Unlike [`RawInputCompat`], which is a *spawn-time behaviour* (it launches an
 * input-translation sidecar), this is a *gating* condition: the activity is
 * only shown / launchable when every listed device type is connected. The
 * enum is closed, so `camera`, `microphone`, and `midi` (future work) fail to
 * parse rather than being silently accepted.
 */
export type RawInputDevice =
  /**
   * A relative pointing device (mouse, trackball, trackpad).
   */
  | "mouse"
  /**
   * A finger touchscreen.
   */
  | "touch"
  /**
   * A physical alphabetic keyboard.
   */
  | "keyboard"
  /**
   * A gamepad / game controller / joystick.
   */
  | "gamepad";

/**
 * Internet connectivity check configuration
 */
export interface RawInternetConfig {
  /**
   * Connectivity check target (e.g., "https://example.com" or "tcp://1.1.1.1:53")
   */
  check?: string | null;
  /**
   * Interval between checks (seconds)
   */
  interval_seconds?: number | null;
  /**
   * Timeout per check (milliseconds)
   */
  timeout_ms?: number | null;
}

/**
 * Time limits
 */
export interface RawLimits {
  /**
   * Minimum session length before this subject's cooldown is started, in
   * seconds. Overrides `service.cooldown_min_session_seconds` (default 120).
   * 0 means the cooldown always starts, however short the session was.
   */
  cooldown_min_session_seconds?: number | null;
  /**
   * Cooldown after session ends, in seconds
   */
  cooldown_seconds?: number | null;
  /**
   * Daily quota in seconds
   */
  daily_quota_seconds?: number | null;
  /**
   * Maximum run duration in seconds
   */
  max_run_seconds?: number | null;
  /**
   * How long this activity gets to save its progress when a resume from
   * sleep lands outside its allowed hours, in seconds (issue #155).
   * Cascades: an entry's own value, else its group's, else
   * `service.save_grace_seconds` (default 120). 0 ends the session as soon
   * as the machine wakes.
   */
  save_grace_seconds?: number | null;
}

/**
 * Management HTTP API configuration
 */
export interface RawManagementApiConfig {
  /**
   * Login and session behaviour. Absent means the defaults below.
   */
  auth?: RawWebAuthConfig | null;
  /**
   * Machine credential for `Authorization: Bearer`, for scripts, the e2e
   * harness and `curl`.
   *
   * Since issue #156 this is deliberately *not* a way for a person to log
   * in: it authenticates a request but cannot open a browser session, so it
   * grants nothing that outlives the call. A parent signs in with a
   * password or an approval on the paired companion instead. Absent, and
   * with no password set, the API answers nothing but the setup endpoints.
   */
  auth_token?: string | null;
  /**
   * IP address to bind to (default: "127.0.0.1")
   */
  bind?: string | null;
  /**
   * How long (in seconds) to keep retrying the initial bind when the requested address is
   * unavailable (e.g. an interface like ZeroTier that has not yet come up). 0 means retry
   * indefinitely. Default: 300.
   */
  bind_retry_seconds?: number | null;
  /**
   * Whether the management API is enabled (default: false)
   */
  enabled?: boolean;
  /**
   * TCP port to listen on (default: 7890)
   */
  port?: number | null;
  /**
   * Transport security (issue #156). Absent means `mode = "auto"`.
   */
  tls?: RawTlsConfig | null;
}

/**
 * How a `media` entry opens.
 */
export type RawMediaMode =
  /**
   * Open the poster grid over the whole library.
   */
  | "browse"
  /**
   * Play a single item end to end.
   */
  | "play";

/**
 * Maximum video quality for a `media` entry.
 */
export type RawMediaQuality =
  | "best"
  | "1080p"
  | "720p"
  | "480p";

/**
 * Service-wide media behaviour (issue #127).
 */
export interface RawMediaServiceConfig {
  /**
   * Maximum total size of the on-disk video cache, in bytes.
   *
   * Bounds the cache, not the volume it sits on — see
   * `free_space_floor_bytes` for that. lunchboxd hands this to every media
   * activity it launches, so the daemon prefetching into the cache and the
   * player trimming it agree on how big it may be.
   */
  cache_max_bytes?: number;
  /**
   * Stop prefetching when the cache filesystem has less than this much free
   * space, in bytes, and warn. Guards the small disks these devices use,
   * where the cache cap alone can still fill the volume. 0 disables the
   * check.
   */
  free_space_floor_bytes?: number;
  /**
   * Download remote library items in the background, so a video the child
   * opens later plays from disk instead of buffering. On by default when any
   * `media` entry is configured.
   *
   * This is the global off switch; individual entries can opt out on their
   * own with `prefetch = false` under `[entries.kind]`.
   */
  prefetch?: boolean;
  /**
   * Keep prefetching while an activity is running. Off by default: a
   * download competing with a game — or with the video the child is
   * watching right now — costs them CPU and bandwidth for content nobody
   * has asked for yet.
   */
  prefetch_while_session_active?: boolean;
  /**
   * Skipping sponsored and self-promotional spans in YouTube videos, using
   * the SponsorBlock database (issue #159). Off unless a parent turns it on.
   */
  sponsorblock?: RawSponsorBlockConfig;
  /**
   * How long, in days, watching a video protects its cached copy from being
   * displaced by a speculative download.
   *
   * This is the whole eviction policy in one number. Inside the window a
   * guess can never cost the child something they chose; past it, the file
   * competes on age like anything else, so a film watched once last spring
   * eventually yields to a video added to the library this week. Raise it
   * for a household that goes offline for long stretches and wants what it
   * has watched to stay put; 0 drops the protection entirely and orders
   * purely by age.
   */
  watched_grace_days?: number;
}

/**
 * Item ordering for a `media` entry.
 */
export type RawMediaSortBy =
  | "library"
  | "title"
  | "id"
  | "kind"
  | "category"
  | "duration";

/**
 * Service-level settings
 */
export interface RawServiceConfig {
  /**
   * Bluetooth LE management transport. Designed as the primary admin
   * path (works without IP autodiscovery or static IP). See
   * `docs/ai/history/2026-06-20 002 ble-management.md`.
   */
  ble_management?: RawBleManagementConfig | null;
  /**
   * Global screen-brightness restrictions
   */
  brightness?: RawBrightnessConfig | null;
  /**
   * Capture stdout/stderr from child applications to log files
   * Files are written to child_log_dir (or log_dir/sessions if not set)
   */
  capture_child_output?: boolean;
  /**
   * Directory for child application logs (default: log_dir/sessions)
   */
  child_log_dir?: string | null;
  /**
   * Minimum session length before a cooldown is started, in seconds
   * (default 120). A session shorter than this leaves the cooldown alone,
   * so an activity that crashes seconds after launch doesn't lock the child
   * out. Set to 0 to always start the cooldown; overridable per entry and
   * per group via `limits.cooldown_min_session_seconds`.
   */
  cooldown_min_session_seconds?: number | null;
  /**
   * Data directory for store (default: $XDG_DATA_HOME/lunchboxd)
   */
  data_dir?: string | null;
  /**
   * Default max run duration
   */
  default_max_run_seconds?: number | null;
  /**
   * Default warning thresholds (can be overridden per entry)
   */
  default_warnings?: RawWarningThreshold[] | null;
  /**
   * External monitor / docking behaviour (issue #87).
   */
  display?: RawDisplayConfig | null;
  /**
   * Remote file management over the web interface (issue #195).
   */
  file_manager?: RawFileManagerConfig | null;
  /**
   * HUD placement (issue #171).
   */
  hud?: RawHudConfig | null;
  /**
   * Internet connectivity check settings
   */
  internet?: RawInternetConfig | null;
  /**
   * Log directory (default: $XDG_STATE_HOME/lunchboxd)
   */
  log_dir?: string | null;
  /**
   * Management HTTP API settings
   */
  management_api?: RawManagementApiConfig | null;
  /**
   * Background media prefetch (issue #127).
   */
  media?: RawMediaServiceConfig | null;
  /**
   * How long a running activity gets to save its progress when a resume
   * from sleep lands outside its allowed hours, in seconds (default 120).
   * The session is clamped to this much time and a warning is shown, rather
   * than being cut the instant the machine wakes (issue #155). Set to 0 to
   * end it immediately; overridable per group and per entry via
   * `limits.save_grace_seconds`.
   */
  save_grace_seconds?: number | null;
  /**
   * IPC socket path (default: $XDG_RUNTIME_DIR/lunchboxd/lunchboxd.sock)
   */
  socket_path?: string | null;
  /**
   * Steam-specific behaviour
   */
  steam?: RawSteamConfig | null;
  /**
   * Global volume restrictions
   */
  volume?: RawVolumeConfig | null;
}

/**
 * Every category the service defines that describes a *span* a player can jump
 * over. Mirrors `lunchbox_media_core::sponsorblock::Category`, which this crate
 * cannot depend on (it compiles to wasm for the config editor); lunchboxd holds
 * the test that the two lists agree.
 *
 * An enum rather than a free string so the config editor gets a generated union
 * type to build its picker from — a category added here and not there is then a
 * build error rather than a control quietly missing an option. It also means a
 * typo is refused when the file is parsed, naming the alternatives.
 *
 * The service's two marker categories, `poi_highlight` and `chapter`, are
 * absent: they label a point rather than describe content to remove.
 */
export type RawSponsorBlockCategory =
  | "sponsor"
  | "selfpromo"
  | "interaction"
  | "intro"
  | "outro"
  | "preview"
  | "filler"
  | "music_offtopic"
  | "hook";

/**
 * SponsorBlock segment skipping (issue #159).
 *
 * Off by default, and deliberately so: it is the one media feature that talks
 * to a third-party service, and in a product that promises no telemetry
 * nothing should reach a new host because a default said so. A parent turns it
 * on; the device is otherwise silent.
 */
export interface RawSponsorBlockConfig {
  /**
   * Base URL of the SponsorBlock instance to query. Point it at a mirror to
   * avoid the public one.
   */
  api?: string;
  /**
   * Which categories to skip. The default is the five spans that are
   * reliably not the video.
   *
   * `preview` (a recap of an earlier episode), `filler` and `music_offtopic`
   * are left out of the default on purpose: their submissions are judgement
   * calls that can cut content somebody wanted.
   */
  categories?: RawSponsorBlockCategory[];
  /**
   * Skip SponsorBlock segments during playback.
   *
   * While this is false nothing is looked up and no request is made — not at
   * launch, not on a play, not in the background.
   */
  enabled?: boolean;
}

/**
 * Steam-specific service configuration
 */
export interface RawSteamConfig {
  /**
   * Allow "risky" interstitial kinds (those whose dismissal launches a game
   * that can't actually be used without missing hardware, e.g.
   * "controller_required") to appear in `auto_dismiss_interstitials`. Without
   * this, listing a risky kind is a configuration error.
   */
  allow_risky_dismiss?: boolean;
  /**
   * Known Steam launch interstitials (blocking modals between launch and the
   * game starting) to auto-dismiss by clicking their affirmative button, so
   * they don't hang the kiosk on a modal it can't show. Each value is an
   * interstitial slug (e.g. "cloud_sync", "controller_recommended"). Only
   * listed kinds are dismissed; an empty list disables the feature entirely
   * (and the CEF remote-debugging port is never opened). When unset, a safe
   * default set of verified, benign kinds is used.
   */
  auto_dismiss_interstitials?: string[] | null;
  /**
   * How long to wait for a Steam game window/process to appear after launch
   * before giving up and ending the session with an error (seconds).
   */
  launch_timeout_seconds?: number | null;
}

/**
 * Time window
 */
export interface RawTimeWindow {
  /**
   * Days of week: "weekdays", "weekends", "all", or list like ["mon", "tue", "wed"]
   */
  days: RawDays;
  /**
   * End time (HH:MM format)
   */
  end: string;
  /**
   * Start time (HH:MM format)
   */
  start: string;
}

/**
 * TLS for the management API (issue #156).
 *
 * A bearer token or a password crossing a LAN in cleartext is a credential
 * the child on that LAN can have for the asking, so the plaintext listener is
 * no longer a thing a device can be left in by accident: `mode = "off"` with a
 * bind other than loopback is a configuration error, not a warning.
 */
export interface RawTlsConfig {
  /**
   * PEM certificate chain, for `mode = "files"`.
   */
  cert?: string | null;
  /**
   * PEM private key, for `mode = "files"`. PKCS#8, SEC1 or PKCS#1.
   */
  key?: string | null;
  /**
   * One of:
   *
   * - `"auto"` (default) — plaintext on a loopback bind, a generated
   *   self-signed certificate on any other. The listener is never
   *   accidentally in the clear on a network, and a device with no domain,
   *   no CA and no Tailscale still comes up serving HTTPS.
   * - `"off"` — plaintext. Loopback binds only.
   * - `"self_signed"` — generate and persist a certificate for this
   *   device's names and addresses. Browsers show an interstitial the first
   *   time; the fingerprint is logged and readable from the companion, so
   *   the click-through can be checked rather than guessed at.
   * - `"files"` — use `cert` and `key`. This is the one that gets a real
   *   padlock: `tailscale cert`, a Let's Encrypt certificate from a DNS-01
   *   client, or a home CA all land here.
   */
  mode?: string | null;
}

/**
 * Token gate (issue #8)
 *
 * Configured on the *target* entry: time spent on the entries listed in
 * `from` banks a balance that this entry spends down as it runs.
 */
export interface RawTokens {
  /**
   * Whether the balance survives local midnight. Default false, matching
   * how the daily quota resets.
   */
  carry_over?: boolean;
  /**
   * Seconds earned per second spent on a source entry. Default 1.0.
   */
  earn_ratio?: number | null;
  /**
   * Subjects whose sessions bank time toward this one: an entry ID, or a
   * group ID prefixed with `group:` to count every member of a category.
   */
  from?: string[];
  /**
   * Ceiling on the banked balance. 0 (the default) means unlimited.
   */
  max_balance_seconds?: number | null;
  /**
   * Balance required before this entry unlocks at all. Default 0, meaning
   * any balance above zero unlocks it.
   */
  minimum_seconds?: number | null;
}

/**
 * Volume control configuration
 */
export interface RawVolumeConfig {
  /**
   * Whether volume changes are allowed at all (default: true)
   */
  allow_change?: boolean;
  /**
   * Whether mute toggle is allowed (default: true)
   */
  allow_mute?: boolean;
  /**
   * Maximum volume percentage allowed (0-100)
   */
  max_volume?: number | null;
  /**
   * Minimum volume percentage allowed (0-100)
   */
  min_volume?: number | null;
}

/**
 * Warning threshold
 */
export interface RawWarningThreshold {
  /**
   * Message template
   */
  message?: string | null;
  /**
   * Seconds before expiry
   */
  seconds_before: number;
  /**
   * Severity: "info", "warn", "critical"
   */
  severity?: string;
}

/**
 * Login and session behaviour for the web UI (issue #156).
 */
export interface RawWebAuthConfig {
  /**
   * Consecutive failed logins from one address before that address is
   * locked out. Default: 8.
   */
  lockout_after?: number | null;
  /**
   * How long the first lockout lasts, in seconds; each subsequent lockout
   * for the same address doubles it, to a ceiling of 16x. Default: 300.
   */
  lockout_seconds?: number | null;
  /**
   * A session unused for this long is dead. Default: 2.
   */
  session_idle_days?: number | null;
  /**
   * A session older than this is dead however actively it is used.
   * Default: 14.
   */
  session_max_days?: number | null;
}

/**
 * How a [`EntryKind::Retroarch`] activity treats its save state across
 * close and re-open.
 *
 * This is the emulator's *snapshot*, not the game's own save file. The
 * in-game save (SRAM / battery save) is flushed on a clean exit either way,
 * and periodically while playing.
 */
export type RetroarchSaveState =
  /**
   * Write a save state when the activity closes and load it on the next
   * open, so the child resumes exactly where they stopped — mid-battle,
   * mid-cutscene, wherever the session ended.
   *
   * Note this makes the console's own power-on screen unreachable, which is
   * what the HUD's reset button is for.
   */
  | "auto"
  /**
   * Leave save states alone. Every launch boots the content from scratch;
   * only the in-game save carries over.
   */
  | "off";
