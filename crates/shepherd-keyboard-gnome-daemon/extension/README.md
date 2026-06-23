# Shepherd Swipe Keyboard — GNOME Shell extension (Phase 4 scaffold)

The GNOME half of the swipe keyboard. GNOME exposes neither `input-method-v2`,
`virtual-keyboard`, nor `layer-shell`, so this is a GJS Shell extension paired with the Rust
[`shepherd-keyboard-gnome-daemon`](..): the extension renders the keyboard, captures touch,
reads input purpose + surrounding text, and commits text through **GNOME's own input-method
object**; it calls the daemon's `Decode` over D-Bus for candidates, so GNOME and wlroots
decode through the identical core path.

## Status: SCAFFOLD — not yet verified on a real GNOME Shell

Targets **GNOME Shell 50** (the version shepherd-launcher's host runs;
`metadata.json` `shell-version: ["50"]`). Re-pin if the host moves.

Concrete and reviewable here: `metadata.json` (`session-modes: ["user", "gdm"]` — a login-screen
keyboard is wanted, so `gdm` is included; `unlock-dialog` stays excluded per GNOME review
guidelines that disallow keyboard-signal connections there), the D-Bus proxy + `Decode` wiring
to the daemon, and the safety-gate decision (mirrors `shepherd-keyboard-core::safety`).

**Not done** (marked `TODO(shell)` in `extension.js`) — needs a real GNOME Shell to write and
verify, because GJS/Shell APIs drift across releases:

1. Suppress GNOME's built-in OSK while active.
2. Render the keyboard actor (grid + suggestion bar) and show/hide it on focus/touch.
3. Capture touch on the actor and assemble a v1 `gesture.json` (same coordinate convention as
   the core's `GestureBuilder`).
4. Commit candidates/taps through GNOME's input-method object.
5. Read input purpose + content hint + surrounding text from the IM object and apply `gate()`.

## Login-screen (gdm) keyboard

A login-screen keyboard is in scope (`session-modes` includes `gdm`), which adds requirements
beyond the user-session keyboard:

- **The extension must be installed system-wide and enabled for gdm**, not just for the user.
  Install under `/usr/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com` and add the
  uuid to gdm's enabled list (e.g. a gdm dconf profile setting
  `org.gnome.shell enabled-extensions`), since `~/.local/share` isn't read in the gdm session.
- **A daemon instance must run on the gdm session bus.** The greeter runs as the `gdm` user with
  its own session bus, so a `shepherd-keyboard-gnome-daemon` must be started there (e.g. a
  systemd user unit in the gdm slice, or D-Bus activation). It loads a bundle readable by the
  `gdm` user from a root-owned location.
- **Profile at the login screen:** there is no logged-in user yet, so the greeter has no
  child/adult identity — run the gdm daemon with the **adult** profile (or a dedicated neutral
  one). The password field is still tap-only via the safety gate, so no password text is
  decoded or fed to the context LM regardless of profile.
- `gdm` mode imposes the same `unlock-dialog` review constraints; keep keyboard-signal handling
  out of any locked mode.

## Open questions before this can ship (spec §9)

- **Confirm GNOME 50 reliably exposes input purpose + content hint + surrounding text + commit**
  through the Shell input-method object without an IBus engine — in **both** `user` and `gdm`
  modes. Per the spec this gates the password-safety guarantee: if purpose can't be reliably
  detected, that is a **release blocker**, not a silent degrade. (Target version and the
  login-screen requirement are now settled: GNOME 50, gdm keyboard wanted.)

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
- [ ] **Login screen (gdm):** the keyboard appears at the GNOME greeter, the password field is
      tap-only, and the keyboard commits the username into the user field.

## Install (development)

User session:

```sh
ln -s "$PWD/crates/shepherd-keyboard-gnome-daemon/extension" \
  ~/.local/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com
# restart GNOME Shell (log out/in on Wayland), then:
gnome-extensions enable shepherd-swipe@armeafamily.com
```

Login screen (gdm) — system-wide, since `~/.local/share` isn't read in the gdm session:

```sh
sudo cp -r "$PWD/crates/shepherd-keyboard-gnome-daemon/extension" \
  /usr/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com
# enable for gdm via its dconf profile (org.gnome.shell enabled-extensions),
# and arrange a daemon instance on the gdm session bus (see "Login-screen keyboard").
```
