package com.armeafamily.shepherd.companion.ui.windows

import com.armeafamily.shepherd.companion.domain.WindowInfo
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * The naming rules for window rows. Every identifying field the
 * compositor reports is optional and which ones are set depends on the
 * toolkit, so each fallback is exercised: a row that reads as blank is a
 * row nobody dares press "Close" on.
 */
class WindowPresentationTest {

    private fun window(
        id: Long = 10,
        name: String? = null,
        appId: String? = null,
        windowClass: String? = null,
        pid: Long? = null,
        inScratchpad: Boolean = false,
        visible: Boolean = true,
        focused: Boolean = false,
        workspace: String? = "1",
    ) = WindowInfo(
        appId = appId,
        focused = focused,
        id = id,
        inScratchpad = inScratchpad,
        name = name,
        pid = pid,
        visible = visible,
        windowClass = windowClass,
        workspace = workspace,
    )

    @Test
    fun `title prefers the window title`() {
        val w = window(name = "Celeste", appId = "steam_app_504230", windowClass = "steam_app_504230")
        assertEquals("Celeste", WindowPresentation.title(w))
    }

    @Test
    fun `title falls back through app_id then class then the container id`() {
        assertEquals("firefox", WindowPresentation.title(window(appId = "firefox")))
        assertEquals("Steam", WindowPresentation.title(window(windowClass = "Steam")))
        assertEquals("Window 42", WindowPresentation.title(window(id = 42)))
    }

    @Test
    fun `a blank title is treated as no title`() {
        // Sway reports "" for a window that set an empty title, which the
        // naive `?:` chain would happily render as an empty row.
        assertEquals("firefox", WindowPresentation.title(window(name = "  ", appId = "firefox")))
    }

    @Test
    fun `subtitle names one identifier, the pid, and the id act_on_window takes`() {
        val wayland = window(id = 7, appId = "firefox", windowClass = "Firefox", pid = 1234)
        assertEquals("app_id=firefox · pid=1234 · id=7", WindowPresentation.subtitle(wayland))

        val xwayland = window(id = 20, windowClass = "Steam", pid = 5678)
        assertEquals("class=Steam · pid=5678 · id=20", WindowPresentation.subtitle(xwayland))

        // The container id is the one field always present, so it alone
        // is a valid subtitle.
        assertEquals("id=10", WindowPresentation.subtitle(window()))
    }

    @Test
    fun `placement separates scratchpad from merely unrendered`() {
        assertEquals(
            WindowPresentation.Placement.SCRATCHPAD,
            WindowPresentation.placement(window(inScratchpad = true, visible = false)),
        )
        assertEquals(
            WindowPresentation.Placement.ON_SCREEN,
            WindowPresentation.placement(window(visible = true)),
        )
        // On a workspace that isn't showing: still ordinary, still
        // hideable — not the same thing as stashed.
        assertEquals(
            WindowPresentation.Placement.HIDDEN,
            WindowPresentation.placement(window(visible = false, workspace = "2")),
        )
    }
}
