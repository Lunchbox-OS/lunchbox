// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from the Rust wire types by
// `cargo run -p shepherd-wire-codegen --bin rpc-codegen`.
// Edit `crates/shepherd-api/src/types.rs` and re-run instead.
//
// Property names are the wire form (snake_case), because that is what the
// daemon sends and nothing renames them in transit.

/** An RFC 3339 timestamp. A `string`; the alias records the intent. */
export type IsoTimestamp = string;

/** A calendar date, `YYYY-MM-DD`. */
export type IsoDate = string;

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
 * These rows are how per-output limits are configured: shepherdd records every
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

export type ClaimStateTag =
  | "unclaimed"
  | "claimed";

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
   * shepherd cannot talk to the compositor, so it cannot see what is on
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
   * shepherdd's own management socket is reachable by processes that are
   * not part of the session — the peer allow-list is not armed, or it is
   * armed somewhere it cannot mean anything (issue #144).
   *
   * Like [`Self::CompositorNotHardened`], the session is deliberately left
   * running, so nothing else about the device looks wrong and the downgrade
   * is invisible unless it is said out loud.
   */
  | "ipc_socket_not_hardened"
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
  | "retroarch_content_missing";

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
   * A `shepherd-media` library activity (issue #127).
   *
   * The fields mirror the flags `shepherd-media` accepts, so shepherdd can
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
       * Whether shepherdd may prefetch this library's remote items in the
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
   * is never edited. See `shepherd-host-linux::retroarch`.
   */
  | {
      type: "retroarch";
      /**
       * Extra arguments, appended after the ones shepherd derives.
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
  | "custom";

/**
 * View of an entry for UI display
 */
export interface EntryView {
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
   * Emitted by shepherdd when it temporarily drops the compositor's
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
   * clock, battery, or activity list). shepherdd holds a logind delay
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
 * Mirrors `shepherd_media_app::Quality`; kept here so the wire schema and the
 * config layer don't depend on the media crates. `shepherd-media`'s `cli`
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
 * Mirrors `shepherd-media`'s `--sort-by` values; see [`MediaQuality`] for
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
 * Full service state snapshot
 */
export interface ServiceStateSnapshot {
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
   * Latest known status of each configured internet connectivity check.
   * Empty when no connectivity checks are configured.
   */
  internet_status?: InternetStatusView[];
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
   * Whether the gate is open right now. Not simply `balance >= minimum`:
   * once opened it stays open until the balance is spent to zero.
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
 * An action that can be performed on a window via the debug API.
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
  | "show";

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
   * What shepherd is supervising behind this window, if anything.
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
 * Who shepherd believes a window belongs to.
 *
 * The compositor cannot answer this — it reports pids, not intent. The host
 * fills it in by matching each window against what it is actually
 * supervising, which is what lets an admin UI tell "the game the child is
 * playing" apart from "something on the screen that no session owns".
 */
export type WindowOwner =
  /**
   * Shepherd's own furniture: the launcher, the HUD, the pairing UI, the
   * mirror, and background processes it keeps warm (the preloaded Steam
   * client). Expected to outlive every session.
   */
  | "shepherd"
  /**
   * A process shepherd is supervising for the current session — the
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
   * No process shepherd knows about. Either something started outside
   * shepherd entirely, or an activity that got away without the host ever
   * noticing — the case supervision cannot fix on its own, and the reason
   * this field exists.
   */
  | "unowned";
