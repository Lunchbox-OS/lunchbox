package com.armeafamily.shepherd.companion.ui.windows

import com.armeafamily.shepherd.companion.domain.WindowInfo

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
}
