// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from the Rust wire types by
// `cargo run -p shepherd-wire-codegen --bin rpc-codegen`.
// Edit `crates/shepherd-api/src/types.rs` and re-run instead.
//
// Helper affordances (extension properties, custom serializers, and the
// types listed as hand-written in `kotlin_types.rs`) live in
// `Models.kt` alongside this file.

@file:OptIn(ExperimentalSerializationApi::class)

package com.armeafamily.shepherd.companion.domain

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonClassDiscriminator
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.modules.SerializersModule
import kotlinx.serialization.modules.polymorphic

/** ISO-8601 timestamp with offset, e.g. "2026-06-21T18:05:00-04:00". */
typealias IsoTimestamp = String

/** ISO-8601 local date, e.g. "2026-06-21". */
typealias IsoDate = String

@Serializable
data class AdminRecord(
    /**
     * `"public"` or `"random"` — matches `bluer`'s `AddressType` enum
     * so the server can compare on reconnect without a parse step.
     */
    val addressType: String,
    val bondedAt: IsoTimestamp,
    val deviceName: String,
    /**
     * Bearer token also accepted by the HTTP API. See the unified-identity
     * section of the BLE management design.
     */
    val httpToken: String,
    /**
     * The BlueZ-resolved identity address for the bonded peer. Once
     * pairing completes BlueZ presents this address regardless of the
     * peer's random MAC rotation, so it doubles as the stable identity.
     */
    val identityAddress: String,
    val role: AdminRole,
)

@Serializable
enum class AdminRole {
    @SerialName("admin") ADMIN,
}

/**
 * The audio output a volume reading applies to.
 *
 * `key` is `<device.name>:output:<route.name>` — the same key WirePlumber uses
 * to persist per-route volume, so our notion of "an output" cannot drift from
 * the volume PipeWire remembers for it. It is stable across reboots and, for
 * USB devices, across being moved to a different port.
 */
@Serializable
data class AudioOutput(
    /**
     * Human-readable label for display. Localized and mutable.
     */
    val description: String,
    /**
     * Stable identity. Use this to correlate, never the description.
     */
    val key: String,
    val kind: AudioOutputKind,
)

/**
 * What kind of thing an audio output is.
 *
 * Advisory only: it drives presentation (an icon, a label) and never policy.
 * It cannot be determined for every device — a generic USB interface reports a
 * nondescript `analog-output` route and no udev form-factor — so `Unknown` is a
 * routine outcome, not a failure.
 */
@Serializable
enum class AudioOutputKind {
    @SerialName("speakers") SPEAKERS,
    @SerialName("headphones") HEADPHONES,
    @SerialName("hdmi") HDMI,
    @SerialName("digital") DIGITAL,
    @SerialName("line_out") LINE_OUT,
    @SerialName("bluetooth") BLUETOOTH,
    @SerialName("unknown") UNKNOWN,
}

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
@Serializable
data class AudioOutputRecord(
    /**
     * Whether this is the output currently selected. Runtime state, not stored.
     */
    val active: Boolean,
    /**
     * Whether the device is plugged in right now, so it can be switched to.
     *
     * Rows outlive the hardware — that is the point, so a cap set on the
     * headphones survives unplugging them — which means a row can name a device
     * that is not here. Defaults to `true` so a client talking to a daemon that
     * predates this field offers the choice and lets the attempt fail loudly,
     * rather than greying out every device it could actually switch to.
     */
    val available: Boolean = true,
    /**
     * When the device last observed this output. Lets the UI show recently used
     * devices first and lets a parent prune ones that are long gone.
     */
    val lastSeen: IsoTimestamp,
    /**
     * Cap for this output. `None` means no per-output cap; the global
     * `[service.volume]` limit applies instead.
     */
    val maxVolume: Long? = null,
    /**
     * Floor for this output. `None` means no per-output floor.
     */
    val minVolume: Long? = null,
    /**
     * Identity, display label, and advisory kind.
     */
    val output: AudioOutput,
)

/**
 * Screen brightness status information
 */
@Serializable
data class BrightnessInfo(
    /**
     * Whether an ambient light sensor is present, so automatic brightness
     * can be offered at all. When false, `auto_enabled` is always false.
     */
    val autoAvailable: Boolean = false,
    /**
     * Whether automatic (ambient-light) brightness is currently enabled.
     */
    val autoEnabled: Boolean = false,
    /**
     * Whether brightness control is available on this host
     */
    val available: Boolean,
    /**
     * The detected brightness backend (e.g. "sysfs", "brightnessctl")
     */
    val backend: String? = null,
    /**
     * Name of the backlight device being controlled, if any
     */
    val device: String? = null,
    /**
     * Brightness percentage (0-100)
     */
    val percent: Long,
    /**
     * Current restrictions on brightness
     */
    val restrictions: BrightnessRestrictions,
)

/**
 * Brightness restrictions that are currently in effect
 */
@Serializable
data class BrightnessRestrictions(
    /**
     * Whether brightness changes are allowed at all
     */
    val allowChange: Boolean,
    /**
     * Maximum brightness percentage allowed
     */
    val maxBrightness: Long? = null,
    /**
     * Minimum brightness percentage allowed
     */
    val minBrightness: Long? = null,
)

@Serializable
enum class ClaimStateTag {
    @SerialName("unclaimed") UNCLAIMED,
    @SerialName("claimed") CLAIMED,
}

/**
 * A parent-set daily override for a single entry
 */
@Serializable
data class DailyOverride(
    /**
     * Override the entry's availability for this day.
     * `Some(false)` blocks it entirely; `Some(true)` allows it outside its time window.
     * `None` means no availability override (quota delta may still apply).
     */
    val availability: Boolean? = null,
    val createdAt: IsoTimestamp,
    val date: IsoDate,
    /**
     * Signed adjustment to today's quota in seconds.
     * Positive = extra time; negative = reduced time; `None` = no change.
     */
    val quotaDeltaSeconds: Long? = null,
    /**
     * What the override applies to: an entry, or a whole group (issue #5).
     * Serializes as a bare entry ID, or `group:<id>` for a group, so overrides
     * written before groups existed round-trip unchanged.
     */
    val subject: LimitSubject,
    val updatedAt: IsoTimestamp,
)

/**
 * Shape returned from the `DeviceInfo` characteristic. Readable
 * unencrypted; carries only what the companion app needs to decide
 * whether to initiate pairing.
 */
@Serializable
data class DeviceInfo(
    val claimState: ClaimStateTag,
    val deviceName: String,
    val firmwareVersion: String,
    val protocolVersion: Long,
)

/**
 * One condition that is currently true.
 */
@Serializable
data class Diagnostic(
    val code: DiagnosticCode,
    /**
     * One line, for a person. Most of these already exist verbatim as the log
     * message the diagnostic replaces.
     */
    val message: String,
    /**
     * What to do about it, when there is a concrete answer — a command to run,
     * a group to join. `None` when the fix is not something we can name.
     */
    val remedy: String? = null,
    val severity: DiagnosticSeverity,
    /**
     * When this condition was first observed. Preserved across a re-raise, so
     * "since" means since it started, not since it was last checked.
     */
    val since: IsoTimestamp,
    val subject: DiagnosticSubject,
)

/**
 * What is wrong. An enum rather than a string so the UIs can special-case
 * presentation and the wire drift test covers the variant set.
 */
@Serializable
enum class DiagnosticCode {
    /**
     * Per-entry firewall enforcement is unavailable on this host — the helper
     * is not installed, or polkit denies it.
     */
    @SerialName("firewall_unenforceable") FIREWALL_UNENFORCEABLE,
    /**
     * This entry configures a firewall that cannot be applied, so it will not
     * launch. Distinct from [`Self::FirewallUnenforceable`], which is the
     * host-wide cause: this one names an activity the child has lost.
     */
    @SerialName("firewall_not_applied") FIREWALL_NOT_APPLIED,
    /**
     * shepherd cannot talk to the compositor, so it cannot see what is on
     * screen. The escape sweep closes nothing and no orphaned window is
     * reported, which is indistinguishable from a clear screen unless it is
     * said out loud (issue #147).
     */
    @SerialName("compositor_unreachable") COMPOSITOR_UNREACHABLE,
    /**
     * The compositor's IPC socket is still reachable by every process at this
     * uid, because hardening it failed (issue #144).
     *
     * The session is deliberately left running — an unhardened kiosk beats no
     * kiosk — so nothing else about the device looks wrong. Without this the
     * only trace is one log line, and a device ships without a protection it
     * is configured to have.
     */
    @SerialName("compositor_not_hardened") COMPOSITOR_NOT_HARDENED,
    /**
     * shepherdd's own management socket is reachable by processes that are
     * not part of the session — the peer allow-list is not armed, or it is
     * armed somewhere it cannot mean anything (issue #144).
     *
     * Like [`Self::CompositorNotHardened`], the session is deliberately left
     * running, so nothing else about the device looks wrong and the downgrade
     * is invisible unless it is said out loud.
     */
    @SerialName("ipc_socket_not_hardened") IPC_SOCKET_NOT_HARDENED,
    /**
     * Something at this uid tried to drive the daemon from outside the
     * session and was refused (issue #144). Worth an administrator's
     * attention: an activity probing the management socket is not something
     * that happens by accident.
     */
    @SerialName("ipc_peer_rejected") IPC_PEER_REJECTED,
    /**
     * This entry sets a browser policy that its kind does not support, so the
     * policy is ignored.
     */
    @SerialName("browser_policy_ignored") BROWSER_POLICY_IGNORED,
    /**
     * A media activity references YouTube but `yt-dlp` is not installed.
     */
    @SerialName("yt_dlp_missing") YT_DLP_MISSING,
    /**
     * Free space on the media cache volume is below the configured floor, so
     * prefetch has stopped.
     */
    @SerialName("media_cache_disk_low") MEDIA_CACHE_DISK_LOW,
    /**
     * A media library could not be read or parsed.
     */
    @SerialName("media_library_unreadable") MEDIA_LIBRARY_UNREADABLE,
    /**
     * No sound backend was detected; volume control does nothing.
     */
    @SerialName("no_sound_backend") NO_SOUND_BACKEND,
    /**
     * The sound backend is present but its device topology could not be read,
     * so which output is selected and which are plugged in are both unknown.
     * Distinct from [`Self::NoSoundBackend`]: there *is* a backend, and the
     * per-output volume limits are running on the last state seen rather than
     * on what is true now.
     */
    @SerialName("audio_topology_unreadable") AUDIO_TOPOLOGY_UNREADABLE,
    /**
     * No readable input devices, so input-gated entries cannot be evaluated.
     */
    @SerialName("input_devices_unavailable") INPUT_DEVICES_UNAVAILABLE,
    /**
     * The BlueZ pairing agent could not be registered; a new phone will not be
     * shown a pairing code.
     */
    @SerialName("ble_pairing_agent_unavailable") BLE_PAIRING_AGENT_UNAVAILABLE,
    /**
     * A RetroArch entry names a libretro core that is not installed, so the
     * activity will not launch.
     */
    @SerialName("retroarch_core_missing") RETROARCH_CORE_MISSING,
    /**
     * A RetroArch entry's content — its ROM or disc image — is not there, so
     * the activity will not launch.
     */
    @SerialName("retroarch_content_missing") RETROARCH_CONTENT_MISSING,
}

/**
 * The current set, as clients see it.
 *
 * A struct rather than a bare `Vec` so the cap can report itself: a client
 * showing 32 of 40 problems while implying it is showing all of them would be
 * worse than showing none.
 */
@Serializable
data class DiagnosticSet(
    /**
     * Sorted most severe first, then by subject, then by code — a stable
     * order, so a client diffing two snapshots sees real changes rather than
     * reordering.
     */
    val items: List<Diagnostic>,
    /**
     * Whether [`MAX_DIAGNOSTICS`] hid anything. UIs must say so.
     */
    val truncated: Boolean,
)

/**
 * How bad it is. Declaration order is the sort order: `Critical` first.
 */
@Serializable
enum class DiagnosticSeverity {
    /**
     * The configuration claims a protection the device is not providing.
     * Unmissable in both UIs.
     */
    @SerialName("critical") CRITICAL,
    /**
     * A feature is unavailable or degraded.
     */
    @SerialName("warning") WARNING,
    /**
     * Worth knowing; nothing is broken.
     */
    @SerialName("info") INFO,
}

/**
 * What a diagnostic is about.
 *
 * The split exists so a UI can render a per-entry problem on the entry itself,
 * next to the availability reasons already there, instead of only in a global
 * list.
 */
@Serializable
@JsonClassDiscriminator("type")
sealed interface DiagnosticSubject {
    /**
     * The device as a whole.
     */
    @Serializable
    @SerialName("service")
    data object Service : DiagnosticSubject

    /**
     * One configured activity.
     */
    @Serializable
    @SerialName("entry")
    data class Entry(
        val entryId: EntryId,
    ) : DiagnosticSubject

    /**
     * A [DiagnosticSubject] this build doesn't know about.
     *
     * Registered as the polymorphic default in `ShepherdWireModule`, so a
     * newer device degrades this one value instead of failing the decode of
     * everything around it.
     */
    @Serializable
    @SerialName("__unknown")
    data class Unknown(val type: String? = null) : DiagnosticSubject
}

/**
 * How the kiosk drives displays when an external monitor is docked (issue #87).
 *
 * Exactly one logical output is ever active in every variant, so the
 * one-activity-at-a-time invariant always holds.
 */
@Serializable
enum class DisplayMode {
    /**
     * Only the internal/primary panel is active — the state when no external
     * display is connected.
     */
    @SerialName("single_internal") SINGLE_INTERNAL,
    /**
     * The external display mirrors the primary. Default whenever an external
     * display connects.
     */
    @SerialName("mirror") MIRROR,
    /**
     * The primary panel is disabled and the external display drives the
     * session at its native resolution.
     */
    @SerialName("external_only") EXTERNAL_ONLY,
}

/**
 * Snapshot of the compositor's display arrangement, broadcast to shells so the
 * HUD can show/hide and label its mirror/external toggle (issue #87).
 */
@Serializable
data class DisplayState(
    val mode: DisplayMode,
    /**
     * Connector name of the primary (internal, first-enumerated) output.
     */
    val primary: String? = null,
    /**
     * Connector name of the external/secondary output, if one is connected.
     */
    val secondary: String? = null,
)

@Serializable
data class DurationSecs(
    val nanos: Long,
    val secs: Long,
)

/**
 * Unique identifier for an entry in the policy whitelist
 */
typealias EntryId = String

/**
 * Entry kind with launch details
 */
@Serializable
@JsonClassDiscriminator("type")
sealed interface EntryKind {
    @Serializable
    @SerialName("process")
    data class Process(
        /**
         * Additional command-line arguments
         */
        val args: List<String> = emptyList(),
        /**
         * Command to run (required)
         */
        val command: String,
        val cwd: String? = null,
        val env: Map<String, String> = emptyMap(),
    ) : EntryKind

    /**
     * Snap application - uses systemd scope-based process management
     */
    @Serializable
    @SerialName("snap")
    data class Snap(
        /**
         * Additional command-line arguments
         */
        val args: List<String> = emptyList(),
        /**
         * Command to run (defaults to snap_name if not specified)
         */
        val command: String? = null,
        /**
         * Additional environment variables
         */
        val env: Map<String, String> = emptyMap(),
        /**
         * The snap name (e.g., "mc-installer")
         */
        val snapName: String,
    ) : EntryKind

    /**
     * Steam game launched via the Steam snap (Linux)
     */
    @Serializable
    @SerialName("steam")
    data class Steam(
        /**
         * Steam App ID (e.g., 504230 for Celeste)
         */
        val appId: Long,
        /**
         * Additional command-line arguments passed to Steam
         */
        val args: List<String> = emptyList(),
        /**
         * Additional environment variables
         */
        val env: Map<String, String> = emptyMap(),
    ) : EntryKind

    /**
     * Flatpak application - uses systemd scope-based process management
     */
    @Serializable
    @SerialName("flatpak")
    data class Flatpak(
        /**
         * The Flatpak application ID (e.g., "org.prismlauncher.PrismLauncher")
         */
        val appId: String,
        /**
         * Additional command-line arguments
         */
        val args: List<String> = emptyList(),
        /**
         * Additional environment variables
         */
        val env: Map<String, String> = emptyMap(),
    ) : EntryKind

    @Serializable
    @SerialName("vm")
    data class Vm(
        val args: JsonElement? = null,
        val driver: String,
    ) : EntryKind

    /**
     * A `shepherd-media` library activity (issue #127).
     *
     * The fields mirror the flags `shepherd-media` accepts, so shepherdd can
     * build the invocation itself instead of an admin restating it as a
     * `Process` argv. `connectivity_check` is not among them: it is resolved
     * from the entry's `internet` policy at spawn time and reaches the host
     * adapter through `SpawnOptions`.
     */
    @Serializable
    @SerialName("media")
    data class Media(
        /**
         * The item to play. Required by (and only meaningful for)
         * [`MediaMode::Play`].
         */
        val item: String? = null,
        /**
         * Library source: a path to a `.toml`/`.m3u`/`.m3u8` file, or a
         * YouTube playlist URL. `~` is expanded for paths at spawn time.
         */
        val library: String,
        /**
         * Whether to open the poster grid or play a single item.
         */
        val mode: MediaMode? = null,
        /**
         * Whether shepherdd may prefetch this library's remote items in the
         * background. `None` inherits `service.media.prefetch`.
         */
        val prefetch: Boolean? = null,
        /**
         * Maximum video quality for playback and background downloads.
         */
        val quality: MediaQuality? = null,
        /**
         * Remember playback positions for this library across sessions.
         */
        val resume: Boolean = false,
        /**
         * Reverse the final item order. Combines with `sort_by`.
         */
        val reverse: Boolean = false,
        /**
         * Field used to order library items before display or lookup.
         */
        val sortBy: MediaSortBy? = null,
    ) : EntryKind

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
    @Serializable
    @SerialName("retroarch")
    data class Retroarch(
        /**
         * Extra arguments, appended after the ones shepherd derives.
         */
        val args: List<String> = emptyList(),
        /**
         * The RetroArch binary. Defaults to `retroarch` on `PATH`.
         */
        val command: String? = null,
        /**
         * The content (ROM / disc image) to load.
         */
        val content: String,
        /**
         * Core short name, e.g. `"mgba"` → `mgba_libretro.so`, resolved
         * against the usual libretro core directories. Mutually exclusive
         * with `core_path`.
         */
        val core: String? = null,
        /**
         * Absolute path to a `*_libretro.so`, bypassing name resolution.
         */
        val corePath: String? = null,
        val env: Map<String, String> = emptyMap(),
        /**
         * Lock RetroArch's own menu so the activity can't be used to browse
         * the filesystem or change emulator settings. On by default: this is
         * a supervised kiosk.
         */
        val kiosk: Boolean = true,
        /**
         * Offer a reset ("reboot the console") button on the HUD. On by
         * default, because `save_state = "auto"` otherwise makes the
         * console's own power-on screen unreachable — there is no way back to
         * the title screen from inside a resumed save state.
         */
        val reset: Boolean = true,
        /**
         * Whether closing the activity saves state and opening restores it.
         */
        val saveState: RetroarchSaveState? = null,
    ) : EntryKind

    @Serializable
    @SerialName("custom")
    data class Custom(
        val payload: JsonElement,
        val typeName: String,
    ) : EntryKind

    /**
     * A [EntryKind] this build doesn't know about.
     *
     * Registered as the polymorphic default in `ShepherdWireModule`, so a
     * newer device degrades this one value instead of failing the decode of
     * everything around it.
     */
    @Serializable
    @SerialName("__unknown")
    data class Unknown(val type: String? = null) : EntryKind
}

/**
 * Entry kind tag for capability matching
 */
@Serializable
enum class EntryKindTag {
    @SerialName("process") PROCESS,
    @SerialName("snap") SNAP,
    @SerialName("steam") STEAM,
    @SerialName("flatpak") FLATPAK,
    @SerialName("vm") VM,
    @SerialName("media") MEDIA,
    @SerialName("retroarch") RETROARCH,
    @SerialName("custom") CUSTOM,
}

/**
 * View of an entry for UI display
 */
@Serializable
data class EntryView(
    val enabled: Boolean,
    val entryId: EntryId,
    /**
     * The group this entry belongs to (issue #5), if any. Management UIs use
     * it to show that an activity's schedule and budget are shared.
     */
    val group: GroupId? = null,
    val iconRef: String? = null,
    val kindTag: EntryKindTag,
    val label: String,
    /**
     * Maximum run duration if started now. None means:
     * - If enabled=false: entry is not available
     * - If enabled=true: entry has no time limit (unlimited)
     */
    val maxRunIfStartedNow: DurationSecs? = null,
    val reasons: List<ReasonCode>,
    /**
     * The entry's own token gate (issue #8), if it has one. A member of a
     * token-gated group carries its own gate only; the category's is on the
     * `GroupView`.
     */
    val tokens: TokenStatus? = null,
)

/**
 * Unique identifier for a group of entries sharing a schedule and limits
 * (issue #5)
 */
typealias GroupId = String

/**
 * View of a group for UI display (issue #5).
 *
 * A group's limits are shared by its members, so a management UI needs to
 * show the *category's* state — combined usage against the combined quota,
 * and whatever is currently restricting it — separately from any one member.
 */
@Serializable
data class GroupView(
    /**
     * Effective daily quota after any override delta. None means unlimited.
     */
    val dailyQuota: DurationSecs? = null,
    /**
     * Whether the group's own restrictions currently permit its members.
     * Individual members may still be unavailable for their own reasons.
     */
    val enabled: Boolean,
    val groupId: GroupId,
    val label: String,
    /**
     * Longest session the group's limits would currently allow a member.
     * None means the group imposes no cap of its own.
     */
    val maxRunIfStartedNow: DurationSecs? = null,
    /**
     * Members, in policy order.
     */
    val memberIds: List<EntryId>,
    /**
     * Why the group is restricting its members, if it is. These are the
     * unwrapped reasons — the same ones members carry inside
     * `ReasonCode::GroupRestricted`.
     */
    val reasons: List<ReasonCode>,
    /**
     * The category's token gate (issue #8), if it has one. Shared by every
     * member, so it belongs here rather than on any one of them.
     */
    val tokens: TokenStatus? = null,
    /**
     * Combined usage across all members today.
     */
    val usedToday: DurationSecs,
)

/**
 * Health status
 */
@Serializable
data class HealthStatus(
    val hostAdapterOk: Boolean,
    val live: Boolean,
    val policyLoaded: Boolean,
    val ready: Boolean,
    val storeOk: Boolean,
)

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
@Serializable
enum class InputCompatMode {
    /**
     * Grab touchscreens and emit synthesized pointer events via
     * `zwlr_virtual_pointer_v1` for the lifetime of the activity.
     */
    @SerialName("touch_to_mouse") TOUCH_TO_MOUSE,
    /**
     * Grab absolute pointers / tablets and emit synthesized touch events for
     * activities that only handle touch input — the inverse of
     * `TouchToMouse`. Useful for developing touch support against
     * mouse/pen-only hardware, or VMs whose pointer is an absolute tablet.
     */
    @SerialName("tablet_to_touch") TABLET_TO_TOUCH,
    /**
     * Grab every touchscreen and discard its events for the lifetime of the
     * activity, effectively disabling the touchscreen. Unlike `TouchToMouse`
     * it emits nothing — useful for activities that misbehave on touch input
     * but should still be playable with a mouse or gamepad.
     */
    @SerialName("disable_touch") DISABLE_TOUCH,
    /**
     * Remap a gamepad to mouse + keyboard using the productivity preset:
     * triggers = LMB, shoulders = RMB, left stick = mouse, right stick =
     * scroll, stick-click toggles which stick drives the mouse, D-pad =
     * arrow keys, A = Enter, Start = Escape.
     */
    @SerialName("gamepad_productivity") GAMEPAD_PRODUCTIVITY,
    /**
     * Remap a gamepad to mouse + keyboard using the GPD/FPS preset:
     * LT = LMB, RT = RMB, LB = MMB, left stick = WASD, right stick = mouse,
     * D-pad = scroll, A = Space, X = R, B = E, Y = F.
     */
    @SerialName("gamepad_gpd") GAMEPAD_GPD,
}

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
@Serializable
enum class InputDeviceType {
    /**
     * A relative pointing device (mouse, trackball, trackpad).
     */
    @SerialName("mouse") MOUSE,
    /**
     * A finger touchscreen (an absolute, direct-input touch device).
     */
    @SerialName("touch") TOUCH,
    /**
     * A physical alphabetic keyboard.
     */
    @SerialName("keyboard") KEYBOARD,
    /**
     * A gamepad / game controller / joystick.
     */
    @SerialName("gamepad") GAMEPAD,
}

/**
 * Status of a single internet connectivity check target
 */
@Serializable
data class InternetStatusView(
    /**
     * Whether the last check succeeded
     */
    val available: Boolean,
    /**
     * Original check string as configured (e.g. "https://example.com")
     */
    val target: String,
)

/**
 * A known Steam "launch interstitial" — one of the blocking modals Steam can
 * show between a launch request and the game actually starting (cloud-sync
 * warnings, controller advisories, etc.). The kiosk can be configured to
 * auto-dismiss specific kinds; see `service.steam.auto_dismiss_interstitials`.
 *
 * This enum is the canonical catalog: config validates against it, and the
 * host adapter attaches the per-kind CEF detection signatures.
 */
@Serializable
enum class InterstitialKind {
    /**
     * "Unable to Sync" Steam Cloud warning shown when launching offline with
     * un-uploaded saves. Affirmative action: "Play anyway". (Verified.)
     */
    @SerialName("cloud_sync") CLOUD_SYNC,
    /**
     * "Grab a controller…" advisory for controller-recommended games launched
     * without a controller. Affirmative action: "OK". (Verified.)
     */
    @SerialName("controller_recommended") CONTROLLER_RECOMMENDED,
    /**
     * First-launch "intro to Steam Input" notice. Affirmative action: "OK".
     * (Best-effort signature.)
     */
    @SerialName("steam_input_intro") STEAM_INPUT_INTRO,
    /**
     * Game *requires* a controller. Dismissing launches a game that cannot be
     * played without one, so this is risky. (Best-effort signature.)
     */
    @SerialName("controller_required") CONTROLLER_REQUIRED,
    /**
     * Game requires a VR headset. Dismissing launches something unusable
     * without VR hardware, so this is risky. (Best-effort signature.)
     */
    @SerialName("vr_required") VR_REQUIRED,
}

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
typealias LimitSubject = String

/**
 * How a [`EntryKind::Media`] activity opens.
 */
@Serializable
enum class MediaMode {
    /**
     * Open the poster grid over the whole library; the user picks items.
     */
    @SerialName("browse") BROWSE,
    /**
     * Play a single item end to end; the grid is never shown.
     */
    @SerialName("play") PLAY,
}

/**
 * Maximum video quality for a [`EntryKind::Media`] activity.
 *
 * Mirrors `shepherd_media_app::Quality`; kept here so the wire schema and the
 * config layer don't depend on the media crates. `shepherd-media`'s `cli`
 * module holds the test that keeps the two spellings in agreement.
 */
@Serializable
enum class MediaQuality {
    /**
     * No height restriction — the best available.
     */
    @SerialName("best") BEST,
    /**
     * Up to 1080p (default).
     */
    @SerialName("1080p") Q_1080P,
    /**
     * Up to 720p.
     */
    @SerialName("720p") Q_720P,
    /**
     * Up to 480p.
     */
    @SerialName("480p") Q_480P,
}

/**
 * How a [`EntryKind::Media`] activity orders its library.
 *
 * Mirrors `shepherd-media`'s `--sort-by` values; see [`MediaQuality`] for
 * where that agreement is tested.
 */
@Serializable
enum class MediaSortBy {
    /**
     * Preserve the order from the library file or playlist (default).
     */
    @SerialName("library") LIBRARY,
    /**
     * Display title, case-insensitive.
     */
    @SerialName("title") TITLE,
    /**
     * Stable item id.
     */
    @SerialName("id") ID,
    /**
     * Item kind (audio before video).
     */
    @SerialName("kind") KIND,
    /**
     * Optional category string, case-insensitive.
     */
    @SerialName("category") CATEGORY,
    /**
     * Optional duration in seconds, ascending.
     */
    @SerialName("duration") DURATION,
}

/**
 * Structured reason codes for why an entry is unavailable
 */
@Serializable
@JsonClassDiscriminator("code")
sealed interface ReasonCode {
    /**
     * Outside allowed time window
     */
    @Serializable
    @SerialName("outside_time_window")
    data class OutsideTimeWindow(
        /**
         * When the next window opens (if known)
         */
        val nextWindowStart: IsoTimestamp? = null,
    ) : ReasonCode

    /**
     * Daily quota exhausted
     */
    @Serializable
    @SerialName("quota_exhausted")
    data class QuotaExhausted(
        val quota: DurationSecs,
        val used: DurationSecs,
    ) : ReasonCode

    /**
     * Cooldown period active
     */
    @Serializable
    @SerialName("cooldown_active")
    data class CooldownActive(
        val availableAt: IsoTimestamp,
    ) : ReasonCode

    /**
     * Another session is active
     */
    @Serializable
    @SerialName("session_active")
    data class SessionActive(
        val entryId: EntryId,
        /**
         * Time remaining in current session. None means unlimited.
         */
        val remaining: DurationSecs? = null,
    ) : ReasonCode

    /**
     * Host doesn't support this entry kind
     */
    @Serializable
    @SerialName("unsupported_kind")
    data class UnsupportedKind(
        val kind: EntryKindTag,
    ) : ReasonCode

    /**
     * The activity kind has not finished warming up yet (e.g. Steam is still
     * performing its initial load). See per-kind readiness (issue #76).
     */
    @Serializable
    @SerialName("not_ready")
    data class NotReady(
        val kind: EntryKindTag,
    ) : ReasonCode

    /**
     * Entry is explicitly disabled
     */
    @Serializable
    @SerialName("disabled")
    data class Disabled(
        val reason: String? = null,
    ) : ReasonCode

    /**
     * Internet connectivity is required but unavailable
     */
    @Serializable
    @SerialName("internet_unavailable")
    data class InternetUnavailable(
        val check: String? = null,
    ) : ReasonCode

    /**
     * Entry is manually disabled for the day via a daily override
     */
    @Serializable
    @SerialName("manually_disabled")
    data class ManuallyDisabled(
        val until: IsoDate,
    ) : ReasonCode

    /**
     * One or more required input devices (issue #96) are not currently
     * connected. `devices` lists the missing device types, sorted and
     * deduplicated.
     */
    @Serializable
    @SerialName("required_input_unavailable")
    data class RequiredInputUnavailable(
        val devices: List<InputDeviceType>,
    ) : ReasonCode

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
    @Serializable
    @SerialName("protection_unavailable")
    data object ProtectionUnavailable : ReasonCode

    /**
     * Not enough time banked on this entry's token gate (issue #8): the
     * activity has to be earned by spending time on its source activities.
     */
    @Serializable
    @SerialName("tokens_insufficient")
    data class TokensInsufficient(
        /**
         * Time currently banked toward this entry.
         */
        val balance: DurationSecs,
        /**
         * Balance needed before it unlocks. Zero means any balance above zero
         * unlocks it, i.e. the entry is simply out of banked time.
         */
        val required: DurationSecs,
    ) : ReasonCode

    /**
     * The restriction comes from the entry's group rather than the entry
     * itself (issue #5) — e.g. the whole category's daily quota is spent.
     * `label` is the group's display name, for explaining it to a caregiver.
     */
    @Serializable
    @SerialName("group_restricted")
    data class GroupRestricted(
        val group: GroupId,
        val label: String,
        val reason: ReasonCode,
    ) : ReasonCode

    /**
     * A [ReasonCode] this build doesn't know about.
     *
     * Registered as the polymorphic default in `ShepherdWireModule`, so a
     * newer device degrades this one value instead of failing the decode of
     * everything around it.
     */
    @Serializable
    @SerialName("__unknown")
    data class Unknown(val code: String? = null) : ReasonCode
}

/**
 * How a [`EntryKind::Retroarch`] activity treats its save state across
 * close and re-open.
 *
 * This is the emulator's *snapshot*, not the game's own save file. The
 * in-game save (SRAM / battery save) is flushed on a clean exit either way,
 * and periodically while playing.
 */
@Serializable
enum class RetroarchSaveState {
    /**
     * Write a save state when the activity closes and load it on the next
     * open, so the child resumes exactly where they stopped — mid-battle,
     * mid-cutscene, wherever the session ended.
     *
     * Note this makes the console's own power-on screen unreachable, which is
     * what the HUD's reset button is for.
     */
    @SerialName("auto") AUTO,
    /**
     * Leave save states alone. Every launch boots the content from scratch;
     * only the in-game save carries over.
     */
    @SerialName("off") OFF,
}

/**
 * Full service state snapshot
 */
@Serializable
data class ServiceStateSnapshot(
    val apiVersion: Long,
    val currentSession: SessionInfo? = null,
    /**
     * Administrator-facing conditions currently true of this device (issue
     * #143) — a missing dependency, a protection that is not in effect. Rides
     * the snapshot so every client has the current set on subscribe; deltas
     * arrive as `EventPayload::DiagnosticsChanged`.
     */
    val diagnostics: DiagnosticSet? = null,
    /**
     * Available entries for UI display
     */
    val entries: List<EntryView> = emptyList(),
    val entryCount: Long,
    /**
     * Latest known status of each configured internet connectivity check.
     * Empty when no connectivity checks are configured.
     */
    val internetStatus: List<InternetStatusView> = emptyList(),
    val policyLoaded: Boolean,
)

/**
 * Session end reason
 */
@Serializable
@JsonClassDiscriminator("type")
sealed interface SessionEndReason {
    /**
     * Session expired (time limit reached)
     */
    @Serializable
    @SerialName("expired")
    data object Expired : SessionEndReason

    /**
     * User requested stop
     */
    @Serializable
    @SerialName("user_stop")
    data object UserStop : SessionEndReason

    /**
     * Admin requested stop
     */
    @Serializable
    @SerialName("admin_stop")
    data object AdminStop : SessionEndReason

    /**
     * Process exited on its own
     */
    @Serializable
    @SerialName("process_exited")
    data class ProcessExited(
        val exitCode: Long? = null,
    ) : SessionEndReason

    /**
     * Policy change terminated session
     */
    @Serializable
    @SerialName("policy_stop")
    data object PolicyStop : SessionEndReason

    /**
     * Service shutdown
     */
    @Serializable
    @SerialName("service_shutdown")
    data object ServiceShutdown : SessionEndReason

    /**
     * Launch failed
     */
    @Serializable
    @SerialName("launch_failed")
    data class LaunchFailed(
        val error: String,
    ) : SessionEndReason

    /**
     * A [SessionEndReason] this build doesn't know about.
     *
     * Registered as the polymorphic default in `ShepherdWireModule`, so a
     * newer device degrades this one value instead of failing the decode of
     * everything around it.
     */
    @Serializable
    @SerialName("__unknown")
    data class Unknown(val type: String? = null) : SessionEndReason
}

/**
 * Unique identifier for a running session
 */
typealias SessionId = String

/**
 * Active session information
 */
@Serializable
data class SessionInfo(
    /**
     * Whether the HUD should offer a reset button for this session — see
     * [`EntryKind::supports_reset`]. Defaults to `false` when absent, so an
     * older payload simply doesn't show the button.
     */
    val canReset: Boolean = false,
    /**
     * Whether the HUD should confirm before its "X" button ends this
     * session (issue #78). Defaults to `true` when absent so older payloads
     * keep the safe behaviour.
     */
    val confirmOnClose: Boolean = true,
    /**
     * Session deadline. None means unlimited (no time limit).
     */
    val deadline: IsoTimestamp? = null,
    val entryId: EntryId,
    val label: String,
    val sessionId: SessionId,
    val startedAt: IsoTimestamp,
    val state: SessionState,
    /**
     * Time remaining. None means unlimited.
     */
    val timeRemaining: DurationSecs? = null,
    val warningsIssued: List<Long>,
)

/**
 * Current session state
 */
@Serializable
enum class SessionState {
    /**
     * Approved and spawning; the activity has not mapped a window yet.
     */
    @SerialName("launching") LAUNCHING,
    /**
     * The activity is running normally.
     */
    @SerialName("running") RUNNING,
    /**
     * Running, and at least one time warning has been issued.
     */
    @SerialName("warned") WARNED,
    /**
     * Past its deadline and being wound down.
     */
    @SerialName("expiring") EXPIRING,
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
    @SerialName("stopping") STOPPING,
    /**
     * Settled and cleared; no activity is running.
     */
    @SerialName("ended") ENDED,
}

/**
 * Stop mode for session termination
 */
@Serializable
enum class StopMode {
    /**
     * Try graceful termination first
     */
    @SerialName("graceful") GRACEFUL,
    /**
     * Force immediate termination
     */
    @SerialName("force") FORCE,
}

/**
 * A token gate's current state, for caregiver UIs (issue #8).
 *
 * Banked time is a currency: source activities earn it and the gated activity
 * spends it. Without this a management UI can only report that something is
 * locked, never how close it is to unlocking, and a manual grant would be
 * made blind.
 */
@Serializable
data class TokenStatus(
    /**
     * Time banked and not yet spent.
     */
    val balance: DurationSecs,
    /**
     * Whether the balance survives local midnight.
     */
    val carryOver: Boolean,
    /**
     * Ceiling on the balance. None means unlimited. A grant past this is
     * clawed back, so a UI should say so rather than let it vanish.
     */
    val maxBalance: DurationSecs? = null,
    /**
     * Balance needed to open the gate. Zero means any balance opens it.
     */
    val minimum: DurationSecs,
    /**
     * Whether the gate is open right now. Not simply `balance >= minimum`:
     * once opened it stays open until the balance is spent to zero.
     */
    val unlocked: Boolean,
)

/**
 * Screen-time usage for a single entry on a single day
 */
@Serializable
data class UsageStat(
    val date: IsoDate,
    val durationSeconds: Long,
    val entryId: EntryId,
    val label: String,
)

/**
 * Volume status information
 */
@Serializable
data class VolumeInfo(
    /**
     * Whether volume control is available
     */
    val available: Boolean,
    /**
     * The detected sound backend (e.g., "pipewire", "pulseaudio", "alsa")
     */
    val backend: String? = null,
    /**
     * Whether audio is muted
     */
    val muted: Boolean,
    /**
     * The output this reading applies to. `None` on hosts without PipeWire, or
     * when the default sink cannot be resolved to a known output.
     */
    val output: AudioOutput? = null,
    /**
     * Volume percentage (0-100)
     */
    val percent: Long,
    /**
     * Current restrictions on volume
     */
    val restrictions: VolumeRestrictions,
)

/**
 * Volume restrictions that are currently in effect
 */
@Serializable
data class VolumeRestrictions(
    /**
     * Whether volume changes are allowed at all
     */
    val allowChange: Boolean,
    /**
     * Whether mute toggle is allowed
     */
    val allowMute: Boolean,
    /**
     * Maximum volume percentage allowed
     */
    val maxVolume: Long? = null,
    /**
     * Minimum volume percentage allowed
     */
    val minVolume: Long? = null,
)

/**
 * Warning severity level
 */
@Serializable
enum class WarningSeverity {
    @SerialName("info") INFO,
    @SerialName("warn") WARN,
    @SerialName("critical") CRITICAL,
}

/**
 * Warning threshold configuration
 */
@Serializable
data class WarningThreshold(
    val messageTemplate: String? = null,
    /**
     * Seconds before expiry to issue this warning
     */
    val secondsBefore: Long,
    val severity: WarningSeverity,
)

/**
 * An action that can be performed on a window via the debug API.
 */
@Serializable
enum class WindowAction {
    /**
     * Ask the window to close (sway `kill`).
     */
    @SerialName("close") CLOSE,
    /**
     * Move the window to the scratchpad to hide it from view.
     */
    @SerialName("hide") HIDE,
    /**
     * Pull the window out of the scratchpad so it is shown again.
     */
    @SerialName("show") SHOW,
}

/**
 * Debug snapshot of a single window known to the host's compositor.
 *
 * Currently surfaced via the management API for debugging the Sway tree —
 * in particular, to see which windows have been moved to the scratchpad
 * (e.g. the hidden Steam client) versus which are on-screen.
 */
@Serializable
data class WindowInfo(
    /**
     * Wayland app_id, if available.
     */
    val appId: String? = null,
    /**
     * True if the window has keyboard focus.
     */
    val focused: Boolean,
    /**
     * Compositor-assigned window/container id.
     */
    val id: Long,
    /**
     * True if the window currently lives on the scratchpad (hidden).
     */
    val inScratchpad: Boolean,
    /**
     * Window title, if the application set one.
     */
    val name: String? = null,
    /**
     * What shepherd is supervising behind this window, if anything.
     *
     * A host that cannot attribute windows reports every one of them as
     * unowned.
     */
    val owner: WindowOwner,
    /**
     * Owning process id, if reported by the compositor.
     */
    val pid: Long? = null,
    /**
     * True if the window is currently being rendered.
     */
    val visible: Boolean,
    /**
     * X11 class (xwayland windows), if available.
     */
    val windowClass: String? = null,
    /**
     * Workspace name the window belongs to, if any. `__i3_scratch` is the
     * scratchpad pseudo-workspace.
     */
    val workspace: String? = null,
)

/**
 * Who shepherd believes a window belongs to.
 *
 * The compositor cannot answer this — it reports pids, not intent. The host
 * fills it in by matching each window against what it is actually
 * supervising, which is what lets an admin UI tell "the game the child is
 * playing" apart from "something on the screen that no session owns".
 */
@Serializable
enum class WindowOwner {
    /**
     * Shepherd's own furniture: the launcher, the HUD, the pairing UI, the
     * mirror, and background processes it keeps warm (the preloaded Steam
     * client). Expected to outlive every session.
     */
    @SerialName("shepherd") SHEPHERD,
    /**
     * A process shepherd is supervising for the current session — the
     * activity itself, something in its process group, a Steam game
     * launched on its behalf, or one of its input sidecars.
     */
    @SerialName("activity") ACTIVITY,
    /**
     * An activity that outlived its own teardown. Its session is over and
     * the host is still working on killing it — the same condition that
     * writes an `ActivityEscaped` audit record.
     */
    @SerialName("escaped") ESCAPED,
    /**
     * No process shepherd knows about. Either something started outside
     * shepherd entirely, or an activity that got away without the host ever
     * noticing — the case supervision cannot fix on its own, and the reason
     * this field exists.
     */
    @SerialName("unowned") UNOWNED,
}

/**
 * Polymorphic defaults for every tagged enum above.
 *
 * Installed on `ShepherdJson`; without it an unrecognised discriminator
 * throws and fails the decode of the entire enclosing response.
 */
val ShepherdWireModule: SerializersModule = SerializersModule {
    polymorphic(DiagnosticSubject::class) { defaultDeserializer { DiagnosticSubject.Unknown.serializer() } }
    polymorphic(EntryKind::class) { defaultDeserializer { EntryKind.Unknown.serializer() } }
    polymorphic(ReasonCode::class) { defaultDeserializer { ReasonCode.Unknown.serializer() } }
    polymorphic(SessionEndReason::class) { defaultDeserializer { SessionEndReason.Unknown.serializer() } }
}
