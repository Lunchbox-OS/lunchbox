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

    @Serializable
    @SerialName("media")
    data class Media(
        val args: JsonElement? = null,
        val libraryId: String,
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
 * Full service state snapshot
 */
@Serializable
data class ServiceStateSnapshot(
    val apiVersion: Long,
    val currentSession: SessionInfo? = null,
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
    @SerialName("launching") LAUNCHING,
    @SerialName("running") RUNNING,
    @SerialName("warned") WARNED,
    @SerialName("expiring") EXPIRING,
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
 * Polymorphic defaults for every tagged enum above.
 *
 * Installed on `ShepherdJson`; without it an unrecognised discriminator
 * throws and fails the decode of the entire enclosing response.
 */
val ShepherdWireModule: SerializersModule = SerializersModule {
    polymorphic(EntryKind::class) { defaultDeserializer { EntryKind.Unknown.serializer() } }
    polymorphic(ReasonCode::class) { defaultDeserializer { ReasonCode.Unknown.serializer() } }
    polymorphic(SessionEndReason::class) { defaultDeserializer { SessionEndReason.Unknown.serializer() } }
}
