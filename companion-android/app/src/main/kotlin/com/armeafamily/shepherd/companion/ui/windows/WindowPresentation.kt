package com.armeafamily.shepherd.companion.ui.windows

import com.armeafamily.shepherd.companion.domain.WindowInfo
import com.armeafamily.shepherd.companion.domain.WindowOwner

/**
 * How a [WindowInfo] reads on the windows screen.
 *
 * Split out of the composable so the naming rules — which are the whole
 * substance of that screen — are unit-testable without a device. Every
 * identifying field on the wire is optional, and the ones that are set
 * vary by toolkit: a Wayland app fills `app_id`, an XWayland one fills
 * `window_class`, and a window that has set no title at all (a splash
 * screen mid-startup, say) fills neither.
 */
object WindowPresentation {

    /**
     * The line a caregiver reads to decide what a row *is*.
     *
     * Falls back through every identifier the compositor might have
     * before naming the container id, so a row is never blank — an
     * unnamed window is exactly the kind this screen exists to kill.
     */
    fun title(w: WindowInfo): String =
        w.name?.takeIf { it.isNotBlank() }
            ?: w.appId?.takeIf { it.isNotBlank() }
            ?: w.windowClass?.takeIf { it.isNotBlank() }
            ?: "Window ${w.id}"

    /**
     * The technical line under the title: whichever of app_id/class the
     * compositor reported, the owning pid, and the container id used by
     * `act_on_window`.
     */
    fun subtitle(w: WindowInfo): String = buildList {
        val appId = w.appId?.takeIf { it.isNotBlank() }
        val cls = w.windowClass?.takeIf { it.isNotBlank() }
        when {
            appId != null -> add("app_id=$appId")
            cls != null -> add("class=$cls")
        }
        w.pid?.let { add("pid=$it") }
        add("id=${w.id}")
    }.joinToString(" · ")

    /** Where the window is: on the scratchpad, rendered, or neither. */
    fun placement(w: WindowInfo): Placement = when {
        w.inScratchpad -> Placement.SCRATCHPAD
        w.visible -> Placement.ON_SCREEN
        else -> Placement.HIDDEN
    }

    enum class Placement(val label: String) {
        /** Stashed on Sway's scratchpad — running, but off-screen. */
        SCRATCHPAD("Scratchpad"),

        /** Currently being rendered. */
        ON_SCREEN("On screen"),

        /** On a workspace that isn't showing (another one is focused). */
        HIDDEN("Hidden"),
    }

    /**
     * Whether nothing on the device is supervising this window.
     *
     * The two owners that mean it are different failures with the same
     * consequence: an activity that outlived its own teardown, and a surface
     * belonging to no session at all. Either way the child is looking at
     * something no time limit will end and no usage record will count, which
     * is the entire reason this screen can be reached from a phone.
     */
    fun isOrphan(w: WindowInfo): Boolean =
        w.owner == WindowOwner.ESCAPED || w.owner == WindowOwner.UNOWNED

    /** The chip naming who the device thinks is behind the window. */
    fun ownerLabel(w: WindowInfo): String = when (w.owner) {
        WindowOwner.SHEPHERD -> "Shepherd"
        WindowOwner.ACTIVITY -> "Activity"
        WindowOwner.ESCAPED -> "Escaped"
        WindowOwner.UNOWNED -> "Unowned"
        // A device newer than this build classified it as something we have no
        // word for. Say so plainly rather than guessing at a category: the row
        // is still worth showing, and a wrong label is worse than an honest
        // gap.
        WindowOwner.UNKNOWN -> "Unrecognised"
    }

    /**
     * What went wrong, for the owners where something did — and null for the
     * ones where nothing did, so an ordinary row stays a single line.
     */
    fun ownerDetail(w: WindowInfo): String? = when (w.owner) {
        WindowOwner.ESCAPED ->
            "This activity outlived its own teardown. Its session is over and " +
                "the device is still trying to close it."
        WindowOwner.UNOWNED ->
            "No process the device knows about. Either it was started outside " +
                "shepherd, or an activity got away without being noticed."
        // `UNKNOWN` gets no detail for the same reason the two ordinary owners
        // do not: this build cannot say anything true about a category it does
        // not have, and `isOrphan` already leaves it out of the orphan set.
        WindowOwner.SHEPHERD, WindowOwner.ACTIVITY, WindowOwner.UNKNOWN -> null
    }
}
