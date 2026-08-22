# Window management in the companion app (issue #140)

**Issue:** [#140 — Add window management to BLE management
app](https://git.armeafamily.com/albert/shepherd-launcher/issues/140)
(no body; the title is the whole brief).

## What was asked

The management API has carried `list_windows` / `act_on_window` since
the BLE work landed, and the web UI has rendered them as a "Sway
Windows" page for as long. The companion app — the *primary* admin
interface, and the only one that works without a network — could not
reach them. The spec anticipated this gap and left it open:

> Most apps can ignore this; useful for a hidden "developer" panel.
> — <docs/ai/history/2026-06-21 001 ble-companion-android-spec.md> §4.9

This closes it: the same three actions the web UI offers (close, hide to
the scratchpad, show from it), on the phone.

## What already existed

Nearly all of the plumbing, which is why the change is small:

* `ManagementClient.listWindows()` / `.actOnWindow(id, action)` — already
  written against the wire contract.
* `WindowInfo` / `WindowAction` in `WireTypes.generated.kt` — generated
  from the Rust trait, so no schema work.
* `RpcMethods.LIST_WINDOWS` / `.ACT_ON_WINDOW` — likewise.

Only the UI and its state were missing.

## What was added

* `ui/windows/WindowsScreen.kt` — the screen. Two sections ("On screen",
  "Scratchpad"), one card per window, Hide/Show + Close per card.
* `ui/windows/WindowPresentation.kt` — the naming rules, split out so
  they are testable without a device.
* `WindowsUiState` + `refreshWindows()` / `actOnWindow()` on
  `ShepherdViewModel`.
* Entry point: **Device controls → Windows…**, below "Log out device
  session".

### Decisions worth keeping

**Not part of `DeviceUiState`.** The window list is fetched only while
its screen is open. Putting it in the shared snapshot would spend an RPC
on every connect for a list nobody is looking at, on a link where a round
trip is measured in hundreds of milliseconds.

**Polled at 5 s, matching the web UI.** No event exists for a window
opening, closing, or being stashed — `EventPayload` has no variant for it
— so the screen polls while composed and stops when it isn't. Verified
live: a `foot` window spawned on the device appeared on the phone
unprompted within one interval.

**A failed refresh keeps the previous list.** The error renders above the
rows rather than replacing them. Blanking on a refresh that lands
mid-reconnect would pull the rows out from under a finger already
reaching for a button — and the ids under those buttons are what
`act_on_window` acts on.

**Close confirms; Hide and Show don't.** Close is the only one that
destroys anything (the child's unsaved work). Hide and Show are
trivially reversible from the same screen, and a dialog on each would
make the screen tedious for its actual use — poking at a window that has
gone wrong.

**The list is cleared when the active device changes.** Window ids are
per-compositor; carrying one box's ids over to another would act on
whatever container happens to hold that id there. Reconnects to the
*same* device keep the list, matching how the rest of the app treats a
dropped link.

**`actOnWindow(id, act)`** — the parameter is spelled `act` because
`action` is the name of the private RPC-error wrapper it delegates to,
which the parameter would otherwise shadow.

## Verification

Unit tests (`./gradlew :app:testDebugUnitTest`):

* `WindowPresentationTest` — the title fallback chain (name → app_id →
  class → `Window <id>`, blank titles included), the subtitle, and
  scratchpad-vs-merely-unrendered.
* `WireTest` — a two-window `list_windows` payload (Wayland and XWayland
  shapes) and the `close`/`hide`/`show` wire spellings.

End-to-end against the headless dev session with a Pixel on USB
(`companion-pairing` skill), which is the only thing that proves the RPCs
land:

| Action | Phone | Device (`shepherd dev tree`) |
|---|---|---|
| Open the screen | "On screen (1)", launcher card, "Focused" chip | — |
| Hide | row moves to "Scratchpad (1)", "Moved to the scratchpad." | `visible: false` |
| Show | row returns to "On screen" | `visible: true`, focused |
| `swaymsg exec foot` on the device | second card appears within 5 s | 2 windows |
| Close → confirm | card gone | 1 window |

### Gotcha hit on the way

The dev box picked the **Qualcomm** radio (`hci1`,
`DC:56:7B:1F:7D:EA`) — the one the `companion-pairing` skill documents as
individually broken — and the app sat in "Reconnecting…" with no connect
attempt in the daemon log. Unbinding it from `btusb` (per the skill's
"`default_adapter()` takes the lowest-indexed adapter" note) moved
shepherdd to the Realtek dongle and the link came up immediately. It is
left unbound; `echo -n 3-6:1.0 | sudo tee /sys/bus/usb/drivers/btusb/bind`
restores it, possibly under a different `hci` index.
