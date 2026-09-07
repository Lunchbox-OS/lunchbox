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
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.JsonClassDiscriminator
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.modules.SerializersModule
import kotlinx.serialization.modules.polymorphic

/** ISO-8601 timestamp with offset, e.g. "2026-06-21T18:05:00-04:00". */
typealias IsoTimestamp = String

/** ISO-8601 local date, e.g. "2026-06-21". */
typealias IsoDate = String

/**
 * Which family an address belongs to. Kept explicit rather than sniffed from
 * the string, so a UI grouping v4 above v6 does not have to count colons.
 */
@Serializable(with = AddressFamily.Serializer::class)
enum class AddressFamily(val wire: String) {
    V4("v4"),
    V6("v6"),
    /**
     * A [AddressFamily] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<AddressFamily> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("AddressFamily", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: AddressFamily) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): AddressFamily {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

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

@Serializable(with = AdminRole.Serializer::class)
enum class AdminRole(val wire: String) {
    ADMIN("admin"),
    /**
     * A [AdminRole] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<AdminRole> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("AdminRole", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: AdminRole) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): AdminRole {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = AudioOutputKind.Serializer::class)
enum class AudioOutputKind(val wire: String) {
    SPEAKERS("speakers"),
    HEADPHONES("headphones"),
    HDMI("hdmi"),
    DIGITAL("digital"),
    LINE_OUT("line_out"),
    BLUETOOTH("bluetooth"),
    UNKNOWN("unknown");

    internal object Serializer : KSerializer<AudioOutputKind> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("AudioOutputKind", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: AudioOutputKind) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): AudioOutputKind {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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

@Serializable(with = ClaimStateTag.Serializer::class)
enum class ClaimStateTag(val wire: String) {
    UNCLAIMED("unclaimed"),
    CLAIMED("claimed"),
    /**
     * A [ClaimStateTag] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<ClaimStateTag> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("ClaimStateTag", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: ClaimStateTag) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): ClaimStateTag {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * How much of the internet the host believes it can reach.
 *
 * Mirrors NetworkManager's connectivity states, which are the only ones any
 * backend here can distinguish. A host with no NetworkManager reports
 * [`Connectivity::Unknown`] — which is honest, and different from
 * [`Connectivity::None`].
 */
@Serializable(with = Connectivity.Serializer::class)
enum class Connectivity(val wire: String) {
    /**
     * Nobody could say. Not a claim that the device is offline.
     */
    UNKNOWN("unknown"),
    /**
     * No route to anywhere.
     */
    NONE("none"),
    /**
     * A captive portal is intercepting traffic. Worth its own state: the
     * device looks connected and nothing works, which is the single most
     * confusing failure to debug remotely.
     */
    PORTAL("portal"),
    /**
     * A route exists but the connectivity probe did not complete.
     */
    LIMITED("limited"),
    /**
     * The host reached the internet.
     */
    FULL("full");

    internal object Serializer : KSerializer<Connectivity> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("Connectivity", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: Connectivity) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): Connectivity {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = DiagnosticCode.Serializer::class)
enum class DiagnosticCode(val wire: String) {
    /**
     * Per-entry firewall enforcement is unavailable on this host — the helper
     * is not installed, or polkit denies it.
     */
    FIREWALL_UNENFORCEABLE("firewall_unenforceable"),
    /**
     * This entry configures a firewall that cannot be applied, so it will not
     * launch. Distinct from [`Self::FirewallUnenforceable`], which is the
     * host-wide cause: this one names an activity the child has lost.
     */
    FIREWALL_NOT_APPLIED("firewall_not_applied"),
    /**
     * shepherd cannot talk to the compositor, so it cannot see what is on
     * screen. The escape sweep closes nothing and no orphaned window is
     * reported, which is indistinguishable from a clear screen unless it is
     * said out loud (issue #147).
     */
    COMPOSITOR_UNREACHABLE("compositor_unreachable"),
    /**
     * The compositor's IPC socket is still reachable by every process at this
     * uid, because hardening it failed (issue #144).
     *
     * The session is deliberately left running — an unhardened kiosk beats no
     * kiosk — so nothing else about the device looks wrong. Without this the
     * only trace is one log line, and a device ships without a protection it
     * is configured to have.
     */
    COMPOSITOR_NOT_HARDENED("compositor_not_hardened"),
    /**
     * Something replaced or removed shepherdd's management socket, so the
     * daemon is no longer reachable at the path its clients use (issue #144).
     *
     * An activity can do this: the socket lives in a directory owned by the
     * uid every activity runs as, and no file mode prevents it — a root-owned
     * directory stops shepherdd binding at all, and the sticky bit restricts
     * deletion to the file's owner, which an activity is. Clients refuse to
     * talk to whatever bound the name instead, so this is a denial rather than
     * a breach; without saying so, it looks like a launcher that stopped
     * working for no reason.
     */
    IPC_SOCKET_REPLACED("ipc_socket_replaced"),
    /**
     * shepherdd's own management socket is reachable by processes that are
     * not part of the session — the peer allow-list is not armed, or it is
     * armed somewhere it cannot mean anything (issue #144).
     *
     * Like [`Self::CompositorNotHardened`], the session is deliberately left
     * running, so nothing else about the device looks wrong and the downgrade
     * is invisible unless it is said out loud.
     */
    IPC_SOCKET_NOT_HARDENED("ipc_socket_not_hardened"),
    /**
     * shepherd's policy and state are files at the uid activities run as,
     * because this device has no state custodian (issue #157).
     *
     * The session is deliberately left running — an unprotected kiosk beats a
     * child staring at a dead screen — so, like
     * [`Self::IpcSocketNotHardened`], nothing else about the device looks
     * wrong and the downgrade is invisible unless it is said out loud.
     *
     * Only for a device that never had one: a packaged install where
     * `shepherd-admin setup-user` has not run, or one deliberately left
     * without. A device whose custodian *is* installed and unreachable does
     * not reach this — it refuses to start, because its state has moved and
     * running anyway would mean an empty database and a launcher with no
     * activities, which looks like a quiet evening rather than a fault.
     *
     * Raised only at startup. A device that fell back mid-session would be a
     * device an activity could *push* into falling back, which is the one
     * thing this must not be.
     */
    STATE_NOT_PROTECTED("state_not_protected"),
    /**
     * Nothing outside the session would notice this daemon being killed, so an
     * activity can leave the session running with nothing supervising it
     * (issue #172).
     *
     * Every activity runs as shepherdd's own uid, and signal permission is a
     * uid comparison — so `kill`, or `SIGSTOP`, is available to anything the
     * device is supervising. The answer is the state custodian, which is
     * outside the session at a uid nothing in it can signal: it watches a
     * connection shepherdd holds and ends the session when the feeding stops.
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
    SESSION_NOT_GUARDED("session_not_guarded"),
    /**
     * Something at this uid tried to drive the daemon from outside the
     * session and was refused (issue #144). Worth an administrator's
     * attention: an activity probing the management socket is not something
     * that happens by accident.
     */
    IPC_PEER_REJECTED("ipc_peer_rejected"),
    /**
     * This entry sets a browser policy that its kind does not support, so the
     * policy is ignored.
     */
    BROWSER_POLICY_IGNORED("browser_policy_ignored"),
    /**
     * A media activity references YouTube but `yt-dlp` is not installed.
     */
    YT_DLP_MISSING("yt_dlp_missing"),
    /**
     * Free space on the media cache volume is below the configured floor, so
     * prefetch has stopped.
     */
    MEDIA_CACHE_DISK_LOW("media_cache_disk_low"),
    /**
     * A media library could not be read or parsed.
     */
    MEDIA_LIBRARY_UNREADABLE("media_library_unreadable"),
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
    MEDIA_REFRESH_FAILED("media_refresh_failed"),
    /**
     * No sound backend was detected; volume control does nothing.
     */
    NO_SOUND_BACKEND("no_sound_backend"),
    /**
     * The sound backend is present but its device topology could not be read,
     * so which output is selected and which are plugged in are both unknown.
     * Distinct from [`Self::NoSoundBackend`]: there *is* a backend, and the
     * per-output volume limits are running on the last state seen rather than
     * on what is true now.
     */
    AUDIO_TOPOLOGY_UNREADABLE("audio_topology_unreadable"),
    /**
     * No readable input devices, so input-gated entries cannot be evaluated.
     */
    INPUT_DEVICES_UNAVAILABLE("input_devices_unavailable"),
    /**
     * The BlueZ pairing agent could not be registered; a new phone will not be
     * shown a pairing code.
     */
    BLE_PAIRING_AGENT_UNAVAILABLE("ble_pairing_agent_unavailable"),
    /**
     * A RetroArch entry names a libretro core that is not installed, so the
     * activity will not launch.
     */
    RETROARCH_CORE_MISSING("retroarch_core_missing"),
    /**
     * A RetroArch entry's content — its ROM or disc image — is not there, so
     * the activity will not launch.
     */
    RETROARCH_CONTENT_MISSING("retroarch_content_missing"),
    /**
     * An ebook entry's book is not there, so the activity opens on an error
     * instead of a page.
     */
    EBOOK_BOOK_MISSING("ebook_book_missing"),
    /**
     * An ebook entry's reader, or the backend for that book's format, is not
     * installed. On Ubuntu the EPUB backend ships separately from Okular, so
     * this is the likely first-run failure.
     */
    EBOOK_READER_MISSING("ebook_reader_missing"),
    /**
     * An ebook entry lays the book out in pages on a device that has no way
     * to turn one: a touchscreen and nothing else. Reading would stop at the
     * end of the first page.
     */
    EBOOK_NO_PAGE_TURN("ebook_no_page_turn"),
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
    MANAGEMENT_API_UNAVAILABLE("management_api_unavailable"),
    /**
     * A [DiagnosticCode] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<DiagnosticCode> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("DiagnosticCode", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: DiagnosticCode) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): DiagnosticCode {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = DiagnosticSeverity.Serializer::class)
enum class DiagnosticSeverity(val wire: String) {
    /**
     * The configuration claims a protection the device is not providing.
     * Unmissable in both UIs.
     */
    CRITICAL("critical"),
    /**
     * A feature is unavailable or degraded.
     */
    WARNING("warning"),
    /**
     * Worth knowing; nothing is broken.
     */
    INFO("info"),
    /**
     * A [DiagnosticSeverity] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<DiagnosticSeverity> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("DiagnosticSeverity", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: DiagnosticSeverity) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): DiagnosticSeverity {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = DisplayMode.Serializer::class)
enum class DisplayMode(val wire: String) {
    /**
     * Only the internal/primary panel is active — the state when no external
     * display is connected.
     */
    SINGLE_INTERNAL("single_internal"),
    /**
     * The external display mirrors the primary. Default whenever an external
     * display connects.
     */
    MIRROR("mirror"),
    /**
     * The primary panel is disabled and the external display drives the
     * session at its native resolution.
     */
    EXTERNAL_ONLY("external_only"),
    /**
     * A [DisplayMode] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<DisplayMode> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("DisplayMode", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: DisplayMode) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): DisplayMode {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
 * How an [`EntryKind::Ebook`] activity lays pages out.
 */
@Serializable(with = EbookLayout.Serializer::class)
enum class EbookLayout(val wire: String) {
    /**
     * Two pages side by side, like an open book. Fits a landscape panel: a
     * single portrait page fitted to 16:9 is letterboxed and small.
     */
    FACING("facing"),
    /**
     * The same, with the first page alone — so the spreads fall where a
     * printed book's would, cover on its own and chapter openings on the
     * right. The default: it costs nothing over `facing` and matches what a
     * child holding a paper book expects.
     */
    FACING_FIRST_CENTERED("facing_first_centered"),
    /**
     * One page at a time. The right choice on a portrait screen.
     */
    SINGLE("single"),
    /**
     * One continuous column, scrolled rather than paged, fitted to the width.
     *
     * The only layout a **touch-only** device can navigate: dragging scrolls
     * it. The paged layouts turn the page on a key, a gamepad D-pad or a
     * scroll wheel, and a touchscreen produces none of those — Okular grabs
     * only the pinch gesture, and has no swipe-to-turn anywhere in its
     * desktop view.
     */
    SCROLL("scroll"),
    /**
     * A [EbookLayout] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<EbookLayout> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("EbookLayout", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: EbookLayout) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): EbookLayout {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * Which reader an [`EntryKind::Ebook`] activity drives.
 *
 * Open rather than closed on purpose: the config surface here — a book and a
 * place in it — is reader-agnostic, even though only one reader is wired up.
 */
@Serializable(with = EbookViewer.Serializer::class)
enum class EbookViewer(val wire: String) {
    /**
     * Okular (`okular`), with `okular-extra-backends` for EPUB. Covers EPUB,
     * PDF, CBZ, DjVu and FictionBook, and is the only reader in Ubuntu with a
     * documented way to disable its own escape hatches.
     */
    OKULAR("okular"),
    /**
     * A [EbookViewer] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<EbookViewer> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("EbookViewer", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: EbookViewer) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): EbookViewer {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

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
        /**
         * Whether to skip SponsorBlock segments in this library. `None`
         * inherits `service.media.sponsorblock.enabled`.
         */
        val sponsorblock: Boolean? = null,
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

    /**
     * One book, opened in a document reader locked down to reading it
     * (issue #160).
     *
     * The reader keeps the page: shepherd's job is to hand it a private
     * configuration that closes every door out of the book, and to close the
     * window politely at the end of the session so the position is written.
     * See [`shepherd_host_linux::ebook`] for what is generated.
     */
    @Serializable
    @SerialName("ebook")
    data class Ebook(
        /**
         * Extra arguments, appended after the ones shepherd derives.
         */
        val args: List<String> = emptyList(),
        /**
         * The book. Absolute, or `~/`-prefixed; expanded at launch.
         */
        val book: String,
        /**
         * The reader binary. Defaults to the viewer's usual name.
         */
        val command: String? = null,
        val env: Map<String, String> = emptyMap(),
        /**
         * Font family for the same. Must be installed on the device.
         */
        val fontFamily: String? = null,
        /**
         * Point size for the reflowed text of an EPUB. Changing it
         * repaginates the book, which moves a remembered position, so pick it
         * before the book is first opened.
         */
        val fontSize: Long = 16L,
        /**
         * Lock the reader down: no file dialog, no printing, no settings, no
         * menubar or toolbar. On by default — this is a supervised kiosk, and
         * off is only for an admin checking what the reader looks like
         * unrestricted.
         */
        val kiosk: Boolean = true,
        /**
         * How pages are laid out. `facing` (the default) suits a landscape
         * panel; `single` a portrait one.
         */
        val layout: EbookLayout? = null,
        /**
         * Page to open on the *first* launch, 1-based. Ignored once the
         * reader has a remembered position for this book.
         */
        val openAt: Long? = null,
        /**
         * Which reader to drive. Only `okular` is implemented.
         */
        val viewer: EbookViewer? = null,
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
@Serializable(with = EntryKindTag.Serializer::class)
enum class EntryKindTag(val wire: String) {
    PROCESS("process"),
    SNAP("snap"),
    STEAM("steam"),
    FLATPAK("flatpak"),
    VM("vm"),
    MEDIA("media"),
    RETROARCH("retroarch"),
    EBOOK("ebook"),
    CUSTOM("custom"),
    /**
     * A [EntryKindTag] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<EntryKindTag> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("EntryKindTag", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: EntryKindTag) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): EntryKindTag {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = HudOrientation.Serializer::class)
enum class HudOrientation(val wire: String) {
    /**
     * A horizontal bar along the top edge. The default, and what every device
     * shipped before issue #171 uses.
     */
    TOP("top"),
    /**
     * A horizontal bar along the bottom edge.
     */
    BOTTOM("bottom"),
    /**
     * A vertical bar down the left edge.
     */
    LEFT("left"),
    /**
     * A [HudOrientation] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<HudOrientation> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("HudOrientation", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: HudOrientation) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): HudOrientation {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = InputCompatMode.Serializer::class)
enum class InputCompatMode(val wire: String) {
    /**
     * Grab touchscreens and emit synthesized pointer events via
     * `zwlr_virtual_pointer_v1` for the lifetime of the activity.
     */
    TOUCH_TO_MOUSE("touch_to_mouse"),
    /**
     * Grab absolute pointers / tablets and emit synthesized touch events for
     * activities that only handle touch input — the inverse of
     * `TouchToMouse`. Useful for developing touch support against
     * mouse/pen-only hardware, or VMs whose pointer is an absolute tablet.
     */
    TABLET_TO_TOUCH("tablet_to_touch"),
    /**
     * Grab every touchscreen and discard its events for the lifetime of the
     * activity, effectively disabling the touchscreen. Unlike `TouchToMouse`
     * it emits nothing — useful for activities that misbehave on touch input
     * but should still be playable with a mouse or gamepad.
     */
    DISABLE_TOUCH("disable_touch"),
    /**
     * Remap a gamepad to mouse + keyboard using the productivity preset:
     * triggers = LMB, shoulders = RMB, left stick = mouse, right stick =
     * scroll, stick-click toggles which stick drives the mouse, D-pad =
     * arrow keys, A = Enter, Start = Escape.
     */
    GAMEPAD_PRODUCTIVITY("gamepad_productivity"),
    /**
     * Remap a gamepad to mouse + keyboard using the GPD/FPS preset:
     * LT = LMB, RT = RMB, LB = MMB, left stick = WASD, right stick = mouse,
     * D-pad = scroll, A = Space, X = R, B = E, Y = F.
     */
    GAMEPAD_GPD("gamepad_gpd"),
    /**
     * A [InputCompatMode] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<InputCompatMode> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("InputCompatMode", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: InputCompatMode) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): InputCompatMode {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = InputDeviceType.Serializer::class)
enum class InputDeviceType(val wire: String) {
    /**
     * A relative pointing device (mouse, trackball, trackpad).
     */
    MOUSE("mouse"),
    /**
     * A finger touchscreen (an absolute, direct-input touch device).
     */
    TOUCH("touch"),
    /**
     * A physical alphabetic keyboard.
     */
    KEYBOARD("keyboard"),
    /**
     * A gamepad / game controller / joystick.
     */
    GAMEPAD("gamepad"),
    /**
     * A [InputDeviceType] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<InputDeviceType> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("InputDeviceType", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: InputDeviceType) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): InputDeviceType {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = InterstitialKind.Serializer::class)
enum class InterstitialKind(val wire: String) {
    /**
     * "Unable to Sync" Steam Cloud warning shown when launching offline with
     * un-uploaded saves. Affirmative action: "Play anyway". (Verified.)
     */
    CLOUD_SYNC("cloud_sync"),
    /**
     * "Grab a controller…" advisory for controller-recommended games launched
     * without a controller. Affirmative action: "OK". (Verified.)
     */
    CONTROLLER_RECOMMENDED("controller_recommended"),
    /**
     * First-launch "intro to Steam Input" notice. Affirmative action: "OK".
     * (Best-effort signature.)
     */
    STEAM_INPUT_INTRO("steam_input_intro"),
    /**
     * Game *requires* a controller. Dismissing launches a game that cannot be
     * played without one, so this is risky. (Best-effort signature.)
     */
    CONTROLLER_REQUIRED("controller_required"),
    /**
     * Game requires a VR headset. Dismissing launches something unusable
     * without VR hardware, so this is risky. (Best-effort signature.)
     */
    VR_REQUIRED("vr_required"),
    /**
     * A [InterstitialKind] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<InterstitialKind> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("InterstitialKind", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: InterstitialKind) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): InterstitialKind {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
 * A login waiting on a tap in the companion app.
 */
@Serializable
data class LoginRequestInfo(
    /**
     * The six digits the browser is displaying. The parent compares.
     */
    val code: String,
    val expiresAt: IsoTimestamp,
    /**
     * The request's public handle — what `approve_login_request` takes.
     *
     * Not the same string the browser polls with. The browser's id is a
     * secret capability; this is a short opaque handle derived from it, so
     * that listing pending requests over BLE does not hand out the ability to
     * collect the resulting session.
     */
    val id: String,
    /**
     * Who is asking, as best the device can tell: "Chrome on Android".
     */
    val label: String,
    /**
     * The address the request came from.
     */
    val peer: String,
    val requestedAt: IsoTimestamp,
)

/**
 * How a [`EntryKind::Media`] activity opens.
 */
@Serializable(with = MediaMode.Serializer::class)
enum class MediaMode(val wire: String) {
    /**
     * Open the poster grid over the whole library; the user picks items.
     */
    BROWSE("browse"),
    /**
     * Play a single item end to end; the grid is never shown.
     */
    PLAY("play"),
    /**
     * A [MediaMode] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<MediaMode> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("MediaMode", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: MediaMode) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): MediaMode {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * Maximum video quality for a [`EntryKind::Media`] activity.
 *
 * Mirrors `shepherd_media_app::Quality`; kept here so the wire schema and the
 * config layer don't depend on the media crates. `shepherd-media`'s `cli`
 * module holds the test that keeps the two spellings in agreement.
 */
@Serializable(with = MediaQuality.Serializer::class)
enum class MediaQuality(val wire: String) {
    /**
     * No height restriction — the best available.
     */
    BEST("best"),
    /**
     * Up to 1080p (default).
     */
    Q_1080P("1080p"),
    /**
     * Up to 720p.
     */
    Q_720P("720p"),
    /**
     * Up to 480p.
     */
    Q_480P("480p"),
    /**
     * A [MediaQuality] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<MediaQuality> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("MediaQuality", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: MediaQuality) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): MediaQuality {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * How a [`EntryKind::Media`] activity orders its library.
 *
 * Mirrors `shepherd-media`'s `--sort-by` values; see [`MediaQuality`] for
 * where that agreement is tested.
 */
@Serializable(with = MediaSortBy.Serializer::class)
enum class MediaSortBy(val wire: String) {
    /**
     * Preserve the order from the library file or playlist (default).
     */
    LIBRARY("library"),
    /**
     * Display title, case-insensitive.
     */
    TITLE("title"),
    /**
     * Stable item id.
     */
    ID("id"),
    /**
     * Item kind (audio before video).
     */
    KIND("kind"),
    /**
     * Optional category string, case-insensitive.
     */
    CATEGORY("category"),
    /**
     * Optional duration in seconds, ascending.
     */
    DURATION("duration"),
    /**
     * A [MediaSortBy] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<MediaSortBy> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("MediaSortBy", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: MediaSortBy) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): MediaSortBy {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * One address on one interface.
 *
 * Address and prefix are separate fields rather than one CIDR string because
 * the address alone is what gets copied into an SSH command, and a UI should
 * not have to split on `/` to offer that.
 */
@Serializable
data class NetworkAddressView(
    /**
     * The address on its own, e.g. `192.168.0.139`.
     */
    val address: String,
    val family: AddressFamily,
    /**
     * Prefix length in bits, e.g. `24`.
     */
    val prefix: Long,
)

/**
 * What kind of interface this is. Advisory: it drives presentation and which
 * addresses are offered as ways in, never policy.
 */
@Serializable(with = NetworkInterfaceKind.Serializer::class)
enum class NetworkInterfaceKind(val wire: String) {
    /**
     * A wireless interface. The one with a network name a person recognises.
     */
    WIFI("wifi"),
    /**
     * A wired interface.
     */
    ETHERNET("ethernet"),
    /**
     * A tunnel — WireGuard, ZeroTier, OpenVPN. Reachable, and on a device
     * administered remotely often the *only* thing reachable, which is why
     * `service.management_api.bind_retry_seconds` exists at all.
     */
    VPN("vpn"),
    /**
     * A container or VM bridge (`lxcbr0`, `docker0`). Has an address; that
     * address is not a way in from the parent's phone.
     */
    BRIDGE("bridge"),
    /**
     * The host talking to itself. Never a way in.
     */
    LOOPBACK("loopback"),
    /**
     * Something we could not name. Reported as a possible way in: being wrong
     * about a veth costs a line in a list, while being wrong about a real
     * interface costs the address somebody needed.
     */
    OTHER("other"),
    /**
     * A [NetworkInterfaceKind] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<NetworkInterfaceKind> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("NetworkInterfaceKind", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: NetworkInterfaceKind) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): NetworkInterfaceKind {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * One network interface as an administrator sees it.
 */
@Serializable
data class NetworkInterfaceView(
    val addresses: List<NetworkAddressView>,
    /**
     * Nameservers configured for this interface.
     */
    val dns: List<String>,
    val gateway: String? = null,
    val kind: NetworkInterfaceKind,
    /**
     * Kernel name, e.g. `wlp3s0`.
     */
    val name: String,
    /**
     * Whether an address here is a plausible way to reach this device from
     * another machine on the same network.
     *
     * Derived, not reported by the host — see
     * [`NetworkStatusView::new`]. A UI leads with these and folds the rest
     * away: on a device running containers most interfaces are noise, and the
     * one a parent needs is the one they will not find by scrolling.
     */
    val reachable: Boolean = false,
    /**
     * Whether the interface is up and configured.
     */
    val up: Boolean,
    /**
     * Present only on a wireless interface.
     */
    val wifi: WifiView? = null,
)

/**
 * Where the status came from, so a UI can say why a field is missing rather
 * than rendering an empty box.
 */
@Serializable(with = NetworkSource.Serializer::class)
enum class NetworkSource(val wire: String) {
    /**
     * NetworkManager over D-Bus: everything below is available.
     */
    NETWORK_MANAGER("network_manager"),
    /**
     * The kernel's interface list. Addresses are real; SSID, gateway, DNS and
     * connectivity are not knowable this way and come back empty.
     */
    INTERFACES("interfaces"),
    /**
     * Neither worked.
     */
    UNAVAILABLE("unavailable"),
    /**
     * A [NetworkSource] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<NetworkSource> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("NetworkSource", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: NetworkSource) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): NetworkSource {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * The whole read-out.
 */
@Serializable
data class NetworkStatusView(
    val connectivity: Connectivity,
    val interfaces: List<NetworkInterfaceView>,
    val managementApi: WebListenerView,
    /**
     * URLs that should open the web management interface, most useful first.
     *
     * Derived here rather than in each UI so the phone and the browser agree,
     * and because the derivation is not obvious: a listener bound to
     * `0.0.0.0` has no address of its own, so the answer is one URL per
     * reachable address on the box — which is the whole point of the ticket.
     */
    val managementUrls: List<String> = emptyList(),
    val source: NetworkSource,
    /**
     * Whether [`MAX_NETWORK_INTERFACES`] hid anything. UIs must say so.
     */
    val truncated: Boolean = false,
)

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
@Serializable(with = RetroarchSaveState.Serializer::class)
enum class RetroarchSaveState(val wire: String) {
    /**
     * Write a save state when the activity closes and load it on the next
     * open, so the child resumes exactly where they stopped — mid-battle,
     * mid-cutscene, wherever the session ended.
     *
     * Note this makes the console's own power-on screen unreachable, which is
     * what the HUD's reset button is for.
     */
    AUTO("auto"),
    /**
     * Leave save states alone. Every launch boots the content from scratch;
     * only the in-game save carries over.
     */
    OFF("off"),
    /**
     * A [RetroarchSaveState] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<RetroarchSaveState> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("RetroarchSaveState", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: RetroarchSaveState) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): RetroarchSaveState {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
     * Whether the HUD should show page-turn buttons for this session. See
     * [`EntryKind::supports_page_turn`].
     */
    val canTurnPages: Boolean = false,
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
@Serializable(with = SessionState.Serializer::class)
enum class SessionState(val wire: String) {
    /**
     * Approved and spawning; the activity has not mapped a window yet.
     */
    LAUNCHING("launching"),
    /**
     * The activity is running normally.
     */
    RUNNING("running"),
    /**
     * Running, and at least one time warning has been issued.
     */
    WARNED("warned"),
    /**
     * Past its deadline and being wound down.
     */
    EXPIRING("expiring"),
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
    STOPPING("stopping"),
    /**
     * Settled and cleared; no activity is running.
     */
    ENDED("ended"),
    /**
     * A [SessionState] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<SessionState> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("SessionState", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: SessionState) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): SessionState {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * Stop mode for session termination
 */
@Serializable(with = StopMode.Serializer::class)
enum class StopMode(val wire: String) {
    /**
     * Try graceful termination first
     */
    GRACEFUL("graceful"),
    /**
     * Force immediate termination
     */
    FORCE("force"),
    /**
     * A [StopMode] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<StopMode> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("StopMode", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: StopMode) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): StopMode {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = WarningSeverity.Serializer::class)
enum class WarningSeverity(val wire: String) {
    INFO("info"),
    WARN("warn"),
    CRITICAL("critical"),
    /**
     * A [WarningSeverity] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<WarningSeverity> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("WarningSeverity", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: WarningSeverity) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): WarningSeverity {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
 * What a client may know about the device's authentication state *before* it
 * has authenticated. Deliberately thin — it says which door to knock on and
 * nothing else.
 */
@Serializable
data class WebAuthStatus(
    /**
     * Whether a paired companion exists to approve a login. False means the
     * password is the only way in, so the UI should not offer the other.
     */
    val companionAvailable: Boolean,
    /**
     * False on a device where nobody has set a password yet: the browser
     * should show the setup screen and ask for the code on the TV.
     */
    val configured: Boolean,
)

/**
 * Whether the web management interface is up, and where.
 *
 * The reason this is not simply the configured `bind`/`port`: the daemon
 * retries a bind that is not yet available (a ZeroTier interface still coming
 * up at login), and a bind that never succeeds only ever reached a log line.
 * From every UI, a device whose management API never came up looked exactly
 * like one that did.
 */
@Serializable(with = WebListenerState.Serializer::class)
enum class WebListenerState(val wire: String) {
    /**
     * `service.management_api` is absent or disabled. Nothing is wrong.
     */
    DISABLED("disabled"),
    /**
     * Configured, and still waiting for its address to exist.
     */
    BINDING("binding"),
    /**
     * Serving.
     */
    LISTENING("listening"),
    /**
     * Configured and not serving. Somebody should know.
     */
    FAILED("failed"),
    /**
     * A [WebListenerState] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<WebListenerState> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("WebListenerState", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: WebListenerState) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): WebListenerState {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
}

/**
 * Where the web management interface is listening, if at all.
 */
@Serializable
data class WebListenerView(
    /**
     * The socket address as configured, e.g. `0.0.0.0:8080`. `None` only when
     * [`WebListenerState::Disabled`].
     */
    val addr: String? = null,
    /**
     * Why it is not serving. Only set with [`WebListenerState::Failed`].
     */
    val error: String? = null,
    /**
     * The port on its own, for building a URL against some other address.
     */
    val port: Long? = null,
    val state: WebListenerState,
    /**
     * Whether the listener terminates TLS (issue #156).
     *
     * Decides the scheme in [`NetworkStatusView::management_urls`], which is
     * not cosmetic: a device serving HTTPS answers a plaintext request with a
     * connection reset, so an `http://` URL for it sends a parent to debug
     * their browser instead of opening their device.
     */
    val tls: Boolean = false,
)

/**
 * One live browser session, as an administrator sees it.
 *
 * Carries no credential: `id` is a public handle used to revoke the session,
 * not the token that authenticates it. The token itself is stored hashed and
 * is never readable back out of this module.
 */
@Serializable
data class WebSessionInfo(
    val createdAt: IsoTimestamp,
    /**
     * True for the session making the request, so the UI can label it and
     * warn before revoking it.
     */
    val current: Boolean,
    val expiresAt: IsoTimestamp,
    val id: String,
    /**
     * Human label derived from the User-Agent at login — "Chrome on Android",
     * not a hex string, because the person revoking sessions is choosing
     * between their own devices.
     */
    val label: String,
    val lastSeen: IsoTimestamp,
    /**
     * The address the session logged in from, for the same reason.
     */
    val peer: String,
)

/**
 * The wireless network an interface is associated with.
 */
@Serializable
data class WifiView(
    /**
     * Channel centre frequency in MHz. A UI can turn 5220 into "5 GHz", which
     * is the part a person debugging a weak signal actually wants.
     */
    val frequencyMhz: Long? = null,
    /**
     * Signal quality, 0–100, as the driver reports it.
     */
    val signalPercent: Long? = null,
    /**
     * The network's name.
     *
     * `None` when the interface is not associated — and also when the SSID is
     * not valid UTF-8, which is legal: 802.11 carries an SSID as up to 32
     * arbitrary octets, not a string. A name we cannot render is reported as
     * no name rather than as mojibake.
     */
    val ssid: String? = null,
)

/**
 * An action that can be performed on a window via the debug API.
 */
@Serializable(with = WindowAction.Serializer::class)
enum class WindowAction(val wire: String) {
    /**
     * Ask the window to close (sway `kill`).
     */
    CLOSE("close"),
    /**
     * Move the window to the scratchpad to hide it from view.
     */
    HIDE("hide"),
    /**
     * Pull the window out of the scratchpad so it is shown again.
     */
    SHOW("show"),
    /**
     * A [WindowAction] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<WindowAction> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("WindowAction", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: WindowAction) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): WindowAction {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
@Serializable(with = WindowOwner.Serializer::class)
enum class WindowOwner(val wire: String) {
    /**
     * Shepherd's own furniture: the launcher, the HUD, the pairing UI, the
     * mirror, and background processes it keeps warm (the preloaded Steam
     * client). Expected to outlive every session.
     */
    SHEPHERD("shepherd"),
    /**
     * A process shepherd is supervising for the current session — the
     * activity itself, something in its process group, a Steam game
     * launched on its behalf, or one of its input sidecars.
     */
    ACTIVITY("activity"),
    /**
     * An activity that outlived its own teardown. Its session is over and
     * the host is still working on killing it — the same condition that
     * writes an `ActivityEscaped` audit record.
     */
    ESCAPED("escaped"),
    /**
     * No process shepherd knows about. Either something started outside
     * shepherd entirely, or an activity that got away without the host ever
     * noticing — the case supervision cannot fix on its own, and the reason
     * this field exists.
     */
    UNOWNED("unowned"),
    /**
     * A [WindowOwner] this build doesn't know about.
     *
     * A newer device degrades to this one value instead of failing the
     * decode of everything around it. Never sent by a device.
     */
    UNKNOWN("__unknown");

    internal object Serializer : KSerializer<WindowOwner> {
        override val descriptor: SerialDescriptor =
            PrimitiveSerialDescriptor("WindowOwner", PrimitiveKind.STRING)
        override fun serialize(encoder: Encoder, value: WindowOwner) =
            encoder.encodeString(value.wire)
        override fun deserialize(decoder: Decoder): WindowOwner {
            val wire = decoder.decodeString()
            return entries.firstOrNull { it.wire == wire } ?: UNKNOWN
        }
    }
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
