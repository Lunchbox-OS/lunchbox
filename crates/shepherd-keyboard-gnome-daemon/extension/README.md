# Shepherd Swipe Keyboard — GNOME Shell extension (Phase 4 scaffold)

The GNOME half of the swipe keyboard. GNOME exposes neither `input-method-v2`,
`virtual-keyboard`, nor `layer-shell`, so this is a GJS Shell extension paired with the Rust
[`shepherd-keyboard-gnome-daemon`](..): the extension renders the keyboard, captures touch,
reads input purpose + surrounding text, and commits text through **GNOME's own input-method
object**; it calls the daemon's `Decode` over D-Bus for candidates, so GNOME and wlroots
decode through the identical core path.

## Status: SCAFFOLD — not yet verified on a real GNOME Shell

Concrete and reviewable here: `metadata.json` (incl. `session-modes: ["user"]` — `unlock-dialog`
deliberately excluded, per GNOME review guidelines that disallow keyboard-signal connections
there), the D-Bus proxy + `Decode` wiring to the daemon, and the safety-gate decision (mirrors
`shepherd-keyboard-core::safety`).

**Not done** (marked `TODO(shell)` in `extension.js`) — needs a real GNOME Shell to write and
verify, because GJS/Shell APIs drift across releases:

1. Suppress GNOME's built-in OSK while active.
2. Render the keyboard actor (grid + suggestion bar) and show/hide it on focus/touch.
3. Capture touch on the actor and assemble a v1 `gesture.json` (same coordinate convention as
   the core's `GestureBuilder`).
4. Commit candidates/taps through GNOME's input-method object.
5. Read input purpose + content hint + surrounding text from the IM object and apply `gate()`.

## Open questions before this can ship (spec §9)

- **Target GNOME Shell version(s)** — pin and verify; update `metadata.json` `shell-version`.
- **Confirm GNOME reliably exposes input purpose + surrounding text + commit** on that version
  without an IBus engine. Per the spec this gates the password-safety guarantee: if purpose
  can't be reliably detected, that is a **release blocker**, not a silent degrade.

## Manual verification checklist (the Phase 4 gate)

On the target GNOME Shell version, with the daemon running (`shepherd-keyboard-gnome-daemon`):

- [ ] Extension enables with no errors in `journalctl --user -f /usr/bin/gnome-shell`.
- [ ] Built-in OSK is suppressed; only this keyboard appears.
- [ ] Keyboard appears on text-field focus / touch and hides on blur.
- [ ] A swipe commits the expected word into the focused field (matches the daemon's `Decode`
      output and the wlroots backend on the same gesture).
- [ ] Suggestion bar shows alternates; tapping one commits it.
- [ ] Space/backspace/enter/shift/symbols behave as on wlroots.
- [ ] A **password/PIN field is tap-only**: no swipe, no suggestions, and surrounding text is
      never read (verify via logs that `Decode` is not called and preceding text is `''`).
- [ ] In a child session there is no profile-switch UI.

## Install (development)

```sh
ln -s "$PWD/crates/shepherd-keyboard-gnome-daemon/extension" \
  ~/.local/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com
# restart GNOME Shell, then:
gnome-extensions enable shepherd-swipe@armeafamily.com
```
