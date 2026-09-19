package com.lunchboxos.companion.ui.windows

import com.lunchboxos.companion.domain.WindowInfo
import com.lunchboxos.companion.domain.WindowOwner
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
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
        owner: WindowOwner = WindowOwner.ACTIVITY,
    ) = WindowInfo(
        appId = appId,
        focused = focused,
        id = id,
        inScratchpad = inScratchpad,
        name = name,
        owner = owner,
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

    @Test
    fun `only escaped and unowned windows count as orphans`() {
        // The two that mean nothing is supervising the window. Lunchbox's own
        // furniture and the running activity are not problems, and treating
        // them as such would put the launcher itself under a "close this"
        // banner on every visit.
        assertTrue(WindowPresentation.isOrphan(window(owner = WindowOwner.ESCAPED)))
        assertTrue(WindowPresentation.isOrphan(window(owner = WindowOwner.UNOWNED)))
        assertFalse(WindowPresentation.isOrphan(window(owner = WindowOwner.ACTIVITY)))
        assertFalse(WindowPresentation.isOrphan(window(owner = WindowOwner.LUNCHBOX)))
    }

    /// Everything a caregiver opens in administrator mode is unowned, so
    /// without this the phone would put a red "Unsupervised" banner over their
    /// own work for as long as they were setting the device up.
    @Test
    fun `nothing is an orphan while the device is in administrator mode`() {
        val escaped = window(owner = WindowOwner.ESCAPED)
        val unowned = window(owner = WindowOwner.UNOWNED)

        assertTrue(WindowPresentation.isOrphan(escaped, adminMode = false))
        assertTrue(WindowPresentation.isOrphan(unowned, adminMode = false))

        assertFalse(WindowPresentation.isOrphan(escaped, adminMode = true))
        assertFalse(WindowPresentation.isOrphan(unowned, adminMode = true))
    }

    @Test
    fun `focus is offered only for an unfocused window that is on screen`() {
        assertTrue(WindowPresentation.canFocus(window()))
        // Already focused: a round trip that changes nothing.
        assertFalse(WindowPresentation.canFocus(window(focused = true)))
        // On the scratchpad `focus` does not raise it; Show is that button.
        assertFalse(WindowPresentation.canFocus(window(inScratchpad = true)))
        assertFalse(WindowPresentation.canFocus(window(inScratchpad = true, focused = true)))
    }

    @Test
    fun `every owner has a chip label`() {
        assertEquals("Lunchbox", WindowPresentation.ownerLabel(window(owner = WindowOwner.LUNCHBOX)))
        assertEquals("Activity", WindowPresentation.ownerLabel(window(owner = WindowOwner.ACTIVITY)))
        assertEquals("Escaped", WindowPresentation.ownerLabel(window(owner = WindowOwner.ESCAPED)))
        assertEquals("Unowned", WindowPresentation.ownerLabel(window(owner = WindowOwner.UNOWNED)))
    }

    @Test
    fun `only the owners that are a problem explain themselves`() {
        // An explanation on every row would be four lines of noise for the
        // three-quarters of the list that is working as intended.
        assertNotNull(WindowPresentation.ownerDetail(window(owner = WindowOwner.ESCAPED)))
        assertNotNull(WindowPresentation.ownerDetail(window(owner = WindowOwner.UNOWNED)))
        assertNull(WindowPresentation.ownerDetail(window(owner = WindowOwner.ACTIVITY)))
        assertNull(WindowPresentation.ownerDetail(window(owner = WindowOwner.LUNCHBOX)))
    }
}
