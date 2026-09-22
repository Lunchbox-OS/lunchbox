// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from the Rust wire types by
// `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`.
// Edit `crates/lunchbox-api/src/types.rs` and re-run instead.
//
// Property names are the wire form (snake_case), because that is what the
// daemon sends and nothing renames them in transit.

/** An RFC 3339 timestamp. A `string`; the alias records the intent. */
export type IsoTimestamp = string;

/** A calendar date, `YYYY-MM-DD`. */
export type IsoDate = string;

/**
 * Which family an address belongs to. Kept explicit rather than sniffed from
 * the string, so a UI grouping v4 above v6 does not have to count colons.
 */
export type AddressFamily =
  | "v4"
  | "v6";

export interface AdminRecord {
  /**
   * `"public"` or `"random"` — matches `bluer`'s `AddressType` enum
   * so the server can compare on reconnect without a parse step.
   */
  address_type: string;
  bonded_at: IsoTimestamp;
  device_name: string;
  /**
   * Bearer token also accepted by the HTTP API. See the unified-identity
   * section of the BLE management design.
   */
  http_token: string;
  /**
   * Stable public handle for this admin, minted once and never reused.
   *
   * Not a credential: it is what `revoke_admin` names and what
   * [`AdminSummary`] hands to a phone listing the others, so it has to be
   * safe to show. The identity address would have served, but an admin is
   * revoked and re-enrolled at the same address often enough — a phone
   * reset, a re-pair — that naming rows by address makes a stale tap in a
   * list act on a record the parent was not looking at.
   *
   * Defaulted rather than required so a v1 record, written before this
   * field existed, still parses and gets one on load.
   */
  id?: string;
  /**
   * The BlueZ-resolved identity address for the bonded peer. Once
   * pairing completes BlueZ presents this address regardless of the
   * peer's random MAC rotation, so it doubles as the stable identity.
   */
  identity_address: string;
  role: AdminRole;
}

export type AdminRole =
  | "admin";

/**
 * One administrator, as anyone with standing to see the list sees them.
 *
 * Carries no credential. The record behind this holds a minted HTTP bearer
 * token, which is the whole reason the summary exists: listing administrators
 * hands the list to a phone or a browser, and a client that can read every
 * other client's token does not need to be revoked to keep using the device
 * after it has been.
 *
 * Here rather than in `lunchbox-ble` because both transports return it and
 * `lunchbox-ble` sits *above* this crate — the same reason
 * [`crate::webauth::WebSessionInfo`] lives here.
 */
export interface AdminSummary {
  bonded_at: IsoTimestamp;
  device_name: string;
  /**
   * Stable public handle — what `revoke_admin` takes. Not a credential.
   */
  id: string;
  /**
   * Shown so a parent can tell two phones with the same name apart, and so
   * a client can recognise its own row without the device having to guess
   * which caller it is talking to.
   */
  identity_address: string;
  role: string;
}

/**
 * The audio output a volume reading applies to.
 *
 * `key` is `<device.name>:output:<route.name>` — the same key WirePlumber uses
 * to persist per-route volume, so our notion of "an output" cannot drift from
 * the volume PipeWire remembers for it. It is stable across reboots and, for
 * USB devices, across being moved to a different port.
 */
export interface AudioOutput {
  /**
   * Human-readable label for display. Localized and mutable.
   */
  description: string;
  /**
   * Stable identity. Use this to correlate, never the description.
   */
  key: string;
  kind: AudioOutputKind;
}

/**
 * What kind of thing an audio output is.
 *
 * Advisory only: it drives presentation (an icon, a label) and never policy.
 * It cannot be determined for every device — a generic USB interface reports a
 * nondescript `analog-output` route and no udev form-factor — so `Unknown` is a
 * routine outcome, not a failure.
 */
export type AudioOutputKind =
  | "speakers"
  | "headphones"
  | "hdmi"
  | "digital"
  | "line_out"
  | "bluetooth"
  | "unknown";

/**
 * An audio output the device has seen, together with any per-output volume
 * limit the parent has set for it.
 *
 * These rows are how per-output limits are configured: lunchboxd records every
 * output it observes, the management UIs list them, and the parent sets a cap
 * on the row they recognise. Nothing has to be predicted or hand-written —
 * which matters because an output often cannot be classified at all (see
 * [`AudioOutputKind`]).
 */
export interface AudioOutputRecord {
  /**
   * Whether this is the output currently selected. Runtime state, not stored.
   */
  active: boolean;
  /**
   * Whether the device is plugged in right now, so it can be switched to.
   *
   * Rows outlive the hardware — that is the point, so a cap set on the
   * headphones survives unplugging them — which means a row can name a device
   * that is not here. Defaults to `true` so a client talking to a daemon that
   * predates this field offers the choice and lets the attempt fail loudly,
   * rather than greying out every device it could actually switch to.
   */
  available?: boolean;
  /**
   * When the device last observed this output. Lets the UI show recently used
   * devices first and lets a parent prune ones that are long gone.
   */
  last_seen: IsoTimestamp;
  /**
   * Cap for this output. `None` means no per-output cap; the global
   * `[service.volume]` limit applies instead.
   */
  max_volume?: number | null;
  /**
   * Floor for this output. `None` means no per-output floor.
   */
  min_volume?: number | null;
  /**
   * Identity, display label, and advisory kind.
   */
  output: AudioOutput;
}

/**
 * Screen brightness status information
 */
export interface BrightnessInfo {
  /**
   * Whether an ambient light sensor is present, so automatic brightness
   * can be offered at all. When false, `auto_enabled` is always false.
   */
  auto_available?: boolean;
  /**
   * Whether automatic (ambient-light) brightness is currently enabled.
   */
  auto_enabled?: boolean;
  /**
   * Whether brightness control is available on this host
   */
  available: boolean;
  /**
   * The detected brightness backend (e.g. "sysfs", "brightnessctl")
   */
  backend?: string | null;
  /**
   * Name of the backlight device being controlled, if any
   */
  device?: string | null;
  /**
   * Brightness percentage (0-100)
   */
  percent: number;
  /**
   * Current restrictions on brightness
   */
  restrictions: BrightnessRestrictions;
}

/**
 * Brightness restrictions that are currently in effect
 */
export interface BrightnessRestrictions {
  /**
   * Whether brightness changes are allowed at all
   */
  allow_change: boolean;
  /**
   * Maximum brightness percentage allowed
   */
  max_brightness?: number | null;
  /**
   * Minimum brightness percentage allowed
   */
  min_brightness?: number | null;
}

/**
 * What `claim` did.
 */
export type ClaimOutcome =
  /**
   * The caller is an admin: freshly enrolled, or already one and asking
   * again. Carries the record, token and all — this is the one moment the
   * token crosses the wire.
   */
  | {
      status: "claimed";
      admin: AdminRecord;
    }
  /**
   * The device already has admins and this phone is not one. It has to be
   * approved from a phone that is; poll `claim` again to find out.
   */
  | {
      status: "pending";
      request: EnrolmentRequestInfo;
    };

export type ClaimStateTag =
  | "unclaimed"
  | "claimed";

/**
 * How much of the internet the host believes it can reach.
 *
 * Mirrors NetworkManager's connectivity states, which are the only ones any
 * backend here can distinguish. A host with no NetworkManager reports
 * [`Connectivity::Unknown`] — which is honest, and different from
 * [`Connectivity::None`].
 */
export type Connectivity =
  /**
   * Nobody could say. Not a claim that the device is offline.
   */
  | "unknown"
  /**
   * No route to anywhere.
   */
  | "none"
  /**
   * A captive portal is intercepting traffic. Worth its own state: the
   * device looks connected and nothing works, which is the single most
   * confusing failure to debug remotely.
   */
  | "portal"
  /**
   * A route exists but the connectivity probe did not complete.
   */
  | "limited"
  /**
   * The host reached the internet.
   */
  | "full";

/**
 * A parent-set daily override for a single entry
 */
export interface DailyOverride {
  /**
   * Override the entry's availability for this day.
   * `Some(false)` blocks it entirely; `Some(true)` allows it outside its time window.
   * `None` means no availability override (quota delta may still apply).
   */
  availability?: boolean | null;
  created_at: IsoTimestamp;
  date: IsoDate;
  /**
   * Signed adjustment to today's quota in seconds.
   * Positive = extra time; negative = reduced time; `None` = no change.
   */
  quota_delta_seconds?: number | null;
  /**
   * What the override applies to: an entry, or a whole group (issue #5).
   * Serializes as a bare entry ID, or `group:<id>` for a group, so overrides
   * written before groups existed round-trip unchanged.
   */
  subject: LimitSubject;
  updated_at: IsoTimestamp;
}

/**
 * One launchable application from the system's `.desktop` files, as
 * administrator mode's app picker sees it (issue #154).
 *
 * Enumerated by `lunchbox_config::desktop`, which does the Desktop Entry
 * parsing; this is only the shape that crosses the wire. Deliberately carries
 * no `Exec`: what a client may do is ask for an id to be launched, not hand
 * the daemon a command line.
 */
export interface DesktopApp {
  /**
   * One-line description (`Comment`), localized the same way.
   */
  comment?: string | null;
  /**
   * Icon theme name or absolute path, straight from `Icon`. Resolved by
   * whichever toolkit draws it, exactly as for a configured entry.
   */
  icon?: string | null;
  /**
   * The desktop file ID — the path relative to its `applications`
   * directory with `/` replaced by `-`, e.g. `org.kde.krita.desktop`. The
   * spec's own identifier, and what `launch_desktop_app` takes.
   */
  id: string;
  /**
   * Display name, localized to the device's locale where the file offers a
   * translation.
   */
  name: string;
  /**
   * Whether the application expects a terminal emulator. Shown so a picker
   * can mark it: on a device with no terminal installed, launching one of
   * these fails, and saying so up front beats a launch that appears to do
   * nothing.
   */
  terminal: boolean;
}

/**
 * Shape returned from the `DeviceInfo` characteristic. Readable
 * unencrypted; carries only what the companion app needs to decide
 * whether to initiate pairing.
 */
export interface DeviceInfo {
  claim_state: ClaimStateTag;
  device_name: string;
  firmware_version: string;
  protocol_version: number;
}

/**
 * One condition that is currently true.
 */
export interface Diagnostic {
  code: DiagnosticCode;
  /**
   * One line, for a person. Most of these already exist verbatim as the log
   * message the diagnostic replaces.
   */
  message: string;
  /**
   * What to do about it, when there is a concrete answer — a command to run,
   * a group to join. `None` when the fix is not something we can name.
   */
  remedy?: string | null;
  severity: DiagnosticSeverity;
  /**
   * When this condition was first observed. Preserved across a re-raise, so
   * "since" means since it started, not since it was last checked.
   */
  since: IsoTimestamp;
  subject: DiagnosticSubject;
}

/**
 * What is wrong. An enum rather than a string so the UIs can special-case
 * presentation and the wire drift test covers the variant set.
 */
export type DiagnosticCode =
  /**
   * Per-entry firewall enforcement is unavailable on this host — the helper
   * is not installed, or polkit denies it.
   */
  | "firewall_unenforceable"
  /**
   * This entry configures a firewall that cannot be applied, so it will not
   * launch. Distinct from [`Self::FirewallUnenforceable`], which is the
   * host-wide cause: this one names an activity the child has lost.
   */
  | "firewall_not_applied"
  /**
   * Lunchbox cannot talk to the compositor, so it cannot see what is on
   * screen. The escape sweep closes nothing and no orphaned window is
   * reported, which is indistinguishable from a clear screen unless it is
   * said out loud (issue #147).
   */
  | "compositor_unreachable"
  /**
   * The compositor's IPC socket is still reachable by every process at this
   * uid, because hardening it failed (issue #144).
   *
   * The session is deliberately left running — an unhardened kiosk beats no
   * kiosk — so nothing else about the device looks wrong. Without this the
   * only trace is one log line, and a device ships without a protection it
   * is configured to have.
   */
  | "compositor_not_hardened"
  /**
   * Something replaced or removed lunchboxd's management socket, so the
   * daemon is no longer reachable at the path its clients use (issue #144).
   *
   * An activity can do this: the socket lives in a directory owned by the
   * uid every activity runs as, and no file mode prevents it — a root-owned
   * directory stops lunchboxd binding at all, and the sticky bit restricts
   * deletion to the file's owner, which an activity is. Clients refuse to
   * talk to whatever bound the name instead, so this is a denial rather than
   * a breach; without saying so, it looks like a launcher that stopped
   * working for no reason.
   */
  | "ipc_socket_replaced"
  /**
   * lunchboxd's own management socket is reachable by processes that are
   * not part of the session — the peer allow-list is not armed, or it is
   * armed somewhere it cannot mean anything (issue #144).
   *
   * Like [`Self::CompositorNotHardened`], the session is deliberately left
   * running, so nothing else about the device looks wrong and the downgrade
   * is invisible unless it is said out loud.
   */
  | "ipc_socket_not_hardened"
  /**
   * Lunchbox's policy and state are files at the uid activities run as,
   * because this device has no state custodian (issue #157).
   *
   * The session is deliberately left running — an unprotected kiosk beats a
   * child staring at a dead screen — so, like
   * [`Self::IpcSocketNotHardened`], nothing else about the device looks
   * wrong and the downgrade is invisible unless it is said out loud.
   *
   * Only for a device that never had one: a packaged install where
   * `lunchbox-admin setup-user` has not run, or one deliberately left
   * without. A device whose custodian *is* installed and unreachable does
   * not reach this — it refuses to start, because its state has moved and
   * running anyway would mean an empty database and a launcher with no
   * activities, which looks like a quiet evening rather than a fault.
   *
   * Raised only at startup. A device that fell back mid-session would be a
   * device an activity could *push* into falling back, which is the one
   * thing this must not be.
   */
  | "state_not_protected"
  /**
   * Nothing outside the session would notice this daemon being killed, so an
   * activity can leave the session running with nothing supervising it
   * (issue #172).
   *
   * Every activity runs as lunchboxd's own uid, and signal permission is a
   * uid comparison — so `kill`, or `SIGSTOP`, is available to anything the
   * device is supervising. The answer is the state custodian, which is
   * outside the session at a uid nothing in it can signal: it watches a
   * connection lunchboxd holds and ends the session when the feeding stops.
   *
   * This is raised when that watchdog exists but cannot act — the polkit
   * rule that lets the custodian end a session is missing, or the connection
   * could not be opened at all. Deliberately *not* raised on a device with
   * no custodian: that device already says so through
   * [`Self::StateNotProtected`], and one fact should not set off two alarms.
   *
   * `Critical`, because a watchdog that cannot fire is worse than no
   * watchdog: it is the shape that looks like protection.
   */
  | "session_not_guarded"
  /**
   * Something at this uid tried to drive the daemon from outside the
   * session and was refused (issue #144). Worth an administrator's
   * attention: an activity probing the management socket is not something
   * that happens by accident.
   */
  | "ipc_peer_rejected"
  /**
   * This entry sets a browser policy that its kind does not support, so the
   * policy is ignored.
   */
  | "browser_policy_ignored"
  /**
   * A media activity references YouTube but `yt-dlp` is not installed.
   */
  | "yt_dlp_missing"
  /**
   * Free space on the media cache volume is below the configured floor, so
   * prefetch has stopped.
   */
  | "media_cache_disk_low"
  /**
   * A media library could not be read or parsed.
   */
  | "media_library_unreadable"
  /**
   * An administrator asked for a media refresh (issue #165) and it could not
   * reach what it was told to re-fetch — the device is offline, the playlist
   * would not load, SponsorBlock did not answer.
   *
   * Distinct from [`Self::MediaLibraryUnreadable`], which is about a library
   * that cannot be *parsed* and is just as broken on a scheduled sweep. This
   * one only ever appears because somebody pressed a button, and it is what
   * stops that button from looking like it worked. The device keeps serving
   * whatever it had cached, so this is a refresh that did not happen rather
   * than a library that is gone.
   */
  | "media_refresh_failed"
  /**
   * No sound backend was detected; volume control does nothing.
   */
  | "no_sound_backend"
  /**
   * The sound backend is present but its device topology could not be read,
   * so which output is selected and which are plugged in are both unknown.
   * Distinct from [`Self::NoSoundBackend`]: there *is* a backend, and the
   * per-output volume limits are running on the last state seen rather than
   * on what is true now.
   */
  | "audio_topology_unreadable"
  /**
   * No readable input devices, so input-gated entries cannot be evaluated.
   */
  | "input_devices_unavailable"
  /**
   * The BlueZ pairing agent could not be registered; a new phone will not be
   * shown a pairing code.
   */
  | "ble_pairing_agent_unavailable"
  /**
   * A RetroArch entry names a libretro core that is not installed, so the
   * activity will not launch.
   */
  | "retroarch_core_missing"
  /**
   * A RetroArch entry's content — its ROM or disc image — is not there, so
   * the activity will not launch.
   */
  | "retroarch_content_missing"
  /**
   * An ebook entry's book is not there, so the activity opens on an error
   * instead of a page.
   */
  | "ebook_book_missing"
  /**
   * An ebook entry's reader, or the backend for that book's format, is not
   * installed. On Ubuntu the EPUB backend ships separately from Okular, so
   * this is the likely first-run failure.
   */
  | "ebook_reader_missing"
  /**
   * An ebook entry lays the book out in pages on a device that has no way
   * to turn one: a touchscreen and nothing else. Reading would stop at the
   * end of the first page.
   */
  | "ebook_no_page_turn"
  /**
   * The web management interface is configured and is not serving, so the
   * address a parent would browse to refuses the connection (issue #182).
   *
   * Until this existed the failure reached exactly one log line on a device
   * nobody can log into, which is the wrong place for it: the whole reason
   * to open the web interface is that something else has already gone
   * wrong. The companion app still works — it is on BLE, not the network —
   * so this is a path lost rather than a device lost, and it is a
   * `Warning`.
   *
   * Not raised while the daemon is still retrying a bind whose address has
   * not appeared yet: that is `bind_retry_seconds` doing its job, and a
   * ZeroTier interface coming up at login would otherwise raise an alarm
   * every boot and clear it seconds later.
   */
  | "management_api_unavailable"
  /**
   * The last run of the daemon was killed with an activity open — a held
   * power button, a crash, an OOM kill — and the session was settled from
   * its checkpoint at this startup instead (issue #201).
   *
   * Raised because the recovery is *not* exact. Usage is checkpointed on an
   * interval, so up to one interval of real play is not in what was
   * charged, and a child who discovers this has found a way to buy time back
   * a little at a time. One power cut is a power cut; the same one every
   * evening is the bypass the issue is about, and nothing else on the device
   * would say so — the launcher looks normal and the quota is merely a
   * little generous.
   *
   * `Warning`, not `Critical`: the protection held, and the time was
   * recovered. What is degraded is the accuracy of the accounting, which is
   * exactly what a supervision device is for.
   *
   * Raised only at startup, and it lives for that boot. The condition is
   * "the previous run ended badly", which stays true for as long as it is
   * the most recent thing that happened; a clean boot starts an empty
   * registry and so clears it without anything having to remember to.
   */
  | "session_interrupted";

/**
 * The current set, as clients see it.
 *
 * A struct rather than a bare `Vec` so the cap can report itself: a client
 * showing 32 of 40 problems while implying it is showing all of them would be
 * worse than showing none.
 */
export interface DiagnosticSet {
  /**
   * Sorted most severe first, then by subject, then by code — a stable
   * order, so a client diffing two snapshots sees real changes rather than
   * reordering.
   */
  items: Diagnostic[];
  /**
   * Whether [`MAX_DIAGNOSTICS`] hid anything. UIs must say so.
   */
  truncated: boolean;
}

/**
 * How bad it is. Declaration order is the sort order: `Critical` first.
 */
export type DiagnosticSeverity =
  /**
   * The configuration claims a protection the device is not providing.
   * Unmissable in both UIs.
   */
  | "critical"
  /**
   * A feature is unavailable or degraded.
   */
  | "warning"
  /**
   * Worth knowing; nothing is broken.
   */
  | "info";

/**
 * What a diagnostic is about.
 *
 * The split exists so a UI can render a per-entry problem on the entry itself,
 * next to the availability reasons already there, instead of only in a global
 * list.
 */
export type DiagnosticSubject =
  /**
   * The device as a whole.
   */
  | { type: "service" }
  /**
   * One configured activity.
   */
  | {
      type: "entry";
      entry_id: EntryId;
    };

/**
 * How the kiosk drives displays when an external monitor is docked (issue #87).
 *
 * Exactly one logical output is ever active in every variant, so the
 * one-activity-at-a-time invariant always holds.
 */
export type DisplayMode =
  /**
   * Only the internal/primary panel is active — the state when no external
   * display is connected.
   */
  | "single_internal"
  /**
   * The external display mirrors the primary. Default whenever an external
   * display connects.
   */
  | "mirror"
  /**
   * The primary panel is disabled and the external display drives the
   * session at its native resolution.
   */
  | "external_only";

/**
 * Snapshot of the compositor's display arrangement, broadcast to shells so the
 * HUD can show/hide and label its mirror/external toggle (issue #87).
 */
export interface DisplayState {
  mode: DisplayMode;
  /**
   * Connector name of the primary (internal, first-enumerated) output.
   */
  primary?: string | null;
  /**
   * Connector name of the external/secondary output, if one is connected.
   */
  secondary?: string | null;
}

export interface Duration {
  nanos: number;
  secs: number;
}

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
 * A phone waiting to be let in, as an administrator sees it.
 */
export interface EnrolmentRequestInfo {
  /**
   * Six digits the requesting phone is displaying. Whoever approves
   * compares them against that phone's screen.
   *
   * Same ritual as BLE pairing's Numeric Comparison and #156's login code,
   * and for the same reason: a racing attacker's request carries different
   * digits, so comparing is what picks the right row out of a list.
   */
  code: string;
  /**
   * What the requesting phone calls itself.
   */
  device_name: string;
  expires_at: IsoTimestamp;
  /**
   * Public handle — what `approve_enrolment_request` takes. Safe to list.
   */
  id: string;
  /**
   * Its address, so two phones with the same name are still distinguishable.
   */
  peer: string;
  requested_at: IsoTimestamp;
}

/**
 * Unique identifier for an entry in the policy whitelist
 */
export type EntryId = string;

/**
 * Entry kind with launch details
 */
export type EntryKind =
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
   * The fields mirror the flags `lunchbox-media` accepts, so lunchboxd can
   * build the invocation itself instead of an admin restating it as a
   * `Process` argv. `connectivity_check` is not among them: it is resolved
   * from the entry's `internet` policy at spawn time and reaches the host
   * adapter through `SpawnOptions`.
   */
  | {
      type: "media";
      /**
       * The item to play. Required by (and only meaningful for)
       * [`MediaMode::Play`].
       */
      item?: string | null;
      /**
       * Library source: a path to a `.toml`/`.m3u`/`.m3u8` file, or a
       * YouTube playlist URL. `~` is expanded for paths at spawn time.
       */
      library: string;
      /**
       * Whether to open the poster grid or play a single item.
       */
      mode?: MediaMode;
      /**
       * Whether lunchboxd may prefetch this library's remote items in the
       * background. `None` inherits `service.media.prefetch`.
       */
      prefetch?: boolean | null;
      /**
       * Maximum video quality for playback and background downloads.
       */
      quality?: MediaQuality;
      /**
       * Remember playback positions for this library across sessions.
       */
      resume?: boolean;
      /**
       * Reverse the final item order. Combines with `sort_by`.
       */
      reverse?: boolean;
      /**
       * Field used to order library items before display or lookup.
       */
      sort_by?: MediaSortBy;
      /**
       * Whether to skip SponsorBlock segments in this library. `None`
       * inherits `service.media.sponsorblock.enabled`.
       */
      sponsorblock?: boolean | null;
    }
  /**
   * A single piece of content played through the RetroArch libretro
   * frontend, launched directly on its CLI (`retroarch -L <core> <content>`).
   *
   * Distinct from [`EntryKind::Process`] because RetroArch needs settings
   * materialized around the launch to behave in a kiosk: save state on
   * close, restore it on open, flush the in-game save periodically, and
   * stay out of its own menu. The host adapter renders those into a config
   * fragment it passes with `--appendconfig`; the user's own `retroarch.cfg`
   * is never edited. See `lunchbox-host-linux::retroarch`.
   */
  | {
      type: "retroarch";
      /**
       * Extra arguments, appended after the ones Lunchbox derives.
       */
      args?: string[];
      /**
       * The RetroArch binary. Defaults to `retroarch` on `PATH`.
       */
      command?: string;
      /**
       * The content (ROM / disc image) to load.
       */
      content: string;
      /**
       * Core short name, e.g. `"mgba"` → `mgba_libretro.so`, resolved
       * against the usual libretro core directories. Mutually exclusive
       * with `core_path`.
       */
      core?: string | null;
      /**
       * Absolute path to a `*_libretro.so`, bypassing name resolution.
       */
      core_path?: string | null;
      env?: Record<string, string>;
      /**
       * Lock RetroArch's own menu so the activity can't be used to browse
       * the filesystem or change emulator settings. On by default: this is
       * a supervised kiosk.
       */
      kiosk?: boolean;
      /**
       * Offer a reset ("reboot the console") button on the HUD. On by
       * default, because `save_state = "auto"` otherwise makes the
       * console's own power-on screen unreachable — there is no way back to
       * the title screen from inside a resumed save state.
       */
      reset?: boolean;
      /**
       * Whether closing the activity saves state and opening restores it.
       */
      save_state?: RetroarchSaveState;
    }
  /**
   * One book, opened in a document reader locked down to reading it
   * (issue #160).
   *
   * The reader keeps the page: Lunchbox's job is to hand it a private
   * configuration that closes every door out of the book, and to close the
   * window politely at the end of the session so the position is written.
   * See [`lunchbox_host_linux::ebook`] for what is generated.
   */
  | {
      type: "ebook";
      /**
       * Extra arguments, appended after the ones Lunchbox derives.
       */
      args?: string[];
      /**
       * The book. Absolute, or `~/`-prefixed; expanded at launch.
       */
      book: string;
      /**
       * The reader binary. Defaults to the viewer's usual name.
       */
      command?: string | null;
      env?: Record<string, string>;
      /**
       * Font family for the same. Must be installed on the device.
       */
      font_family?: string;
      /**
       * Point size for the reflowed text of an EPUB. Changing it
       * repaginates the book, which moves a remembered position, so pick it
       * before the book is first opened.
       */
      font_size?: number;
      /**
       * Lock the reader down: no file dialog, no printing, no settings, no
       * menubar or toolbar. On by default — this is a supervised kiosk, and
       * off is only for an admin checking what the reader looks like
       * unrestricted.
       */
      kiosk?: boolean;
      /**
       * How pages are laid out. `facing` (the default) suits a landscape
       * panel; `single` a portrait one.
       */
      layout?: EbookLayout;
      /**
       * Page to open on the *first* launch, 1-based. Ignored once the
       * reader has a remembered position for this book.
       */
      open_at?: number | null;
      /**
       * Which reader to drive. Only `okular` is implemented.
       */
      viewer?: EbookViewer;
    }
  | {
      type: "custom";
      payload: unknown;
      type_name: string;
    };

/**
 * Entry kind tag for capability matching
 */
export type EntryKindTag =
  | "process"
  | "snap"
  | "steam"
  | "flatpak"
  | "vm"
  | "media"
  | "retroarch"
  | "ebook"
  | "custom";

/**
 * View of an entry for UI display
 */
export interface EntryView {
  /**
   * Whether time spent here banks time toward some *other* activity's gate
   * (issue #8) — i.e. this entry, or the category it belongs to, appears in
   * some `tokens.from`.
   *
   * The launcher wears it as the "earn" pill (issue #207), which is the only
   * place a child is told that this activity is worth something beyond
   * itself. Deriving it in a client is not possible: a gate names its
   * sources, so the sources themselves carry no trace of being one.
   */
  earns_tokens?: boolean;
  enabled: boolean;
  entry_id: EntryId;
  /**
   * The group this entry belongs to (issue #5), if any. Management UIs use
   * it to show that an activity's schedule and budget are shared.
   */
  group?: GroupId | null;
  icon_ref?: string | null;
  kind_tag: EntryKindTag;
  label: string;
  /**
   * Maximum run duration if started now. None means:
   * - If enabled=false: entry is not available
   * - If enabled=true: entry has no time limit (unlimited)
   */
  max_run_if_started_now?: Duration | null;
  reasons: ReasonCode[];
  /**
   * The entry's own token gate (issue #8), if it has one. A member of a
   * token-gated group carries its own gate only; the category's is on the
   * `GroupView`.
   */
  tokens?: TokenStatus | null;
}

/**
 * Event envelope
 */
export interface Event {
  api_version: number;
  payload: EventPayload;
  timestamp: IsoTimestamp;
}

/**
 * All possible events from the service to clients
 */
export type EventPayload =
  /**
   * Full state snapshot (sent on subscribe and major changes)
   */
  | ({ type: "state_changed" } & ServiceStateSnapshot)
  /**
   * Session has started
   */
  | {
      type: "session_started";
      /**
       * Whether the HUD should offer a reset button for this session.
       * Defaults to `false` when absent, so an older payload just doesn't
       * show the button.
       */
      can_reset?: boolean;
      /**
       * Whether the HUD should offer page-turn buttons for this session
       * (issue #160). Defaults to `false` when absent, like `can_reset`.
       */
      can_turn_pages?: boolean;
      /**
       * Whether the HUD should confirm before its "X" button ends this
       * session (issue #78). Defaults to `true` when absent.
       */
      confirm_on_close?: boolean;
      /**
       * Deadline for session. None means unlimited.
       */
      deadline?: IsoTimestamp | null;
      entry_id: EntryId;
      label: string;
      session_id: SessionId;
    }
  /**
   * Warning issued for current session
   */
  | {
      type: "warning_issued";
      message?: string | null;
      session_id: SessionId;
      severity: WarningSeverity;
      threshold_seconds: number;
      time_remaining: Duration;
    }
  /**
   * Session is expiring (termination initiated)
   */
  | {
      type: "session_expiring";
      session_id: SessionId;
    }
  /**
   * Session has ended
   */
  | {
      type: "session_ended";
      duration: Duration;
      entry_id: EntryId;
      reason: SessionEndReason;
      session_id: SessionId;
    }
  /**
   * Policy was reloaded
   */
  | {
      type: "policy_reloaded";
      entry_count: number;
    }
  /**
   * The device entered or left administrator mode (issue #154).
   *
   * Carries the flag rather than being two variants so a client that only
   * cares about the current value can handle one arm. The full snapshot also
   * carries `admin_mode`, so a client that resubscribes mid-mode is not left
   * guessing.
   */
  | {
      type: "admin_mode_changed";
      active: boolean;
    }
  /**
   * The screen was locked or unlocked (issue #154).
   */
  | {
      type: "lock_changed";
      locked: boolean;
    }
  /**
   * Entry availability changed (for UI updates)
   */
  | {
      type: "entry_availability_changed";
      enabled: boolean;
      entry_id: EntryId;
    }
  /**
   * Volume status changed.
   *
   * Carries the full state rather than just the reading. The active output can
   * change without anyone touching the volume (a headset is plugged in, a dock
   * switches sinks), and that changes both the effective volume *and* which
   * restrictions apply — so a subscriber that merged only `percent`/`muted`
   * into a cached snapshot would keep showing the previous output's limits.
   * `percent` and `muted` stay at the top level for wire compatibility with
   * clients built before the other fields existed.
   */
  | {
      type: "volume_changed";
      muted: boolean;
      output?: AudioOutput | null;
      percent: number;
      restrictions?: VolumeRestrictions;
    }
  /**
   * Screen brightness changed. `auto_enabled` reports whether automatic
   * (ambient-light) brightness is currently on, so subscribers can keep an
   * auto/manual indicator in sync from the same event.
   */
  | {
      type: "brightness_changed";
      auto_enabled: boolean;
      percent: number;
    }
  /**
   * HUD UI scale factor changed. The HUD is expected to multiply its
   * font/padding/height by `factor` on top of the compositor scale.
   *
   * Emitted by lunchboxd when it temporarily drops the compositor's
   * output scale to 1.0 for an XWayland activity that cannot render at
   * the panel's native resolution otherwise; on entry start the factor
   * is the captured pre-launch output scale, and on entry exit it
   * returns to 1.0. Clients that don't care can ignore it.
   */
  | {
      type: "hud_scale_changed";
      factor: number;
    }
  /**
   * The screen edge the HUD should occupy has changed (issue #171).
   *
   * Emitted when an activity whose `hud_orientation` differs from the
   * global one starts, and again when it ends and the global setting takes
   * over. Like `HudScaleChanged` this fires only on *change*, so a shell
   * that connected late or reconnected mid-session must seed itself with
   * `get_hud_orientation` rather than assume the default (the reconnect
   * hole issue #118 opened for the scale factor).
   */
  | {
      type: "hud_orientation_changed";
      orientation: HudOrientation;
    }
  /**
   * Internet connectivity check changed. `target` matches the
   * `InternetStatusView::target` field in `ServiceStateSnapshot`.
   */
  | {
      type: "internet_status_changed";
      available: boolean;
      target: string;
    }
  /**
   * The external-display arrangement changed (issue #87). Shells use this to
   * show/hide their mirror/external toggle and to re-anchor their layer-shell
   * surface to the currently active output. Emitted on boot, on hotplug, and
   * on every mode toggle.
   */
  | {
      type: "display_mode_changed";
      state: DisplayState;
    }
  /**
   * The system is about to suspend/sleep. Clients should immediately
   * commit a static "cover" frame (e.g. a loading screen) so the image
   * frozen on screen across the suspend/resume gap is not stale (old
   * clock, battery, or activity list). lunchboxd holds a logind delay
   * inhibitor for a short grace period after emitting this so clients have
   * time to draw before the screen freezes.
   */
  | { type: "system_suspending" }
  /**
   * The system has resumed from suspend/sleep. A fresh `StateChanged`
   * follows immediately so clients can replace the cover with up-to-date
   * content.
   */
  | { type: "system_resumed" }
  /**
   * The set of administrator-facing conditions changed (issue #143): one
   * was raised, cleared, or updated.
   *
   * Carries the whole set rather than a delta. The set is small and capped,
   * and a client that missed an event would otherwise need reconciliation
   * logic to work out what it now holds — for a payload this size that is
   * cost with no benefit.
   */
  | ({ type: "diagnostics_changed" } & DiagnosticSet)
  /**
   * Service is shutting down
   */
  | { type: "shutdown" }
  /**
   * Audit event (for admin clients)
   */
  | {
      type: "audit_entry";
      details: unknown;
      event_type: string;
    };

/**
 * Unique identifier for a group of entries sharing a schedule and limits
 * (issue #5)
 */
export type GroupId = string;

/**
 * View of a group for UI display (issue #5).
 *
 * A group's limits are shared by its members, so a management UI needs to
 * show the *category's* state — combined usage against the combined quota,
 * and whatever is currently restricting it — separately from any one member.
 */
export interface GroupView {
  /**
   * Effective daily quota after any override delta. None means unlimited.
   */
  daily_quota?: Duration | null;
  /**
   * Whether time spent on this category's members banks time toward some
   * other activity's gate. See `EntryView::earns_tokens`.
   */
  earns_tokens?: boolean;
  /**
   * Whether the group's own restrictions currently permit its members.
   * Individual members may still be unavailable for their own reasons.
   */
  enabled: boolean;
  group_id: GroupId;
  label: string;
  /**
   * Longest session the group's limits would currently allow a member.
   * None means the group imposes no cap of its own.
   */
  max_run_if_started_now?: Duration | null;
  /**
   * Members, in policy order.
   */
  member_ids: EntryId[];
  /**
   * Why the group is restricting its members, if it is. These are the
   * unwrapped reasons — the same ones members carry inside
   * `ReasonCode::GroupRestricted`.
   */
  reasons: ReasonCode[];
  /**
   * The category's token gate (issue #8), if it has one. Shared by every
   * member, so it belongs here rather than on any one of them.
   */
  tokens?: TokenStatus | null;
  /**
   * Combined usage across all members today.
   */
  used_today: Duration;
  /**
   * When the availability window the category is *currently inside* closes
   * (issue #207). `None` when the category is always available, has no
   * windows, or is outside all of them — in none of those cases is there a
   * closing time today to name.
   *
   * The launcher prints it on the compartment floor ("Until 6:00 PM"), which
   * is why it is a wall-clock time rather than the remaining duration
   * `max_run_if_started_now` already carries: that one is the *shortest* of
   * every limit the category imposes, so it says when the child must stop,
   * not when the category shuts.
   */
  window_closes_at?: IsoTimestamp | null;
}

/**
 * Health status
 */
export interface HealthStatus {
  host_adapter_ok: boolean;
  live: boolean;
  policy_loaded: boolean;
  ready: boolean;
  store_ok: boolean;
}

/**
 * Which screen edge the HUD occupies (issue #171).
 *
 * Configurable globally under `[service.hud]` and per entry, because the
 * right answer depends on both the hardware (a tall panel gives up less to a
 * side bar) and the activity (a game whose own UI lives along the top).
 *
 * The vertical form is "the HUD rotated 90 degrees to the left": same
 * controls, same order, read bottom-to-top with the end-session button at the
 * top. `Right` is deliberately not offered yet — nothing in the layout
 * forecloses it, but no config or code path ships for it.
 */
export type HudOrientation =
  /**
   * A horizontal bar along the top edge. The default, and what every device
   * shipped before issue #171 uses.
   */
  | "top"
  /**
   * A horizontal bar along the bottom edge.
   */
  | "bottom"
  /**
   * A vertical bar down the left edge.
   */
  | "left";

/**
 * Input compatibility mode for an activity.
 *
 * Some activities don't process raw touch or gamepad events from Wayland and
 * need a shim to translate input at the compositor level. Modes are mostly
 * orthogonal: an activity can stack `TouchToMouse` (or `TabletToTouch`, or
 * `DisableTouch`) with one of the `Gamepad*` modes. The touch-handling modes
 * are the exception — `TouchToMouse`, `TabletToTouch`, and `DisableTouch` all
 * grab or produce the touchscreen, so at most one of them can be active at a
 * time.
 */
export type InputCompatMode =
  /**
   * Grab touchscreens and emit synthesized pointer events via
   * `zwlr_virtual_pointer_v1` for the lifetime of the activity.
   */
  | "touch_to_mouse"
  /**
   * Grab absolute pointers / tablets and emit synthesized touch events for
   * activities that only handle touch input — the inverse of
   * `TouchToMouse`. Useful for developing touch support against
   * mouse/pen-only hardware, or VMs whose pointer is an absolute tablet.
   */
  | "tablet_to_touch"
  /**
   * Grab every touchscreen and discard its events for the lifetime of the
   * activity, effectively disabling the touchscreen. Unlike `TouchToMouse`
   * it emits nothing — useful for activities that misbehave on touch input
   * but should still be playable with a mouse or gamepad.
   */
  | "disable_touch"
  /**
   * Remap a gamepad to mouse + keyboard using the productivity preset:
   * triggers = LMB, shoulders = RMB, left stick = mouse, right stick =
   * scroll, stick-click toggles which stick drives the mouse, D-pad =
   * arrow keys, A = Enter, Start = Escape.
   */
  | "gamepad_productivity"
  /**
   * Remap a gamepad to mouse + keyboard using the GPD/FPS preset:
   * LT = LMB, RT = RMB, LB = MMB, left stick = WASD, right stick = mouse,
   * D-pad = scroll, A = Space, X = R, B = E, Y = F.
   */
  | "gamepad_gpd";

/**
 * A category of physical input device an activity can depend on (issue #96).
 *
 * Distinct from [`InputCompatMode`], which changes how input is *translated*
 * while an activity runs. `InputDeviceType` is a *gating* concept: an activity
 * can require one or more of these device types to be connected before it is
 * shown or launchable. The canonical example is a "learn to type" activity
 * installed on a gaming handheld that should only appear once a physical
 * keyboard is attached.
 *
 * Camera/microphone and MIDI are intentionally omitted for now; the issue
 * marks them as future work and this enum is closed, so configuring one is a
 * parse error rather than a silently-ignored value.
 */
export type InputDeviceType =
  /**
   * A relative pointing device (mouse, trackball, trackpad).
   */
  | "mouse"
  /**
   * A finger touchscreen (an absolute, direct-input touch device).
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
 * Status of a single internet connectivity check target
 */
export interface InternetStatusView {
  /**
   * Whether the last check succeeded
   */
  available: boolean;
  /**
   * Original check string as configured (e.g. "https://example.com")
   */
  target: string;
}

/**
 * A known Steam "launch interstitial" — one of the blocking modals Steam can
 * show between a launch request and the game actually starting (cloud-sync
 * warnings, controller advisories, etc.). The kiosk can be configured to
 * auto-dismiss specific kinds; see `service.steam.auto_dismiss_interstitials`.
 *
 * This enum is the canonical catalog: config validates against it, and the
 * host adapter attaches the per-kind CEF detection signatures.
 */
export type InterstitialKind =
  /**
   * "Unable to Sync" Steam Cloud warning shown when launching offline with
   * un-uploaded saves. Affirmative action: "Play anyway". (Verified.)
   */
  | "cloud_sync"
  /**
   * "Grab a controller…" advisory for controller-recommended games launched
   * without a controller. Affirmative action: "OK". (Verified.)
   */
  | "controller_recommended"
  /**
   * First-launch "intro to Steam Input" notice. Affirmative action: "OK".
   * (Best-effort signature.)
   */
  | "steam_input_intro"
  /**
   * Game *requires* a controller. Dismissing launches a game that cannot be
   * played without one, so this is risky. (Best-effort signature.)
   */
  | "controller_required"
  /**
   * Game requires a VR headset. Dismissing launches something unusable
   * without VR hardware, so this is risky. (Best-effort signature.)
   */
  | "vr_required";

export type LaunchOutcome =
  | {
      Approved: {
        deadline?: IsoTimestamp | null;
        session_id: string;
      };
    }
  | {
      Denied: {
        reasons: ReasonCode[];
      };
    };

/**
 * Something a limit can be attached to: an individual entry, or a group of
 * them (issue #5).
 *
 * Cooldowns, token balances, and daily overrides are all keyed by a subject so
 * that a group can carry the same state an entry can.
 *
 * The string form of an entry subject is the bare entry ID, and only groups
 * take the `group:` prefix. That keeps every pre-existing entry-keyed row and
 * API call valid without rewriting them — which is why entry IDs are forbidden
 * from starting with `group:` at config-validation time.
 */
export type LimitSubject = string;

/**
 * A login waiting on a tap in the companion app.
 */
export interface LoginRequestInfo {
  /**
   * The six digits the browser is displaying. The parent compares.
   */
  code: string;
  expires_at: IsoTimestamp;
  /**
   * The request's public handle — what `approve_login_request` takes.
   *
   * Not the same string the browser polls with. The browser's id is a
   * secret capability; this is a short opaque handle derived from it, so
   * that listing pending requests over BLE does not hand out the ability to
   * collect the resulting session.
   */
  id: string;
  /**
   * Who is asking, as best the device can tell: "Chrome on Android".
   */
  label: string;
  /**
   * The address the request came from.
   */
  peer: string;
  requested_at: IsoTimestamp;
}

/**
 * How a [`EntryKind::Media`] activity opens.
 */
export type MediaMode =
  /**
   * Open the poster grid over the whole library; the user picks items.
   */
  | "browse"
  /**
   * Play a single item end to end; the grid is never shown.
   */
  | "play";

/**
 * Maximum video quality for a [`EntryKind::Media`] activity.
 *
 * Mirrors `lunchbox_media_app::Quality`; kept here so the wire schema and the
 * config layer don't depend on the media crates. `lunchbox-media`'s `cli`
 * module holds the test that keeps the two spellings in agreement.
 */
export type MediaQuality =
  /**
   * No height restriction — the best available.
   */
  | "best"
  /**
   * Up to 1080p (default).
   */
  | "1080p"
  /**
   * Up to 720p.
   */
  | "720p"
  /**
   * Up to 480p.
   */
  | "480p";

/**
 * How a [`EntryKind::Media`] activity orders its library.
 *
 * Mirrors `lunchbox-media`'s `--sort-by` values; see [`MediaQuality`] for
 * where that agreement is tested.
 */
export type MediaSortBy =
  /**
   * Preserve the order from the library file or playlist (default).
   */
  | "library"
  /**
   * Display title, case-insensitive.
   */
  | "title"
  /**
   * Stable item id.
   */
  | "id"
  /**
   * Item kind (audio before video).
   */
  | "kind"
  /**
   * Optional category string, case-insensitive.
   */
  | "category"
  /**
   * Optional duration in seconds, ascending.
   */
  | "duration";

/**
 * One address on one interface.
 *
 * Address and prefix are separate fields rather than one CIDR string because
 * the address alone is what gets copied into an SSH command, and a UI should
 * not have to split on `/` to offer that.
 */
export interface NetworkAddressView {
  /**
   * The address on its own, e.g. `192.168.0.139`.
   */
  address: string;
  family: AddressFamily;
  /**
   * Prefix length in bits, e.g. `24`.
   */
  prefix: number;
}

/**
 * What kind of interface this is. Advisory: it drives presentation and which
 * addresses are offered as ways in, never policy.
 */
export type NetworkInterfaceKind =
  /**
   * A wireless interface. The one with a network name a person recognises.
   */
  | "wifi"
  /**
   * A wired interface.
   */
  | "ethernet"
  /**
   * A tunnel — WireGuard, ZeroTier, OpenVPN. Reachable, and on a device
   * administered remotely often the *only* thing reachable, which is why
   * `service.management_api.bind_retry_seconds` exists at all.
   */
  | "vpn"
  /**
   * A container or VM bridge (`lxcbr0`, `docker0`). Has an address; that
   * address is not a way in from the parent's phone.
   */
  | "bridge"
  /**
   * The host talking to itself. Never a way in.
   */
  | "loopback"
  /**
   * Something we could not name. Reported as a possible way in: being wrong
   * about a veth costs a line in a list, while being wrong about a real
   * interface costs the address somebody needed.
   */
  | "other";

/**
 * One network interface as an administrator sees it.
 */
export interface NetworkInterfaceView {
  addresses: NetworkAddressView[];
  /**
   * Nameservers configured for this interface.
   */
  dns: string[];
  gateway?: string | null;
  kind: NetworkInterfaceKind;
  /**
   * Kernel name, e.g. `wlp3s0`.
   */
  name: string;
  /**
   * Whether an address here is a plausible way to reach this device from
   * another machine on the same network.
   *
   * Derived, not reported by the host — see
   * [`NetworkStatusView::new`]. A UI leads with these and folds the rest
   * away: on a device running containers most interfaces are noise, and the
   * one a parent needs is the one they will not find by scrolling.
   */
  reachable?: boolean;
  /**
   * Whether the interface is up and configured.
   */
  up: boolean;
  /**
   * Present only on a wireless interface.
   */
  wifi?: WifiView | null;
}

/**
 * Where the status came from, so a UI can say why a field is missing rather
 * than rendering an empty box.
 */
export type NetworkSource =
  /**
   * NetworkManager over D-Bus: everything below is available.
   */
  | "network_manager"
  /**
   * The kernel's interface list. Addresses are real; SSID, gateway, DNS and
   * connectivity are not knowable this way and come back empty.
   */
  | "interfaces"
  /**
   * Neither worked.
   */
  | "unavailable";

/**
 * The whole read-out.
 */
export interface NetworkStatusView {
  connectivity: Connectivity;
  interfaces: NetworkInterfaceView[];
  management_api: WebListenerView;
  /**
   * URLs that should open the web management interface, most useful first.
   *
   * Derived here rather than in each UI so the phone and the browser agree,
   * and because the derivation is not obvious: a listener bound to
   * `0.0.0.0` has no address of its own, so the answer is one URL per
   * reachable address on the box — which is the whole point of the ticket.
   */
  management_urls?: string[];
  source: NetworkSource;
  /**
   * Whether [`MAX_NETWORK_INTERFACES`] hid anything. UIs must say so.
   */
  truncated?: boolean;
}

/**
 * Structured reason codes for why an entry is unavailable
 */
export type ReasonCode =
  /**
   * Outside allowed time window
   */
  | {
      code: "outside_time_window";
      /**
       * When the next window opens (if known)
       */
      next_window_start?: IsoTimestamp | null;
    }
  /**
   * Daily quota exhausted
   */
  | {
      code: "quota_exhausted";
      quota: Duration;
      used: Duration;
    }
  /**
   * Cooldown period active
   */
  | {
      code: "cooldown_active";
      available_at: IsoTimestamp;
    }
  /**
   * Another session is active
   */
  | {
      code: "session_active";
      entry_id: EntryId;
      /**
       * Time remaining in current session. None means unlimited.
       */
      remaining?: Duration | null;
    }
  /**
   * Host doesn't support this entry kind
   */
  | {
      code: "unsupported_kind";
      kind: EntryKindTag;
    }
  /**
   * The activity kind has not finished warming up yet (e.g. Steam is still
   * performing its initial load). See per-kind readiness (issue #76).
   */
  | {
      code: "not_ready";
      kind: EntryKindTag;
    }
  /**
   * Entry is explicitly disabled
   */
  | {
      code: "disabled";
      reason?: string | null;
    }
  /**
   * Internet connectivity is required but unavailable
   */
  | {
      code: "internet_unavailable";
      check?: string | null;
    }
  /**
   * Entry is manually disabled for the day via a daily override
   */
  | {
      code: "manually_disabled";
      until: IsoDate;
    }
  /**
   * The device is in administrator mode (issue #154), so nothing launches as
   * an activity. Not a restriction on the child in the sense the others are:
   * it clears the moment the caregiver leaves the mode, and it applies to
   * every entry at once.
   */
  | { code: "admin_mode" }
  /**
   * One or more required input devices (issue #96) are not currently
   * connected. `devices` lists the missing device types, sorted and
   * deduplicated.
   */
  | {
      code: "required_input_unavailable";
      devices: InputDeviceType[];
    }
  /**
   * A protection this entry's configuration requires cannot be applied on
   * this host, so the entry does not launch (issue #143) — today, an
   * `[entries.firewall]` on a host where enforcement is unavailable.
   *
   * Carries no detail on purpose. This is the child-facing half: to them the
   * activity is simply unavailable, and nothing they can do changes it. The
   * administrator-facing half — which protection, why, and how to fix it —
   * is the matching `Diagnostic`.
   */
  | { code: "protection_unavailable" }
  /**
   * Not enough time banked on this entry's token gate (issue #8): the
   * activity has to be earned by spending time on its source activities.
   */
  | {
      code: "tokens_insufficient";
      /**
       * Time currently banked toward this entry.
       */
      balance: Duration;
      /**
       * Balance needed before it unlocks. Zero means any balance above zero
       * unlocks it, i.e. the entry is simply out of banked time.
       */
      required: Duration;
    }
  /**
   * The restriction comes from the entry's group rather than the entry
   * itself (issue #5) — e.g. the whole category's daily quota is spent.
   * `label` is the group's display name, for explaining it to a caregiver.
   */
  | {
      code: "group_restricted";
      group: GroupId;
      label: string;
      reason: ReasonCode;
    };

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

/**
 * A saved profile, as a list of known networks shows it.
 *
 * **Never carries a secret.** There is no field for one, and no method
 * returns one: a stored password is write-only from every management UI. A
 * parent who has forgotten theirs re-types it; the device will not read it
 * back to them, because the same call would read it back to anything else
 * that could reach the API.
 */
export interface SavedWifiNetwork {
  /**
   * Whether this profile is the active one.
   */
  active: boolean;
  /**
   * Whether NetworkManager may join this on its own.
   */
  autoconnect: boolean;
  /**
   * Saved for a network that does not broadcast its name.
   */
  hidden: boolean;
  /**
   * Stable handle for [`connect`](WifiJoinRequest) and forget.
   *
   * NetworkManager's connection UUID, and not the SSID, because an SSID is
   * not unique: this dev device carries two profiles for one printer's
   * network, written by GNOME months apart with different security.
   */
  id: string;
  security: WifiSecurity;
  ssid: string;
}

/**
 * Full service state snapshot
 */
export interface ServiceStateSnapshot {
  /**
   * Whether the device is in administrator mode (issue #154) — the kiosk's
   * restrictions relaxed so a caregiver can set activities up in place.
   *
   * Every client that behaves differently in the mode reads it from here
   * rather than tracking it: the shells change what they draw, and the
   * window panels stop calling admin-launched windows orphans. (The screen
   * staying awake is not one of them — that check moved inside the daemon
   * with issue #144, and `set_screen_power` reads the engine directly.)
   * Absent from an older payload means "not in admin mode", which is the
   * safe reading.
   */
  admin_mode?: boolean;
  api_version: number;
  current_session?: SessionInfo | null;
  /**
   * Administrator-facing conditions currently true of this device (issue
   * #143) — a missing dependency, a protection that is not in effect. Rides
   * the snapshot so every client has the current set on subscribe; deltas
   * arrive as `EventPayload::DiagnosticsChanged`.
   */
  diagnostics?: DiagnosticSet;
  /**
   * Available entries for UI display
   */
  entries?: EntryView[];
  entry_count: number;
  /**
   * The categories those entries belong to (issue #5), in policy order.
   *
   * Rides the snapshot rather than being a separate `list_groups` call
   * (issue #207): the launcher draws one compartment per category and has to
   * redraw on every `StateChanged`, so fetching them apart from the entries
   * would both double the round trips and let the two drift — a category's
   * banked time and its members' could be a moment out of step, which is
   * exactly what the child would notice.
   *
   * Empty from an older payload, which reads as "no categories" and renders
   * as the flat grid that predates compartments.
   */
  groups?: GroupView[];
  /**
   * Latest known status of each configured internet connectivity check.
   * Empty when no connectivity checks are configured.
   */
  internet_status?: InternetStatusView[];
  /**
   * Whether the screen is locked (issue #154).
   *
   * Only ever set inside administrator mode: it is what makes walking away
   * from a half-configured device safe, and it is deliberately not something
   * a child's session can enter. Clearing it is a management RPC — there is
   * no local affordance, which is the entire point.
   */
  locked?: boolean;
  policy_loaded: boolean;
}

/**
 * Session end reason
 */
export type SessionEndReason =
  /**
   * Session expired (time limit reached)
   */
  | { type: "expired" }
  /**
   * User requested stop
   */
  | { type: "user_stop" }
  /**
   * Admin requested stop
   */
  | { type: "admin_stop" }
  /**
   * Process exited on its own
   */
  | {
      type: "process_exited";
      exit_code?: number | null;
    }
  /**
   * Policy change terminated session
   */
  | { type: "policy_stop" }
  /**
   * Service shutdown
   */
  | { type: "service_shutdown" }
  /**
   * The daemon stopped without settling this session — a power cut, a
   * crash, or a kill — and it was recovered from the store's snapshot at
   * the next startup (issue #201).
   *
   * Distinct from [`Self::ServiceShutdown`] on purpose: that one is an
   * orderly exit that settled the session itself, and this one is the
   * record that something took the daemon out from under a child mid-play.
   * The duration is what the last checkpoint saw, so it is a lower bound.
   */
  | { type: "interrupted" }
  /**
   * Launch failed
   */
  | {
      type: "launch_failed";
      error: string;
    };

/**
 * Unique identifier for a running session
 */
export type SessionId = string;

/**
 * Active session information
 */
export interface SessionInfo {
  /**
   * Whether the HUD should offer a reset button for this session — see
   * [`EntryKind::supports_reset`]. Defaults to `false` when absent, so an
   * older payload simply doesn't show the button.
   */
  can_reset?: boolean;
  /**
   * Whether the HUD should show page-turn buttons for this session. See
   * [`EntryKind::supports_page_turn`].
   */
  can_turn_pages?: boolean;
  /**
   * Whether the HUD should confirm before its "X" button ends this
   * session (issue #78). Defaults to `true` when absent so older payloads
   * keep the safe behaviour.
   */
  confirm_on_close?: boolean;
  /**
   * Session deadline. None means unlimited (no time limit).
   */
  deadline?: IsoTimestamp | null;
  entry_id: EntryId;
  label: string;
  session_id: SessionId;
  started_at: IsoTimestamp;
  state: SessionState;
  /**
   * Time remaining. None means unlimited.
   */
  time_remaining?: Duration | null;
  warnings_issued: number[];
}

/**
 * Current session state
 */
export type SessionState =
  /**
   * Approved and spawning; the activity has not mapped a window yet.
   */
  | "launching"
  /**
   * The activity is running normally.
   */
  | "running"
  /**
   * Running, and at least one time warning has been issued.
   */
  | "warned"
  /**
   * Past its deadline and being wound down.
   */
  | "expiring"
  /**
   * Teardown has been requested and the activity is being stopped.
   *
   * The session is still current: the activity is on screen until the host
   * confirms otherwise, so nothing else may launch and shells must keep the
   * launcher out of the way. Shells should render this as a
   * non-interactive "closing" state — without it a child gets no feedback
   * that their press registered, which is why they pressed again on
   * 2026-08-20 (issue #136).
   */
  | "stopping"
  /**
   * Settled and cleared; no activity is running.
   */
  | "ended";

/**
 * Stop mode for session termination
 */
export type StopMode =
  /**
   * Try graceful termination first
   */
  | "graceful"
  /**
   * Force immediate termination
   */
  | "force";

/**
 * A token gate's current state, for caregiver UIs (issue #8).
 *
 * Banked time is a currency: source activities earn it and the gated activity
 * spends it. Without this a management UI can only report that something is
 * locked, never how close it is to unlocking, and a manual grant would be
 * made blind.
 */
export interface TokenStatus {
  /**
   * Time banked and not yet spent.
   */
  balance: Duration;
  /**
   * Whether the balance survives local midnight.
   */
  carry_over: boolean;
  /**
   * Ceiling on the balance. None means unlimited. A grant past this is
   * clawed back, so a UI should say so rather than let it vanish.
   */
  max_balance?: Duration | null;
  /**
   * Balance needed to open the gate. Zero means any balance opens it.
   */
  minimum: Duration;
  /**
   * Whether the gate is open right now: the balance is above zero and at
   * least `minimum`. That holds every time, not only the first (issue #193).
   */
  unlocked: boolean;
}

/**
 * Screen-time usage for a single entry on a single day
 */
export interface UsageStat {
  date: IsoDate;
  duration_seconds: number;
  entry_id: EntryId;
  label: string;
}

/**
 * Volume status information
 */
export interface VolumeInfo {
  /**
   * Whether volume control is available
   */
  available: boolean;
  /**
   * The detected sound backend (e.g., "pipewire", "pulseaudio", "alsa")
   */
  backend?: string | null;
  /**
   * Whether audio is muted
   */
  muted: boolean;
  /**
   * The output this reading applies to. `None` on hosts without PipeWire, or
   * when the default sink cannot be resolved to a known output.
   */
  output?: AudioOutput | null;
  /**
   * Volume percentage (0-100)
   */
  percent: number;
  /**
   * Current restrictions on volume
   */
  restrictions: VolumeRestrictions;
}

/**
 * Volume restrictions that are currently in effect
 */
export interface VolumeRestrictions {
  /**
   * Whether volume changes are allowed at all
   */
  allow_change: boolean;
  /**
   * Whether mute toggle is allowed
   */
  allow_mute: boolean;
  /**
   * Maximum volume percentage allowed
   */
  max_volume?: number | null;
  /**
   * Minimum volume percentage allowed
   */
  min_volume?: number | null;
}

/**
 * Warning severity level
 */
export type WarningSeverity =
  | "info"
  | "warn"
  | "critical";

/**
 * Warning threshold configuration
 */
export interface WarningThreshold {
  message_template?: string | null;
  /**
   * Seconds before expiry to issue this warning
   */
  seconds_before: number;
  severity: WarningSeverity;
}

/**
 * What a client may know about the device's authentication state *before* it
 * has authenticated. Deliberately thin — it says which door to knock on and
 * nothing else.
 */
export interface WebAuthStatus {
  /**
   * Whether a paired companion exists to approve a login. False means the
   * password is the only way in, so the UI should not offer the other.
   */
  companion_available: boolean;
  /**
   * False on a device where nobody has set a password yet: the browser
   * should show the setup screen and ask for the code on the TV.
   */
  configured: boolean;
}

/**
 * Whether the web management interface is up, and where.
 *
 * The reason this is not simply the configured `bind`/`port`: the daemon
 * retries a bind that is not yet available (a ZeroTier interface still coming
 * up at login), and a bind that never succeeds only ever reached a log line.
 * From every UI, a device whose management API never came up looked exactly
 * like one that did.
 */
export type WebListenerState =
  /**
   * `service.management_api` is absent or disabled. Nothing is wrong.
   */
  | "disabled"
  /**
   * Configured, and still waiting for its address to exist.
   */
  | "binding"
  /**
   * Serving.
   */
  | "listening"
  /**
   * Configured and not serving. Somebody should know.
   */
  | "failed";

/**
 * Where the web management interface is listening, if at all.
 */
export interface WebListenerView {
  /**
   * The socket address as configured, e.g. `0.0.0.0:8080`. `None` only when
   * [`WebListenerState::Disabled`].
   */
  addr?: string | null;
  /**
   * Why it is not serving. Only set with [`WebListenerState::Failed`].
   */
  error?: string | null;
  /**
   * The port on its own, for building a URL against some other address.
   */
  port?: number | null;
  state: WebListenerState;
  /**
   * Whether the listener terminates TLS (issue #156).
   *
   * Decides the scheme in [`NetworkStatusView::management_urls`], which is
   * not cosmetic: a device serving HTTPS answers a plaintext request with a
   * connection reset, so an `http://` URL for it sends a parent to debug
   * their browser instead of opening their device.
   */
  tls?: boolean;
}

/**
 * One live browser session, as an administrator sees it.
 *
 * Carries no credential: `id` is a public handle used to revoke the session,
 * not the token that authenticates it. The token itself is stored hashed and
 * is never readable back out of this module.
 */
export interface WebSessionInfo {
  created_at: IsoTimestamp;
  /**
   * True for the session making the request, so the UI can label it and
   * warn before revoking it.
   */
  current: boolean;
  expires_at: IsoTimestamp;
  id: string;
  /**
   * Human label derived from the User-Agent at login — "Chrome on Android",
   * not a hex string, because the person revoking sessions is choosing
   * between their own devices.
   */
  label: string;
  last_seen: IsoTimestamp;
  /**
   * The address the session logged in from, for the same reason.
   */
  peer: string;
}

/**
 * Why a join ended without a network, and — where there is one — the
 * backend's own word for it.
 *
 * A kind plus an optional detail rather than an enum carrying payloads. The
 * payload-carrying shape has no Kotlin equivalent the generator can render,
 * and this one is easier to switch on in both UIs anyway: a UI maps `kind` to
 * a sentence a parent can act on, and shows `detail` only where it has
 * nothing better to say.
 */
export interface WifiJoinFailure {
  /**
   * The backend's own description, for
   * [`WifiJoinFailureKind::Rejected`] and
   * [`WifiJoinFailureKind::Other`]. `None` for the kinds whose meaning is
   * already in the kind.
   */
  detail?: string | null;
  kind: WifiJoinFailureKind;
}

/**
 * Why a join ended without a network.
 *
 * Every variant is a distinct thing to *do* about it, which is the only
 * reason to distinguish them. Measured against NetworkManager 1.54.3 —
 * association failures surface on the device's `StateChanged`, never on the
 * active connection, which reports a generic disconnect for all of these.
 */
export type WifiJoinFailureKind =
  /**
   * The key was refused. NetworkManager reason 7, `no-secrets`.
   *
   * With no secret agent in the kiosk session there is nothing to re-prompt,
   * so a wrong key fails instead of hanging — which is what makes this
   * reportable at all.
   */
  | "wrong_password"
  /**
   * No access point with that name answered. Reason 53, `ssid-not-found`.
   * Out of range, switched off, or — for a network that does not broadcast
   * — saved without `hidden` set.
   */
  | "not_found"
  /**
   * Associated, and then no address. Reason 5, `ip-config-unavailable`.
   *
   * The key was right and the radio link came up; DHCP did not answer.
   * Distinct from [`Self::WrongPassword`] because the thing to check is the
   * router, not the key — and a parent told "wrong password" here will
   * retype a correct one until they give up.
   */
  | "no_address"
  /**
   * The device may not write a profile: no polkit grant, and no custodian
   * to borrow one from.
   */
  | "not_authorized"
  /**
   * It was refused before NetworkManager saw it.
   */
  | "rejected"
  /**
   * Anything else. `detail` names it as NetworkManager named it, so a log
   * is useful even for a case nobody anticipated.
   */
  | "other";

/**
 * Save a network, and maybe join it now.
 *
 * One request for both because they differ by one bool and every other field
 * is shared — and because "remember this" and "get on it" are the same act
 * from a parent's side, distinguished only by where they are standing.
 */
export interface WifiJoinRequest {
  /**
   * `true` joins now. `false` only remembers it — what the web leads with,
   * because joining from a browser can cut off the browser.
   */
  connect: boolean;
  /**
   * Whether this network does not broadcast its name. Set from the manual
   * form; a network picked from a scan was broadcasting by definition.
   */
  hidden?: boolean;
  /**
   * The key. `None` for [`WifiSecurity::Open`] and [`WifiSecurity::Owe`],
   * which have none.
   */
  password?: string | null;
  security: WifiSecurity;
  ssid: string;
}

/**
 * What the most recent join is doing.
 */
export type WifiJoinState =
  /**
   * None has been asked for since the daemon started.
   */
  | { state: "idle" }
  /**
   * Asked for, and not yet settled.
   */
  | {
      state: "connecting";
      ssid: string;
    }
  /**
   * On the network.
   */
  | {
      state: "connected";
      ssid: string;
    }
  /**
   * Over, without a network.
   */
  | {
      state: "failed";
      reason: WifiJoinFailure;
      ssid: string;
    };

/**
 * One network in a scan, aggregated across every access point announcing it.
 *
 * A home mesh puts the same SSID on three radios in two bands; a picker that
 * listed each would ask a parent to choose between three identical rows. So
 * entries are merged by (name, security) and the best signal wins.
 *
 * No BSSID, matching #182's stance on MAC addresses: it is not a fact anybody
 * picking a network needs, and it identifies hardware.
 */
export interface WifiNetwork {
  /**
   * Whether this is the network the device is on right now.
   */
  active: boolean;
  /**
   * Which bands it was heard on, in GHz, ascending — `[2]`, `[5]`, or
   * `[2, 5]` for a network on both.
   */
  bands_ghz: number[];
  /**
   * Whether a saved profile already exists for this network.
   */
  saved: boolean;
  security: WifiSecurity;
  /**
   * Signal quality 0–100, the strongest among the access points announcing
   * this network.
   */
  signal_percent: number;
  /**
   * The network's name. Always valid UTF-8 here: an SSID is up to 32
   * arbitrary octets, and one that is not text cannot be shown in a picker
   * or typed into the manual form, so it is left out of the scan entirely.
   */
  ssid: string;
}

/**
 * Everything one poll of the wireless state returns.
 *
 * Scan list and join progress arrive together deliberately. The UI is already
 * polling this to refresh signal strengths while a parent looks at the list;
 * making the join's outcome ride along means no second timer, no second
 * endpoint, and no window where the list has updated but the result has not.
 */
export interface WifiScanView {
  /**
   * Whether this device may write a profile at all.
   *
   * False on a device whose custodian holds no NetworkManager grant. The
   * UIs use it to disable the forms up front and point at the remedy,
   * rather than letting a parent type a password into a box that was never
   * going to work. The matching Health diagnostic says the same thing in
   * the place people look for problems.
   */
  can_configure: boolean;
  /**
   * What the most recent join is doing. [`WifiJoinState::Idle`] when none
   * has been asked for since the daemon started.
   */
  join: WifiJoinState;
  /**
   * How long ago the last scan completed. `None` when no scan has run since
   * boot.
   *
   * Seconds, and derived on the host, because NetworkManager reports this
   * as a `CLOCK_BOOTTIME` reading that means nothing on the phone reading
   * it.
   */
  last_scan_age_s?: number | null;
  /**
   * Networks in range, strongest first, capped at [`MAX_WIFI_NETWORKS`].
   */
  networks: WifiNetwork[];
  /**
   * Whether the radio is on. Reported, never changed — toggling it is out
   * of scope, and a device switched off at the rfkill has to be fixed where
   * it is.
   */
  radio_enabled: boolean;
  /**
   * Whether this device has a wireless adapter at all. Everything below is
   * empty when it does not, and a UI says so rather than showing an empty
   * list that looks like a failed scan.
   */
  supported: boolean;
  /**
   * Whether [`MAX_WIFI_NETWORKS`] hid anything.
   */
  truncated?: boolean;
}

/**
 * How a network is protected.
 *
 * Derived from an access point's beacon flags, which is the only thing a scan
 * can tell us. The mapping is measured against real beacons — see
 * [`WifiSecurity::from_ap_flags`].
 */
export type WifiSecurity =
  /**
   * No protection at all. Joinable with no key.
   */
  | "open"
  /**
   * Enhanced Open (OWE). Encrypted, still no key to type.
   */
  | "owe"
  /**
   * WPA/WPA2 Personal. Also the right choice for a WPA2/WPA3 transition
   * access point, because every card that can see one can join it this way.
   */
  | "wpa_psk"
  /**
   * WPA3 Personal (SAE).
   */
  | "sae"
  /**
   * 802.1X. Recognised so it can be shown as unsupported; see
   * [`WifiSecurity::joinable`].
   */
  | "enterprise"
  /**
   * WEP. Recognised for the same reason, and it is not coming back.
   */
  | "wep";

/**
 * The wireless network an interface is associated with.
 */
export interface WifiView {
  /**
   * Channel centre frequency in MHz. A UI can turn 5220 into "5 GHz", which
   * is the part a person debugging a weak signal actually wants.
   */
  frequency_mhz?: number | null;
  /**
   * Signal quality, 0–100, as the driver reports it.
   */
  signal_percent?: number | null;
  /**
   * The network's name.
   *
   * `None` when the interface is not associated — and also when the SSID is
   * not valid UTF-8, which is legal: 802.11 carries an SSID as up to 32
   * arbitrary octets, not a string. A name we cannot render is reported as
   * no name rather than as mojibake.
   */
  ssid?: string | null;
}

/**
 * An action that can be performed on a window through the management API.
 */
export type WindowAction =
  /**
   * Ask the window to close (sway `kill`).
   */
  | "close"
  /**
   * Move the window to the scratchpad to hide it from view.
   */
  | "hide"
  /**
   * Pull the window out of the scratchpad so it is shown again.
   */
  | "show"
  /**
   * Give the window keyboard focus, raising it above the others (sway
   * `focus`).
   *
   * Note that on a window currently *on* the scratchpad this also pulls it
   * off — verified against sway 1.11, where focusing a stashed window
   * clears `in_scratchpad` and makes it visible — so it overlaps
   * [`WindowAction::Show`] for that case rather than being a no-op.
   * Clients that list the two placements separately should therefore still
   * offer `Show` on a scratchpad row and `Focus` on an on-screen one, so
   * each row has one obvious action, not because `Focus` would fail there.
   */
  | "focus";

/**
 * Debug snapshot of a single window known to the host's compositor.
 *
 * Currently surfaced via the management API for debugging the Sway tree —
 * in particular, to see which windows have been moved to the scratchpad
 * (e.g. the hidden Steam client) versus which are on-screen.
 */
export interface WindowInfo {
  /**
   * Wayland app_id, if available.
   */
  app_id?: string | null;
  /**
   * True if the window has keyboard focus.
   */
  focused: boolean;
  /**
   * Compositor-assigned window/container id.
   */
  id: number;
  /**
   * True if the window currently lives on the scratchpad (hidden).
   */
  in_scratchpad: boolean;
  /**
   * Window title, if the application set one.
   */
  name?: string | null;
  /**
   * What Lunchbox is supervising behind this window, if anything.
   *
   * A host that cannot attribute windows reports every one of them as
   * unowned.
   */
  owner: WindowOwner;
  /**
   * Owning process id, if reported by the compositor.
   */
  pid?: number | null;
  /**
   * True if the window is currently being rendered.
   */
  visible: boolean;
  /**
   * X11 class (xwayland windows), if available.
   */
  window_class?: string | null;
  /**
   * Workspace name the window belongs to, if any. `__i3_scratch` is the
   * scratchpad pseudo-workspace.
   */
  workspace?: string | null;
}

/**
 * Who Lunchbox believes a window belongs to.
 *
 * The compositor cannot answer this — it reports pids, not intent. The host
 * fills it in by matching each window against what it is actually
 * supervising, which is what lets an admin UI tell "the game the child is
 * playing" apart from "something on the screen that no session owns".
 */
export type WindowOwner =
  /**
   * Lunchbox's own furniture: the launcher, the HUD, the pairing UI, the
   * mirror, and background processes it keeps warm (the preloaded Steam
   * client). Expected to outlive every session.
   */
  | "lunchbox"
  /**
   * A process Lunchbox is supervising for the current session — the
   * activity itself, something in its process group, a Steam game
   * launched on its behalf, or one of its input sidecars.
   */
  | "activity"
  /**
   * An activity that outlived its own teardown. Its session is over and
   * the host is still working on killing it — the same condition that
   * writes an `ActivityEscaped` audit record.
   */
  | "escaped"
  /**
   * No process Lunchbox knows about. Either something started outside
   * Lunchbox entirely, or an activity that got away without the host ever
   * noticing — the case supervision cannot fix on its own, and the reason
   * this field exists.
   */
  | "unowned";
